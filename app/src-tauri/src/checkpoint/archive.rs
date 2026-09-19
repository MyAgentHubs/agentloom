use super::*;

pub(super) fn create_private_archive_dirs(root: &Path, target: &Path) -> Result<(), String> {
    if !target.starts_with(root) {
        return Err("checkpoint archive directory escaped its root".into());
    }
    fs::create_dir_all(target).map_err(|error| error.to_string())?;
    let mut current = Some(target);
    while let Some(dir) = current {
        set_private_dir_permissions(dir).map_err(|error| error.to_string())?;
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(super) fn set_private_blob_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub(super) fn set_private_blob_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub(super) fn remove_archive_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("checkpoint archive path is a symlink; refusing to follow it".into())
        }
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(path).map_err(|e| e.to_string())
        }
        Ok(_) => Err("checkpoint archive path is not a directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
