use std::path::{Path, PathBuf};

use super::*;

/// Reads `CFBundleShortVersionString` from
/// `<app_path>/Contents/Info.plist`. Returns `None` rather than a hard error if
/// the file is missing, the XML is invalid, or the key is absent. This is
/// reused for version comparison by `stage_bytes` and as the caller's default
/// version reader for `plan_recovery`.
pub fn read_bundle_version(app_path: &Path) -> Option<String> {
    let plist_path = app_path.join("Contents").join("Info.plist");
    let value = plist::Value::from_file(&plist_path).ok()?;
    value
        .as_dictionary()?
        .get("CFBundleShortVersionString")?
        .as_string()
        .map(|s| s.to_string())
}

/// The marker, the target path's actual version, and the current process
/// version jointly prove that the swap is complete and only reopening remains.
/// This pure predicate is used by both the check and download entry points, so
/// the running old process does not download the same version again and a new
/// process is not mistakenly considered pending reopen during the healthy
/// cleanup window.
pub fn swapped_awaiting_reopen(
    marker: Option<&TxnMarker>,
    bundle_version: Option<&str>,
    running_version: &str,
) -> bool {
    marker.is_some_and(|marker| {
        marker.stage == Stage::Swapped
            && bundle_version == Some(marker.target_version.as_str())
            && running_version != marker.target_version
    })
}

/// Reconstructs and verifies the canonical bundle's parent-directory anchor
/// from both paths in the marker. The staged bundle must live at
/// `<bundle_parent>/.agentloom-update-*/<AppName>.app`, so the staging layer's
/// parent directory is an ownership record independent of `bundle_path`.
/// Reopen and swap must share this constraint so neither path trusts only the
/// independently tamperable `bundle_path` in the marker.
pub(super) fn marker_parent_anchor(marker: &TxnMarker) -> Result<(&Path, &Path), String> {
    let bundle_parent = marker
        .bundle_path
        .parent()
        .ok_or_else(|| "bundle path has no parent".to_string())?;
    let staging_layer = marker
        .staged_path
        .parent()
        .ok_or_else(|| "staged path has no parent (staging layer)".to_string())?;
    let recorded_parent = staging_layer
        .parent()
        .ok_or_else(|| "staging layer has no parent".to_string())?;
    if bundle_parent != recorded_parent {
        return Err("bundle path is not inside the parent recorded by the staged path".to_string());
    }
    Ok((recorded_parent, staging_layer))
}

/// Revalidates the path and version before `updater_reopen` invokes
/// LaunchServices. It verifies only the bundle already swapped into the
/// canonical location and does not touch staging; this command never swaps or
/// deletes directories.
pub fn validate_reopen_bundle(marker: &TxnMarker) -> Result<PathBuf, InstallError> {
    let (recorded_parent, _) = marker_parent_anchor(marker).map_err(InstallError::PathEscape)?;
    if is_symlink(&marker.bundle_path) {
        return Err(InstallError::PathEscape(
            "bundle path is itself a symlink".to_string(),
        ));
    }
    let real_bundle = realpath(&marker.bundle_path)?;
    if real_bundle != marker.bundle_path {
        return Err(InstallError::PathEscape(
            "bundle path in marker is not its own realpath".to_string(),
        ));
    }
    let real_recorded_parent = realpath(recorded_parent)?;
    if real_bundle.parent() != Some(real_recorded_parent.as_path()) {
        return Err(InstallError::PathEscape(
            "bundle realpath is outside the parent recorded by the staged path".to_string(),
        ));
    }
    let found = read_bundle_version(&real_bundle);
    if found.as_deref() != Some(marker.target_version.as_str()) {
        return Err(InstallError::VersionMismatch {
            expected: marker.target_version.clone(),
            found,
        });
    }
    Ok(real_bundle)
}
