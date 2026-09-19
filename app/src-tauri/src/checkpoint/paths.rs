use super::*;

pub(super) const OUTSIDE_ALLOWED_ROOT_ERROR: &str =
    "checkpoint target is outside the allowed project root";

pub(super) enum RecordingPathError {
    OutsideRoot,
    Rejected(String),
}

impl From<String> for RecordingPathError {
    fn from(error: String) -> Self {
        Self::Rejected(error)
    }
}

impl From<&str> for RecordingPathError {
    fn from(error: &str) -> Self {
        Self::Rejected(error.to_string())
    }
}

#[cfg(unix)]
pub(super) fn open_current_parent(
    entry: &CheckpointEntry,
) -> Result<Option<(fs::File, std::ffi::CString)>, String> {
    let allowed_root = entry
        .allowed_root
        .as_deref()
        .ok_or_else(|| "checkpoint entry has no allowed project root".to_string())?;
    let relative = entry
        .file_path
        .strip_prefix(allowed_root)
        .map_err(|_| "checkpoint entry escaped its allowed project root".to_string())?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("checkpoint entry has an unsafe relative path".into());
    }
    match open_checkpoint_parent_at(allowed_root, relative, false) {
        Ok(parent) => Ok(Some(parent)),
        Err(OpenCheckpointParentError::MissingAncestor) => Ok(None),
        Err(OpenCheckpointParentError::Other(reason)) => Err(reason),
    }
}

#[cfg(unix)]
pub(super) fn checkpoint_c_string(value: &OsStr) -> Result<std::ffi::CString, String> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(value.as_bytes())
        .map_err(|_| "checkpoint path contains an interior NUL".to_string())
}

#[cfg(unix)]
pub(super) enum OpenCheckpointParentError {
    MissingAncestor,
    Other(String),
}

#[cfg(unix)]
fn open_checkpoint_dir_at(
    parent_fd: i32,
    name: &OsStr,
    create: bool,
) -> Result<fs::File, OpenCheckpointParentError> {
    use std::os::fd::FromRawFd;
    let name = checkpoint_c_string(name).map_err(OpenCheckpointParentError::Other)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let mut fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
    let mut open_error = (fd < 0).then(std::io::Error::last_os_error);
    if create && open_error.as_ref().and_then(std::io::Error::raw_os_error) == Some(libc::ENOENT) {
        let created = unsafe { libc::mkdirat(parent_fd, name.as_ptr(), 0o755) };
        if created < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(OpenCheckpointParentError::Other(error.to_string()));
            }
        }
        fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
        open_error = (fd < 0).then(std::io::Error::last_os_error);
    }
    if let Some(error) = open_error {
        if !create && error.raw_os_error() == Some(libc::ENOENT) {
            return Err(OpenCheckpointParentError::MissingAncestor);
        }
        return Err(OpenCheckpointParentError::Other(format!(
            "cannot open checkpoint parent without following symlinks: {}",
            error
        )));
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
pub(super) fn open_checkpoint_parent_at(
    allowed_root: &Path,
    relative: &Path,
    create: bool,
) -> Result<(fs::File, std::ffi::CString), OpenCheckpointParentError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let root_name =
        checkpoint_c_string(allowed_root.as_os_str()).map_err(OpenCheckpointParentError::Other)?;
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(OpenCheckpointParentError::Other(format!(
            "cannot open allowed project root without following symlinks: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut parent = unsafe { fs::File::from_raw_fd(root_fd) };
    let mut components = relative.components().peekable();
    let leaf = loop {
        let component = components.next().ok_or_else(|| {
            OpenCheckpointParentError::Other("checkpoint path has no leaf".to_string())
        })?;
        let Component::Normal(name) = component else {
            return Err(OpenCheckpointParentError::Other(
                "checkpoint path has an unsafe component".into(),
            ));
        };
        if components.peek().is_none() {
            break name.to_os_string();
        }
        parent = open_checkpoint_dir_at(parent.as_raw_fd(), name, create)?;
    };
    Ok((
        parent,
        checkpoint_c_string(&leaf).map_err(OpenCheckpointParentError::Other)?,
    ))
}

pub(super) fn canonical_file_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let leaf = absolute
        .file_name()
        .ok_or_else(|| "checkpoint path must name a file".to_string())?
        .to_os_string();
    let parent = absolute
        .parent()
        .ok_or_else(|| "checkpoint path has no parent".to_string())?;
    Ok(canonicalize_allow_missing(parent)?.join(leaf))
}

/// Validates a checkpoint target against the canonical project root.
///
/// `Path::components()` normalizes intermediate `.` components, so spellings such as
/// `<root>/./sub/file` are accepted. This is safe because canonicalization plus `strip_prefix`
/// below is the authoritative root boundary. A `..` component is retained as
/// `Component::ParentDir` and rejected.
pub(super) fn validate_recording_path(
    allowed_root: &Path,
    file_path: &Path,
) -> Result<(PathBuf, PathBuf), RecordingPathError> {
    if !file_path.is_absolute()
        || file_path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err("checkpoint target must be an absolute path without dot components".into());
    }
    if file_path
        .components()
        .any(|component| component.as_os_str() == OsStr::new(".git"))
    {
        return Err("checkpoint refuses to record paths inside .git".into());
    }
    let input_within_allowed_root = file_path.starts_with(allowed_root);
    let allowed_root = fs::canonicalize(allowed_root).map_err(|error| {
        format!(
            "cannot canonicalize checkpoint allowed root {}: {error}",
            allowed_root.display()
        )
    })?;
    let input_within_allowed_root =
        input_within_allowed_root || file_path.starts_with(&allowed_root);
    if !fs::metadata(&allowed_root)
        .map_err(|error| error.to_string())?
        .is_dir()
    {
        return Err("checkpoint allowed root is not a directory".into());
    }
    let file_path = canonical_file_path(file_path)?;
    let relative = match file_path.strip_prefix(&allowed_root) {
        Ok(relative) => relative,
        // A path spelled beneath the root that resolves outside it escaped through a symlinked
        // ancestor. That remains a rejection rather than the benign outside-root case.
        Err(_) if input_within_allowed_root => {
            return Err(RecordingPathError::Rejected(
                OUTSIDE_ALLOWED_ROOT_ERROR.to_string(),
            ));
        }
        Err(_) => return Err(RecordingPathError::OutsideRoot),
    };
    if relative.as_os_str().is_empty() {
        return Err("checkpoint target must name a file inside the project root".into());
    }
    if relative.components().any(|component| {
        !matches!(component, Component::Normal(_)) || component.as_os_str() == OsStr::new(".git")
    }) {
        return Err("checkpoint target has an unsafe project-relative path".into());
    }
    match fs::symlink_metadata(&file_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let resolved = fs::canonicalize(&file_path)
                .map_err(|error| format!("cannot validate checkpoint symlink target: {error}"))?;
            if !resolved.starts_with(&allowed_root) {
                return Err("checkpoint symlink target is outside the allowed project root".into());
            }
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string().into()),
    }
    Ok((allowed_root, file_path))
}

pub(super) fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf, String> {
    let mut cursor = path.to_path_buf();
    let mut missing = Vec::<OsString>::new();
    loop {
        match fs::canonicalize(&cursor) {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor
                    .file_name()
                    .ok_or_else(|| format!("cannot canonicalize {}", path.display()))?;
                missing.push(name.to_os_string());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| format!("cannot canonicalize {}", path.display()))?
                    .to_path_buf();
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

pub(super) fn validate_id<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if component == OsStr::new(value) => Ok(value),
        _ => Err(format!("invalid {label}")),
    }
}

pub(super) fn path_to_db_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "checkpoint path is not valid UTF-8".to_string())
}

#[cfg(unix)]
pub(super) fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
pub(super) fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
pub(super) fn bytes_os_string(value: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(value)
}

#[cfg(not(unix))]
pub(super) fn bytes_os_string(value: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&value).into_owned())
}
