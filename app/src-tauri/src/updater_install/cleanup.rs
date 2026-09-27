use std::fs;
use std::path::Path;

use super::*;

// ---------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------

/// Revalidates before deletion: the realpath of `staged` must be inside a
/// non-symlink staging directory with the expected name prefix, and that
/// staging directory must live directly under `parent` on the same volume.
/// The operation removes the entire staging directory created by `mkdtemp`,
/// where `staged` is only its `<AppName>.app`, rather than removing only
/// `staged`. This uses the same staging-layer rules as `revalidate_pair`.
pub fn cleanup_staged(parent: &Path, staged: &Path) -> Result<(), InstallError> {
    if is_symlink(staged) {
        return Err(InstallError::PathEscape(
            "staged path is a symlink".to_string(),
        ));
    }

    let staging_layer = staged.parent().ok_or_else(|| {
        InstallError::PathEscape("staged path has no parent (staging layer)".to_string())
    })?;
    let staging_layer_name = staging_layer
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| InstallError::PathEscape("staging layer has no valid name".to_string()))?;
    if !staging_layer_name.starts_with(STAGING_DIR_PREFIX) {
        return Err(InstallError::PathEscape(format!(
            "staged path's parent `{staging_layer_name}` is not a recognised staging directory"
        )));
    }
    if is_symlink(staging_layer) {
        return Err(InstallError::PathEscape(
            "staging layer directory is itself a symlink".to_string(),
        ));
    }

    let real_parent = realpath(parent)?;
    let staging_parent = staging_layer.parent().ok_or_else(|| {
        InstallError::PathEscape("staging layer has no parent directory".to_string())
    })?;
    let real_staging_parent = realpath(staging_parent)?;
    if real_staging_parent != real_parent {
        return Err(InstallError::PathEscape(
            "staging layer is not directly inside parent directory".to_string(),
        ));
    }

    let real_staging_layer = match fs::symlink_metadata(staging_layer) {
        Ok(_) => realpath(staging_layer)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(InstallError::Io(format!(
                "stat {}: {e}",
                staging_layer.display()
            )))
        }
    };

    if real_staging_layer.parent() != Some(real_parent.as_path()) {
        return Err(InstallError::PathEscape(
            "staging layer is not directly inside parent directory".to_string(),
        ));
    }
    if real_staging_layer != real_staging_parent.join(staging_layer_name) {
        return Err(InstallError::PathEscape(
            "staging layer path is not its own realpath".to_string(),
        ));
    }
    if !same_device(&real_parent, &real_staging_layer)? {
        return Err(InstallError::PathEscape(
            "staging layer is on a different device than parent".to_string(),
        ));
    }

    match fs::symlink_metadata(staged) {
        Ok(_) => {
            let real_staged = realpath(staged)?;
            let staged_name = staged.file_name().ok_or_else(|| {
                InstallError::PathEscape("staged path has no file name".to_string())
            })?;
            if real_staged != real_staging_layer.join(staged_name)
                || real_staged.parent() != Some(real_staging_layer.as_path())
            {
                return Err(InstallError::PathEscape(
                    "staged path is not its own realpath inside the staging layer".to_string(),
                ));
            }
            if !same_device(&real_parent, &real_staged)? {
                return Err(InstallError::PathEscape(
                    "staged path is on a different device than parent".to_string(),
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(InstallError::Io(format!("stat {}: {e}", staged.display()))),
    }

    fs::remove_dir_all(&real_staging_layer).map_err(|e| InstallError::Io(e.to_string()))
}
