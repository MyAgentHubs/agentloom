use super::*;

static RESTORE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn restore_entry(run_dir: &Path, entry: &CheckpointEntry) -> Result<(), String> {
    restore_entry_if_unchanged(run_dir, entry, None).map(|_| ())
}

pub(super) fn restore_entry_if_unchanged(
    run_dir: &Path,
    entry: &CheckpointEntry,
    expected_digest: Option<&str>,
) -> Result<bool, String> {
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

    let blob_path = if entry.existed {
        let blob_name = entry
            .blob_sha
            .as_deref()
            .ok_or_else(|| "recorded preimage has no content blob".to_string())?;
        validate_id(blob_name, "blob name")?;
        Some(run_dir.join("blobs").join(blob_name))
    } else {
        None
    };
    let pre_xattrs: Vec<StoredXattr> = entry
        .pre_xattrs
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|error| format!("invalid recorded preimage xattrs: {error}"))?
        .unwrap_or_default();

    atomic_restore(
        allowed_root,
        relative,
        &entry.file_path,
        blob_path.as_deref(),
        entry.is_symlink,
        entry.file_mode,
        &pre_xattrs,
        expected_digest,
    )
}

#[cfg(unix)]
fn atomic_restore(
    allowed_root: &Path,
    relative: &Path,
    _absolute_path: &Path,
    blob_path: Option<&Path>,
    is_symlink: bool,
    mode: Option<u32>,
    pre_xattrs: &[StoredXattr],
    expected_digest: Option<&str>,
) -> Result<bool, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;

    let (parent, leaf_c) =
        open_checkpoint_parent_at(allowed_root, relative, true).map_err(|error| match error {
            OpenCheckpointParentError::MissingAncestor => {
                "checkpoint parent disappeared while restoring".to_string()
            }
            OpenCheckpointParentError::Other(reason) => reason,
        })?;

    let Some(blob_path) = blob_path else {
        if let Some(expected) = expected_digest {
            let current = read_content_state_at(parent.as_raw_fd(), &leaf_c)?;
            if content_state_digest(&current)? != expected {
                return Ok(false);
            }
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                leaf_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound {
                Ok(true)
            } else {
                Err(error.to_string())
            };
        }
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            return Err("target became a directory; refusing to remove it".into());
        }
        if unsafe { libc::unlinkat(parent.as_raw_fd(), leaf_c.as_ptr(), 0) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        parent
            .sync_all()
            .map_err(|error| format!("file removed but parent directory fsync failed: {error}"))?;
        return Ok(true);
    };

    let sequence = RESTORE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_name = OsString::from(format!(".agentloom-undo-{}-{sequence}", std::process::id()));
    let temp_c = checkpoint_c_string(&temp_name)?;
    let cleanup_temp = || unsafe {
        libc::unlinkat(parent.as_raw_fd(), temp_c.as_ptr(), 0);
    };

    if is_symlink {
        let target = fs::read(blob_path).map_err(|error| error.to_string())?;
        let target = checkpoint_c_string(&bytes_os_string(target))?;
        if unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), temp_c.as_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if let Err(error) = set_symlink_xattrs_at(parent.as_raw_fd(), &temp_c, pre_xattrs) {
            cleanup_temp();
            return Err(error);
        }
    } else {
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                temp_c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut temp = unsafe { fs::File::from_raw_fd(fd) };
        let mut blob = fs::File::open(blob_path).map_err(|error| {
            cleanup_temp();
            error.to_string()
        })?;
        if let Err(error) = std::io::copy(&mut blob, &mut temp) {
            cleanup_temp();
            return Err(error.to_string());
        }
        if let Some(mode) = mode {
            if let Err(error) = temp.set_permissions(fs::Permissions::from_mode(mode)) {
                cleanup_temp();
                return Err(error.to_string());
            }
        }
        if let Err(error) = set_xattrs_fd(&temp, pre_xattrs) {
            cleanup_temp();
            return Err(error);
        }
        if let Err(error) = temp.sync_all() {
            cleanup_temp();
            return Err(error.to_string());
        }
    }

    if let Some(expected) = expected_digest {
        let current = match read_content_state_at(parent.as_raw_fd(), &leaf_c) {
            Ok(current) => current,
            Err(error) => {
                cleanup_temp();
                return Err(error);
            }
        };
        if content_state_digest(&current)? != expected {
            cleanup_temp();
            return Ok(false);
        }
    }
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            temp_c.as_ptr(),
            parent.as_raw_fd(),
            leaf_c.as_ptr(),
        )
    } < 0
    {
        let error = std::io::Error::last_os_error();
        cleanup_temp();
        return Err(error.to_string());
    }
    parent
        .sync_all()
        .map_err(|error| format!("file restored but parent directory fsync failed: {error}"))?;
    Ok(true)
}

#[cfg(not(unix))]
fn atomic_restore(
    allowed_root: &Path,
    relative: &Path,
    absolute_path: &Path,
    blob_path: Option<&Path>,
    is_symlink: bool,
    mode: Option<u32>,
    pre_xattrs: &[StoredXattr],
    expected_digest: Option<&str>,
) -> Result<bool, String> {
    let path = allowed_root.join(relative);
    let parent = path
        .parent()
        .ok_or_else(|| "checkpoint path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| error.to_string())?;
    if !canonical_parent.starts_with(allowed_root) {
        return Err("checkpoint parent escaped the allowed project root".into());
    }
    let Some(blob_path) = blob_path else {
        if let Some(expected) = expected_digest {
            if content_state_digest(&read_content_state(absolute_path)?)? != expected {
                return Ok(false);
            }
        }
        return match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                Err("target became a directory; refusing to remove it".into())
            }
            Ok(_) => {
                fs::remove_file(path).map_err(|error| error.to_string())?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error.to_string()),
        };
    };
    let temp = parent.join(format!(
        ".agentloom-undo-{}-{}",
        std::process::id(),
        RESTORE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    if is_symlink {
        if !pre_xattrs.is_empty() {
            return Err(
                "restoring xattrs on symlink preimages is not supported on this platform".into(),
            );
        }
        create_symlink(
            bytes_os_string(fs::read(blob_path).map_err(|e| e.to_string())?),
            &temp,
        )?;
    } else {
        let mut source = fs::File::open(blob_path).map_err(|error| error.to_string())?;
        let mut destination = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| error.to_string())?;
        std::io::copy(&mut source, &mut destination).map_err(|error| error.to_string())?;
        destination.sync_all().map_err(|error| error.to_string())?;
        set_xattrs_fd(&destination, pre_xattrs)?;
        restore_permission_mode(&temp, mode)?;
    }
    if let Some(expected) = expected_digest {
        if content_state_digest(&read_content_state(absolute_path)?)? != expected {
            let _ = fs::remove_file(&temp);
            return Ok(false);
        }
    }
    fs::rename(&temp, &path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        error.to_string()
    })?;
    Ok(true)
}

#[cfg(unix)]
fn restore_permission_mode(path: &Path, mode: Option<u32>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restore_permission_mode(_path: &Path, _mode: Option<u32>) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: OsString, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn create_symlink(target: OsString, link: &Path) -> Result<(), String> {
    std::os::windows::fs::symlink_file(target, link).map_err(|e| e.to_string())
}
