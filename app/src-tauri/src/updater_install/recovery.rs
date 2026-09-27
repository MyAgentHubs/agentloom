use std::path::{Path, PathBuf};

use super::*;

// ---------------------------------------------------------------------
// Startup recovery planning
// ---------------------------------------------------------------------

/// Where the current process is actually running relative to the two paths in
/// the marker. This considers only the running location and actual versions on
/// both sides, not `marker.stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunningAt {
    Bundle,
    Staged,
    Elsewhere,
}

/// Shared explanation used when the staging directory exists but its version
/// cannot be read. Production code and tests share this constant so tests do
/// not duplicate a string that can drift from production.
pub(super) const STAGED_VERSION_UNKNOWN_REASON: &str =
    "staged bundle exists but its Info.plist version could not be read";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryPlan {
    /// No marker exists, or the version matrix does not indicate any actionable
    /// case. This includes degraded states where neither side is the target but
    /// both versions are readable, or where both sides are already the target.
    None,
    /// Running successfully from the new `bundle_path`: delete the staged old version and marker.
    HealthyCleanup { staged_old: PathBuf },
    /// Running from `staged_path` after a manual rollback: prompt to swap back.
    RunningFromStaged { bundle_path: PathBuf },
    /// The staged path no longer exists: clear the orphaned marker.
    ClearStaleMarker,
    /// The two versions show that the exchange did not happen: treat as `Staged`.
    TreatAsStaged,
    /// The two versions show that the exchange happened: treat as `Swapped`.
    TreatAsSwapped,
    /// The staged path exists, but its version cannot be read because
    /// `Info.plist` is invalid, missing, or unparseable. This must not be treated
    /// as a missing path. The conservative response is to preserve the marker
    /// and return the decision to the caller for logging or a later retry.
    Unknown { reason: String },
}

/// Reconstructs the truth solely from `(staged_exists, bundle_version,
/// staged_version, running_at, target_version)` and never from `marker.stage`.
/// The stage is only a snapshot from the last marker write and can disagree
/// with the actual contents, such as when an exchange completed while the
/// marker remained at `Swapping`.
///
/// `path_exists` checks existence independently from `read_version`, usually
/// with `symlink_metadata(..).is_ok()`. An unreadable version must not stand in
/// for a missing path: the former may only mean a broken `Info.plist`, while the
/// latter means the staging directory is truly absent.
///
/// Decision order. Existence must be checked first: if `bundle` is already the
/// target version but `staged` is gone, the code must acknowledge the missing
/// staging anchor instead of entering the healthy branch and touching it.
///
/// The rules are: a missing staged path clears the orphaned marker first;
/// `running_at == Bundle` with `bundle_version == target` is healthy and cleans
/// up the old version; `running_at == Staged` offers a swap back;
/// `bundle_version != target` with `staged_version == target` means no exchange;
/// `bundle_version == target` with `staged_version != target` means exchanged.
/// Otherwise, an unreadable staged version yields `Unknown`, while readable
/// versions that both match or both differ yield `None`.
pub fn plan_recovery(
    marker: Option<&TxnMarker>,
    running_exe_bundle: &Path,
    path_exists: &dyn Fn(&Path) -> bool,
    read_version: &dyn Fn(&Path) -> Option<String>,
) -> RecoveryPlan {
    let marker = match marker {
        Some(m) => m,
        None => return RecoveryPlan::None,
    };

    // Check existence first and independently from version parsing. Using
    // `read_version(..).is_some()` would mistake an unreadable Info.plist for a
    // missing directory.
    if !path_exists(&marker.staged_path) {
        return RecoveryPlan::ClearStaleMarker;
    }

    let running_at = if running_exe_bundle == marker.bundle_path {
        RunningAt::Bundle
    } else if running_exe_bundle == marker.staged_path {
        RunningAt::Staged
    } else {
        RunningAt::Elsewhere
    };

    let bundle_version = read_version(&marker.bundle_path);
    let staged_version = read_version(&marker.staged_path);

    // Healthy canonical bundle.
    if running_at == RunningAt::Bundle
        && bundle_version.as_deref() == Some(marker.target_version.as_str())
    {
        return RecoveryPlan::HealthyCleanup {
            staged_old: marker.staged_path.clone(),
        };
    }

    // Consider only the actual running location, regardless of marker.stage.
    if running_at == RunningAt::Staged {
        return RecoveryPlan::RunningFromStaged {
            bundle_path: marker.bundle_path.clone(),
        };
    }

    let bundle_is_target = bundle_version.as_deref() == Some(marker.target_version.as_str());
    let staged_is_target = staged_version.as_deref() == Some(marker.target_version.as_str());

    match (bundle_is_target, staged_is_target) {
        // The exchange did not happen.
        (false, true) => RecoveryPlan::TreatAsStaged,
        // The exchange happened.
        (true, false) => RecoveryPlan::TreatAsSwapped,
        // Neither side is the target. If the staged version is unreadable rather
        // than merely different, return the decision to the caller.
        (false, false) if staged_version.is_none() => RecoveryPlan::Unknown {
            reason: STAGED_VERSION_UNKNOWN_REASON.to_string(),
        },
        // If neither side is the target but both versions are readable, or both
        // sides are already the target, there is no clear action signal.
        _ => RecoveryPlan::None,
    }
}
