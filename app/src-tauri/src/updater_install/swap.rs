use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::*;

// ---------------------------------------------------------------------
// Exchange with RENAME_SWAP and marker state transitions
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SwapDirection {
    /// `Staged -> Swapping -> Swapped`: normal installation.
    Forward,
    /// `Swapped -> Swapping -> Staged`: recovery path that swaps back to the old version.
    Backward,
}

/// Revalidates immediately before the exchange. **`staged_path` is not a direct
/// sibling of `bundle_path`**; it is
/// `<bundle_parent>/.agentloom-update-XXXXXX/<AppName>.app`, separated by the
/// staging directory that `stage_bytes` creates with `mkdtemp`. Validation
/// requires that staging directory's parent to equal the bundle's parent, and
/// that its name has the expected `mkdtemp` prefix and is not a symlink. It does
/// not require the two `.app` directories to live directly at the same level.
pub(super) fn revalidate_pair(marker: &TxnMarker) -> Result<(), InstallError> {
    let (_bundle_parent, staging_layer) =
        marker_parent_anchor(marker).map_err(|reason| InstallError::SwapFailed {
            reason,
            marker_restore_error: None,
        })?;
    let staging_layer_name = staging_layer
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| InstallError::SwapFailed {
            reason: "staging layer has no valid name".to_string(),
            marker_restore_error: None,
        })?;
    if !staging_layer_name.starts_with(STAGING_DIR_PREFIX) {
        return Err(InstallError::SwapFailed {
            reason: format!(
                "staged path's parent `{staging_layer_name}` is not a recognised staging directory"
            ),
            marker_restore_error: None,
        });
    }
    if is_symlink(staging_layer) {
        return Err(InstallError::SwapFailed {
            reason: "staging layer directory is itself a symlink".to_string(),
            marker_restore_error: None,
        });
    }

    if is_symlink(&marker.bundle_path) || is_symlink(&marker.staged_path) {
        return Err(InstallError::SwapFailed {
            reason: "bundle or staged path is itself a symlink".to_string(),
            marker_restore_error: None,
        });
    }

    let real_bundle = realpath(&marker.bundle_path).map_err(|_| InstallError::SwapFailed {
        reason: "bundle path does not exist or is unreadable".to_string(),
        marker_restore_error: None,
    })?;
    let real_staged = realpath(&marker.staged_path).map_err(|_| InstallError::SwapFailed {
        reason: "staged path does not exist or is unreadable".to_string(),
        marker_restore_error: None,
    })?;

    if real_bundle != marker.bundle_path {
        return Err(InstallError::SwapFailed {
            reason: "bundle path in marker is not its own realpath".to_string(),
            marker_restore_error: None,
        });
    }
    if real_staged != marker.staged_path {
        return Err(InstallError::SwapFailed {
            reason: "staged path in marker is not its own realpath".to_string(),
            marker_restore_error: None,
        });
    }

    let dev_bundle = fs::metadata(&real_bundle)
        .map_err(|e| InstallError::Io(format!("stat {}: {e}", real_bundle.display())))?
        .dev();
    let dev_staged = fs::metadata(&real_staged)
        .map_err(|e| InstallError::Io(format!("stat {}: {e}", real_staged.display())))?
        .dev();
    require_same_device(dev_bundle, dev_staged)?;

    Ok(())
}

pub(super) fn swap_impl_with_marker_writer(
    marker_dir: &Path,
    marker: &TxnMarker,
    direction: SwapDirection,
    fault: Option<Fault>,
    // Test-only seam selecting the directory for the marker write after the
    // exchange, whether the exchange succeeds or fails. Production always uses
    // `None`, which falls back to `marker_dir`; tests use it to fail only the
    // post-exchange marker write without affecting the pre-exchange write. This
    // covers `SwappedMarkerWriteFailed` and a failed restore after rename fails.
    post_rename_marker_dir_override: Option<&Path>,
    mut marker_writer: impl FnMut(&Path, &TxnMarker) -> Result<MarkerDurability, InstallError>,
) -> Result<SwapOutcome, InstallError> {
    if fault == Some(Fault::Swap) {
        return Err(InstallError::InjectedFault(Fault::Swap.step_name()));
    }

    revalidate_pair(marker)?;

    if direction == SwapDirection::Forward {
        let found = read_bundle_version(&marker.staged_path);
        if found.as_deref() != Some(marker.target_version.as_str()) {
            return Err(InstallError::VersionMismatch {
                expected: marker.target_version.clone(),
                found,
            });
        }
    }

    let mut swapping = marker.clone();
    swapping.stage = Stage::Swapping;
    match marker_writer(marker_dir, &swapping) {
        Ok(MarkerDurability::Durable) => {}
        Ok(MarkerDurability::CommittedNotDurable) => {
            return Err(InstallError::SwapFailed {
                reason: "marker_not_durable".to_string(),
                marker_restore_error: None,
            });
        }
        Err(e) => {
            return Err(InstallError::SwapFailed {
                reason: format!("failed to persist swapping marker before exchange: {e}"),
                marker_restore_error: None,
            });
        }
    }

    let post_rename_dir = post_rename_marker_dir_override.unwrap_or(marker_dir);

    let bundle_c = path_to_cstring(&marker.bundle_path)?;
    let staged_c = path_to_cstring(&marker.staged_path)?;
    // renameatx_np with RENAME_SWAP atomically exchanges two directories in one
    // system call. The target path always contains a launchable .app, so there
    // is no crash window in which the target is empty.
    let rc = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            bundle_c.as_ptr(),
            libc::AT_FDCWD,
            staged_c.as_ptr(),
            libc::RENAME_SWAP as libc::c_uint,
        )
    };

    if rc != 0 {
        let errno = std::io::Error::last_os_error();
        let mut reverted = marker.clone();
        reverted.stage = match direction {
            SwapDirection::Forward => Stage::Staged,
            SwapDirection::Backward => Stage::Swapped,
        };
        // The exchange itself failed, so neither side changed. If writing the
        // marker back to its original stage also fails, the caller must see it.
        let restore_result = marker_writer(post_rename_dir, &reverted);
        return Err(InstallError::SwapFailed {
            reason: format!("renameatx_np(RENAME_SWAP) failed: {errno}"),
            marker_restore_error: restore_result.err().map(|e| e.to_string()),
        });
    }

    if fault == Some(Fault::PostSwapPreMarker) {
        // Simulates the process being killed right after the swap succeeds but before the
        // marker is durably written as Swapped. This path cannot be asserted from a normal unit
        // test (the process is killed), so it is exercised only via real-machine drills; this call gives it a deterministic trigger point.
        std::process::abort();
    }

    let mut done = marker.clone();
    done.stage = match direction {
        SwapDirection::Forward => Stage::Swapped,
        SwapDirection::Backward => Stage::Staged,
    };
    match marker_writer(post_rename_dir, &done) {
        Ok(MarkerDurability::Durable) => Ok(SwapOutcome::Swapped),
        Ok(MarkerDurability::CommittedNotDurable) => Ok(SwapOutcome::Swapped),
        Err(e) => {
            // The physical exchange already succeeded. This is not a failed
            // exchange: the caller must treat it as swapped even though the
            // marker could not record the result accurately.
            Ok(SwapOutcome::SwappedMarkerWriteFailed {
                marker_write_error: e.to_string(),
            })
        }
    }
}

pub(super) fn swap_impl(
    marker_dir: &Path,
    marker: &TxnMarker,
    direction: SwapDirection,
    fault: Option<Fault>,
    post_rename_marker_dir_override: Option<&Path>,
) -> Result<SwapOutcome, InstallError> {
    swap_impl_with_marker_writer(
        marker_dir,
        marker,
        direction,
        fault,
        post_rename_marker_dir_override,
        write_marker,
    )
}

/// Normal installation: `Staged -> Swapping -> Swapped`. Failure never falls
/// back to two renames and never elevates privileges. On `Err`, the old
/// `bundle_path` remains unchanged and the staged bundle is preserved.
pub fn swap(marker_dir: &Path, marker: &TxnMarker) -> Result<SwapOutcome, InstallError> {
    swap_impl(
        marker_dir,
        marker,
        SwapDirection::Forward,
        injected_fault(),
        None,
    )
}

/// Recovery path for swapping back after the user manually opens the old
/// version in the staging directory. `Swapped -> Swapping -> Staged` reuses the
/// same `RENAME_SWAP`, which is its own inverse.
pub fn swap_back(marker_dir: &Path, marker: &TxnMarker) -> Result<SwapOutcome, InstallError> {
    swap_impl(
        marker_dir,
        marker,
        SwapDirection::Backward,
        injected_fault(),
        None,
    )
}

#[cfg(test)]
pub(super) fn swap_with_fault(
    marker_dir: &Path,
    marker: &TxnMarker,
    fault: Fault,
) -> Result<SwapOutcome, InstallError> {
    swap_impl(
        marker_dir,
        marker,
        SwapDirection::Forward,
        Some(fault),
        None,
    )
}
