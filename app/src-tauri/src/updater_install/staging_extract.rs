use std::fs;
use std::io::{Cursor, Read};
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{FromRawFd, RawFd};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;

use super::*;

// ---------------------------------------------------------------------
// Staging: mkdtemp + component-by-component no-follow dirfd extraction +
// version comparison + verify()
// ---------------------------------------------------------------------

/// Creates an unpredictable, exclusive `0700` staging directory under the
/// parent directory of `bundle_path`.
///
/// Uses `libc::mkdtemp` instead of promoting `tempfile` from a dev-dependency
/// to a regular dependency. `mkdtemp(3)` itself guarantees exclusive creation
/// of the directory with mode `0700` (which is explicitly reinforced here).
pub(super) fn make_staging_dir(parent: &Path) -> Result<PathBuf, InstallError> {
    let template_path = parent.join(format!("{STAGING_DIR_PREFIX}XXXXXX"));
    let mut template_bytes = template_path.as_os_str().as_bytes().to_vec();
    template_bytes.push(0);

    let ptr = template_bytes.as_mut_ptr() as *mut libc::c_char;
    let result = unsafe { libc::mkdtemp(ptr) };
    if result.is_null() {
        return Err(InstallError::Io(format!(
            "mkdtemp({}) failed: {}",
            template_path.display(),
            std::io::Error::last_os_error()
        )));
    }

    // mkdtemp replaces the trailing XXXXXX in the template in place with the
    // generated name. Slice the actual directory path back out of these bytes
    // (without the trailing NUL) to avoid borrowing it through a CStr lifetime.
    let nul_at = template_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(template_bytes.len());
    let dir = PathBuf::from(std::ffi::OsStr::from_bytes(&template_bytes[..nul_at]));

    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    Ok(dir)
}

pub(super) fn safe_components(rel_path: &Path) -> Result<Vec<String>, InstallError> {
    let mut out = Vec::new();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(part) => {
                let s = part.to_str().ok_or_else(|| {
                    InstallError::PathEscape(format!(
                        "non-UTF-8 entry path: {}",
                        rel_path.display()
                    ))
                })?;
                out.push(s.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(InstallError::PathEscape(format!(
                    "entry path contains `..`: {}",
                    rel_path.display()
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(InstallError::PathEscape(format!(
                    "absolute entry path: {}",
                    rel_path.display()
                )));
            }
        }
    }
    Ok(out)
}

/// Checks whether a symlink target, resolved relative to the entry's own
/// directory, remains within the `<staging>/<app_root>/` subtree. This is only
/// a **lexical/nominal** check. The defense that actually prevents a real
/// symlink chain from causing later entries to land outside the staging
/// directory is component-by-component dirfd traversal with `O_NOFOLLOW`
/// during extraction (see `openat_dir_component`). Both layers are required:
/// this lexical check alone can be bypassed by self-referential symlink chains
/// such as `a -> .` and `a/b -> .`.
pub(super) fn resolve_symlink_target(
    entry_dir: &[String],
    link_target: &Path,
    app_root: &str,
) -> Result<Vec<String>, InstallError> {
    if link_target.is_absolute() {
        return Err(InstallError::PathEscape(format!(
            "symlink target is absolute: {}",
            link_target.display()
        )));
    }

    let mut stack: Vec<String> = entry_dir.to_vec();
    for comp in link_target.components() {
        match comp {
            Component::Normal(part) => {
                let s = part.to_str().ok_or_else(|| {
                    InstallError::PathEscape("non-UTF-8 symlink target".to_string())
                })?;
                stack.push(s.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if stack.pop().is_none() {
                    return Err(InstallError::PathEscape(format!(
                        "symlink target escapes staging root: {}",
                        link_target.display()
                    )));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(InstallError::PathEscape(format!(
                    "symlink target is absolute: {}",
                    link_target.display()
                )));
            }
        }
    }

    if stack.first().map(String::as_str) != Some(app_root) {
        return Err(InstallError::PathEscape(format!(
            "symlink target escapes bundle: {}",
            link_target.display()
        )));
    }

    Ok(stack)
}

/// RAII fd wrapper that automatically calls `close` when the scope ends or an
/// error returns early, preventing file descriptor leaks when `?` exits during
/// component-by-component dirfd traversal.
struct OwnedFd(RawFd);

impl OwnedFd {
    fn raw(&self) -> RawFd {
        self.0
    }
}

impl Drop for OwnedFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe {
                libc::close(self.0);
            }
        }
    }
}

fn open_root_dir_no_follow(dir: &Path) -> Result<OwnedFd, InstallError> {
    let c_path = path_to_cstring(dir)?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(InstallError::ExtractionFailed(format!(
            "open staging root {}: {}",
            dir.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(OwnedFd(fd))
}

/// Opens or creates one intermediate directory component while **never
/// following symlinks**. If the component is already a symlink, it is always
/// rejected, regardless of its target, even if it points to a valid directory.
///
/// This is the key defense against chained attacks in which a symlink entry
/// first masquerades as a directory and later entries use its name to write to
/// the real target. `fs::create_dir_all`, `File::create`, and `symlink()` use
/// ordinary path resolution and follow symlinks in intermediate components;
/// `openat(..., O_NOFOLLOW)` instead returns `ELOOP` when the final component is
/// a symlink, forcing explicit handling here instead of traversing it.
pub(super) fn openat_dir_component(parent_fd: RawFd, name: &str) -> Result<RawFd, InstallError> {
    let c_name = str_to_cstring(name)?;
    let flags = libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    let fd = unsafe { libc::openat(parent_fd, c_name.as_ptr(), flags) };
    if fd >= 0 {
        return Ok(fd);
    }
    let open_err = std::io::Error::last_os_error();
    if open_err.raw_os_error() != Some(libc::ENOENT) {
        // Reject ELOOP (the component is a symlink), ENOTDIR (the component is
        // a regular file), and similar errors as path escapes.
        return Err(InstallError::PathEscape(format!(
            "refusing to traverse `{name}`: {open_err} (likely a symlink or non-directory)"
        )));
    }

    let mkdir_rc = unsafe { libc::mkdirat(parent_fd, c_name.as_ptr(), 0o755) };
    if mkdir_rc != 0 {
        return Err(InstallError::ExtractionFailed(format!(
            "mkdirat `{name}`: {}",
            std::io::Error::last_os_error()
        )));
    }

    let fd2 = unsafe { libc::openat(parent_fd, c_name.as_ptr(), flags) };
    if fd2 < 0 {
        return Err(InstallError::PathEscape(format!(
            "refusing to traverse `{name}` after creating it: {} (likely raced with a symlink)",
            std::io::Error::last_os_error()
        )));
    }
    Ok(fd2)
}

/// Traverses to the end of the directory chain represented by `components` and
/// returns an independent fd for that directory. If `components` is empty,
/// returns a `dup` of `root_fd`, so every fd given to a caller is independent
/// and can be closed safely without affecting `root_fd` itself.
fn open_dir_chain(root_fd: RawFd, components: &[String]) -> Result<OwnedFd, InstallError> {
    if components.is_empty() {
        let dup_fd = unsafe { libc::dup(root_fd) };
        if dup_fd < 0 {
            return Err(InstallError::Io(format!(
                "dup staging root fd: {}",
                std::io::Error::last_os_error()
            )));
        }
        return Ok(OwnedFd(dup_fd));
    }

    let mut current: RawFd = root_fd;
    let mut owned: Option<OwnedFd> = None;
    for name in components {
        let next = openat_dir_component(current, name)?;
        owned = Some(OwnedFd(next));
        current = next;
    }
    Ok(owned.expect("components non-empty implies at least one iteration"))
}

/// Creates a leaf directory (a tar `Directory` entry) under `parent_fd`. If the
/// name already exists, it is accepted as a harmless duplicate declaration
/// only when it is truly a plain, non-symlink directory. Otherwise it is
/// rejected as a path escape, preventing an attacker from reserving the name
/// with a symlink in advance.
fn mkdirat_leaf_directory(parent_fd: RawFd, name: &str) -> Result<(), InstallError> {
    let c_name = str_to_cstring(name)?;
    let rc = unsafe { libc::mkdirat(parent_fd, c_name.as_ptr(), 0o755) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EEXIST) {
        return Err(InstallError::ExtractionFailed(format!(
            "mkdirat `{name}`: {err}"
        )));
    }

    let mut st: libc::stat = unsafe { mem::zeroed() };
    let stat_rc = unsafe {
        libc::fstatat(
            parent_fd,
            c_name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if stat_rc != 0 || (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
        return Err(InstallError::PathEscape(format!(
            "`{name}` already exists and is not a plain directory"
        )));
    }
    Ok(())
}

/// Exclusively creates a new regular file under `parent_fd`
/// (`O_EXCL|O_NOFOLLOW`). If the name already exists, whether as a file,
/// directory, or symlink, creation always fails, so a pre-planted symlink can
/// never redirect the content elsewhere.
fn openat_new_regular_file(
    parent_fd: RawFd,
    name: &str,
    mode: u32,
) -> Result<OwnedFd, InstallError> {
    let c_name = str_to_cstring(name)?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::openat(parent_fd, c_name.as_ptr(), flags, mode as libc::c_uint) };
    if fd < 0 {
        return Err(InstallError::PathEscape(format!(
            "refusing to create `{name}`: {} (name already exists, or a symlink is in the way)",
            std::io::Error::last_os_error()
        )));
    }
    Ok(OwnedFd(fd))
}

/// `symlinkat` is likewise exclusive: it fails if the name already exists and
/// never overwrites or follows an existing entry.
fn symlinkat_new(parent_fd: RawFd, name: &str, target: &Path) -> Result<(), InstallError> {
    let c_name = str_to_cstring(name)?;
    let c_target = path_to_cstring(target)?;
    let rc = unsafe { libc::symlinkat(c_target.as_ptr(), parent_fd, c_name.as_ptr()) };
    if rc != 0 {
        return Err(InstallError::PathEscape(format!(
            "refusing to create symlink `{name}`: {} (name already exists)",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// Extracts a gzip tar into `staging_dir` without following symlinks and returns
/// the name of the single top-level `*.app` directory.
///
/// Two defense layers are used; the earlier single lexical layer could be
/// bypassed by real symlink chains:
/// 1. `safe_components`/`resolve_symlink_target` **lexically** verify that each
///    entry path and symlink target contains no `..`, is not absolute, and
///    nominally remains under the app root.
/// 2. This function traverses dirfds component by component with
///    `openat(..., O_NOFOLLOW)` to verify the **real filesystem**. Traversal is
///    rejected if any intermediate component is already a symlink, even one
///    created by this extraction, instead of transparently following it as
///    `fs::create_dir_all` or `File::create` would.
///    Data is written only after both layers pass.
pub(super) fn extract_tar_gz(bytes: &[u8], staging_dir: &Path) -> Result<String, InstallError> {
    let mut gz = GzDecoder::new(bytes);
    let mut tar_bytes = Vec::new();
    gz.read_to_end(&mut tar_bytes)
        .map_err(|e| InstallError::ExtractionFailed(format!("gzip decode failed: {e}")))?;

    let root_fd = open_root_dir_no_follow(staging_dir)?;

    let mut archive = tar::Archive::new(Cursor::new(&tar_bytes));
    let entries = archive
        .entries()
        .map_err(|e| InstallError::ExtractionFailed(format!("read tar entries: {e}")))?;

    let mut app_root: Option<String> = None;

    for entry_result in entries {
        let mut entry = entry_result
            .map_err(|e| InstallError::ExtractionFailed(format!("read tar entry: {e}")))?;
        let entry_type = entry.header().entry_type();

        let rel_path = entry
            .path()
            .map_err(|e| InstallError::PathEscape(format!("bad entry path: {e}")))?
            .into_owned();
        let components = safe_components(&rel_path)?;
        if components.is_empty() {
            // Skip placeholder top-level entries such as ".".
            continue;
        }

        let top = components[0].clone();
        match &app_root {
            None => {
                if !top.ends_with(".app") {
                    return Err(InstallError::ExtractionFailed(format!(
                        "archive top-level entry is not a .app bundle: {top}"
                    )));
                }
                app_root = Some(top);
            }
            Some(existing) if *existing != top => {
                return Err(InstallError::ExtractionFailed(format!(
                    "archive has multiple top-level entries: {existing} and {top}"
                )));
            }
            Some(_) => {}
        }

        let parent_components = &components[..components.len() - 1];
        let final_name = components.last().expect("components non-empty").as_str();
        let parent_dir = open_dir_chain(root_fd.raw(), parent_components)?;

        match entry_type {
            tar::EntryType::Directory => {
                mkdirat_leaf_directory(parent_dir.raw(), final_name)?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
                let file_fd = openat_new_regular_file(parent_dir.raw(), final_name, mode)?;
                // File takes ownership of this fd's lifetime (its Drop closes
                // it), so forget OwnedFd to prevent both values from trying to
                // close the same fd.
                let raw = file_fd.raw();
                mem::forget(file_fd);
                let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
                std::io::copy(&mut entry, &mut file)
                    .map_err(|e| InstallError::ExtractionFailed(e.to_string()))?;
            }
            tar::EntryType::Symlink => {
                let link_name = entry
                    .link_name()
                    .map_err(|e| InstallError::PathEscape(e.to_string()))?
                    .ok_or_else(|| {
                        InstallError::PathEscape("symlink entry missing target".to_string())
                    })?;
                let app_root_so_far = app_root.as_deref().expect("app_root set above");
                // Layer 1: lexically verify that the target does not escape the
                // app root.
                resolve_symlink_target(parent_components, &link_name, app_root_so_far)?;
                // Layer 2: create the real entry with symlinkat, so this entry
                // itself is not followed. If the next entry tries to traverse
                // this name as a directory, the dirfd layer rejects it.
                symlinkat_new(parent_dir.raw(), final_name, &link_name)?;
            }
            tar::EntryType::Link => {
                return Err(InstallError::PathEscape(
                    "hardlink entries are not allowed".to_string(),
                ));
            }
            other => {
                return Err(InstallError::PathEscape(format!(
                    "unsupported tar entry type: {other:?}"
                )));
            }
        }
    }

    app_root.ok_or_else(|| {
        InstallError::ExtractionFailed("archive is empty or has no .app bundle".to_string())
    })
}
