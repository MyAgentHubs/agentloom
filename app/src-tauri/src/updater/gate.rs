use super::*;

// ---------------------------------------------------------------------
// Pure logic core: Ready manual check/discard, download gate, pending ownership, marker result
//
// These free functions extract the Tauri shell decisions most vulnerable to races and ordering
// bugs into pure functions that can be tested without `tauri`, `tauri_plugin_updater`, or
// `updater_install`.
// ---------------------------------------------------------------------

fn parse_semver(version: &str) -> Option<([u64; 3], Vec<&str>)> {
    let without_build = version.split_once('+').map_or(version, |(head, _)| head);
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, Vec::new()), |(core, pre)| {
            (core, pre.split('.').collect())
        });
    let mut parts = core.split('.');
    let parsed = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    if parts.next().is_some()
        || prerelease.iter().any(|part| part.is_empty())
        || prerelease.iter().any(|part| {
            !part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        })
    {
        return None;
    }
    Some((parsed, prerelease))
}

fn compare_prerelease(left: &[&str], right: &[&str]) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    for (left, right) in left.iter().zip(right) {
        let ordering = match (left.parse::<u64>(), right.parse::<u64>()) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => left.cmp(right),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

/// Replaces old staging only when the manifest version is definitively higher than the staged
/// version under SemVer. Parse failures conservatively return false so an installable Ready
/// package is retained rather than deleting a package with an unknown version relationship.
pub(super) fn is_version_newer(candidate: &str, staged: &str) -> bool {
    let (candidate_core, candidate_pre) = match parse_semver(candidate) {
        Some(version) => version,
        None => return false,
    };
    let (staged_core, staged_pre) = match parse_semver(staged) {
        Some(version) => version,
        None => return false,
    };
    candidate_core
        .cmp(&staged_core)
        .then_with(|| compare_prerelease(&candidate_pre, &staged_pre))
        == Ordering::Greater
}

pub(super) struct ReadyCleanupFsOps<'a> {
    pub(super) cleanup_staged: &'a mut dyn FnMut(&str) -> Result<(), String>,
    pub(super) clear_marker: &'a mut dyn FnMut() -> Result<(), String>,
}

/// Finalization boundary for a manual Ready check. Old staging and its marker are removed only
/// when a higher version is found. Any failure enters a visible Error instead of publishing an
/// Available state alongside old staging.
pub(super) fn finish_check_with_ready_cleanup(
    machine: &mut Machine,
    manual: bool,
    outcome: CheckOutcome,
    fs_ops: &mut ReadyCleanupFsOps,
) -> UpdaterSnapshot {
    if let Some(staged_path) = machine.ready_replacement_path(&outcome).map(str::to_owned) {
        let cleanup_result =
            (fs_ops.cleanup_staged)(&staged_path).and_then(|()| (fs_ops.clear_marker)());
        if let Err(detail) = cleanup_result {
            return machine.ready_action_failed(crate::ui_msg::al_err(
                "updater.discard_failed",
                &[("detail", detail)],
            ));
        }
    }
    machine.on_check_result(manual, outcome)
}

/// Pure execution core for `updater_discard_update`. The gate precedes every file operation.
/// After successful cleanup it clears the marker and returns to Idle; cleanup failure preserves
/// the marker unchanged and enters a visible Error.
pub(super) fn discard_ready_update(
    machine: &mut Machine,
    fs_ops: &mut ReadyCleanupFsOps,
) -> Result<UpdaterSnapshot, UpdaterState> {
    let staged_path = match &machine.snapshot.state {
        UpdaterState::Ready { staged_path, .. } => staged_path.clone(),
        state => return Err(state.clone()),
    };
    if let Err(detail) = (fs_ops.cleanup_staged)(&staged_path) {
        return Ok(machine.ready_action_failed(crate::ui_msg::al_err(
            "updater.discard_failed",
            &[("detail", detail)],
        )));
    }
    if let Err(detail) = (fs_ops.clear_marker)() {
        return Ok(machine.ready_action_failed(crate::ui_msg::al_err(
            "updater.discard_failed",
            &[("detail", detail)],
        )));
    }
    machine.discard_ready()
}

/// Atomic gate for `updater_download_and_install`: validates single-flight state, applies
/// preflight, and chooses `Downloading` or `Error` in one call. A preflight failure produces the
/// single transition `Available → Error`, so an intermediate `Downloading` snapshot is
/// structurally impossible.
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadGate {
    /// The current state is not `Available`; the single-flight gate rejects without a transition.
    Busy(UpdaterSnapshot),
    /// Preflight failed: `Available → Error`, without entering `Downloading`.
    Rejected(UpdaterSnapshot),
    /// Preflight passed: `Available → Downloading`, with this download's generation.
    Proceed { snapshot: UpdaterSnapshot, gen: u64 },
}

impl DownloadGate {
    /// Returns the snapshot ultimately produced by this call for every branch, allowing the
    /// caller to emit it uniformly.
    pub fn snapshot(&self) -> UpdaterSnapshot {
        match self {
            DownloadGate::Busy(s) | DownloadGate::Rejected(s) => s.clone(),
            DownloadGate::Proceed { snapshot, .. } => snapshot.clone(),
        }
    }
}

pub(super) fn begin_download_gate(
    machine: &mut Machine,
    preflight: Result<(), String>,
) -> DownloadGate {
    if !matches!(machine.snapshot().state, UpdaterState::Available { .. }) {
        return DownloadGate::Busy(machine.snapshot());
    }
    match preflight {
        Err(reason) => {
            let snap = machine
                .reject_not_installable(crate::ui_msg::al_err(
                    "updater.not_installable",
                    &[("detail", reason)],
                ))
                .expect("checked Available above");
            DownloadGate::Rejected(snap)
        }
        Ok(()) => {
            let (snapshot, gen) = machine.begin_download().expect("checked Available above");
            DownloadGate::Proceed { snapshot, gen }
        }
    }
}

/// Before downloading, the version in `pending`, the cached `Update`, must exactly match the
/// version displayed by `Available`. The generic `Option<&str>` keeps this decision testable in
/// pure Machine tests without constructing a `tauri_plugin_updater::Update`, which has no public
/// test constructor.
pub(super) fn pending_matches_available(
    state: &UpdaterState,
    pending_version: Option<&str>,
) -> bool {
    match state {
        UpdaterState::Available { version, .. } => pending_version == Some(version.as_str()),
        _ => false,
    }
}

/// Decides whether `pending` should remain after a transition. It is useful only in `Available`,
/// where the next download consumes it. `UpToDate`, `Error`, `Idle`, and every other state must
/// clear it to avoid retaining an obsolete downloadable `Update` after a later check skips that
/// version.
pub(super) fn should_retain_pending(state: &UpdaterState) -> bool {
    matches!(state, UpdaterState::Available { .. })
}

/// Result of writing the marker after staging completes. The separate type and pure function
/// allow tests to verify that a marker write failure cleans staging and never reports `Ready`,
/// without starting a Tauri app or touching the real file system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerOutcome {
    /// The marker was persisted, so reporting `Ready` is safe.
    Written,
    /// Marker persistence failed. `cleanup_ok` records whether staging cleanup also failed for
    /// logging only; the caller must never enter `Ready` in either case.
    Failed {
        write_error: String,
        cleanup_ok: bool,
    },
}

/// Injected closures represent `write_marker` and `cleanup_staged`; production uses
/// `updater_install::write_marker` and `updater_install::cleanup_staged`. Avoiding a direct
/// dependency in this pure function keeps it easy to test.
pub(super) fn finalize_marker<W, C>(write_marker: W, cleanup_staged: C) -> MarkerOutcome
where
    W: FnOnce() -> Result<(), String>,
    C: FnOnce() -> Result<(), String>,
{
    match write_marker() {
        Ok(()) => MarkerOutcome::Written,
        Err(write_error) => {
            let cleanup_ok = cleanup_staged().is_ok();
            MarkerOutcome::Failed {
                write_error,
                cleanup_ok,
            }
        }
    }
}

/// A staging directory left by an old transaction must be removed before creating new staging,
/// followed by clearing its marker. The caller injects all file-system actions so ordering and
/// the rule that cleanup failure prevents staging can be tested as pure logic.
pub(super) fn stage_after_old_staging_cleanup<T>(
    old_marker_exists: bool,
    cleanup_old: impl FnOnce() -> Result<(), String>,
    clear_old_marker: impl FnOnce() -> Result<(), String>,
    stage_new: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if old_marker_exists {
        cleanup_old()?;
        clear_old_marker()?;
    }
    stage_new()
}

/// Production download wiring: when an old marker exists, first claim and delete its staging
/// layer through the installer's validated cleanup path, then clear the marker. Centralizing
/// real file-system calls here avoids a bare `remove_dir_all` and lets wiring tests enforce path
/// escape rejection.
#[cfg(target_os = "macos")]
pub(super) fn stage_after_old_marker_cleanup<T>(
    old_marker: Option<(&std::path::Path, &crate::updater_install::TxnMarker)>,
    bundle_parent: &std::path::Path,
    stage_new: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    stage_after_old_staging_cleanup(
        old_marker.is_some(),
        || {
            let (_, marker) = old_marker.expect("old marker exists");
            crate::updater_install::cleanup_staged(bundle_parent, &marker.staged_path)
                .map_err(|e| format!("failed to clean old staging before staging new update: {e}"))
        },
        || {
            let (marker_dir, _) = old_marker.expect("old marker exists");
            crate::updater_install::clear_marker_checked(marker_dir).map_err(|e| e.to_string())
        },
        stage_new,
    )
}

/// Reading the marker directory and contents is part of the old-staging cleanup transaction.
/// Direct staging is allowed only when the marker file is confirmed absent; directory access,
/// reads, or JSON parsing failures must abort.
#[cfg(target_os = "macos")]
pub(super) fn stage_after_old_marker_lookup<T>(
    marker_dir: Result<std::path::PathBuf, String>,
    bundle_parent: &std::path::Path,
    stage_new: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let marker_dir = marker_dir?;
    let old_marker = crate::updater_install::read_marker_checked(&marker_dir)
        .map_err(|e| format!("failed to read old update marker before staging: {e}"))?;
    stage_after_old_marker_cleanup(
        old_marker
            .as_ref()
            .map(|marker| (marker_dir.as_path(), marker)),
        bundle_parent,
        stage_new,
    )
}

/// Core decision shared by `updater_relaunch` and `updater_swap_back`, extracted as a pure
/// function independent of the real `updater_install` file system and operating only on a
/// `Machine` already in `Swapping`. Production wiring translates results from
/// `updater_install::swap`, `swap_back`, and `/usr/bin/open -n` into these parameters. Tests can
/// supply `Ok(())`, `Err(..)`, and fixed-result closures without launching an `.app` or touching
/// `AGENTLOOM_UPDATER_FAULT`, whose process-global state would make parallel tests interfere.
#[derive(Debug, Clone, PartialEq)]
pub enum RelaunchOutcome {
    /// The swap and opening the new version both succeeded; the caller should immediately invoke
    /// `app.exit(0)` without returning to the application UI.
    Exit,
    /// Either the swap itself failed through `Machine::swap_failed`, or the swap succeeded but
    /// opening the new version failed through `Machine::relaunch_failed_after_swap`. The caller
    /// only forwards this snapshot because `Machine` has already selected the appropriate final
    /// state.
    Failed(UpdaterSnapshot),
}

pub(super) fn apply_relaunch_outcome(
    machine: &mut Machine,
    swap_result: Result<(), String>,
    open_new_bundle: impl FnOnce() -> Result<(), String>,
) -> RelaunchOutcome {
    match swap_result {
        Err(reason) => RelaunchOutcome::Failed(machine.swap_failed(crate::ui_msg::al_err(
            "updater.swap_failed",
            &[("detail", reason)],
        ))),
        Ok(()) => {
            if let Err(detail) = open_new_bundle() {
                // The swap already succeeded, so do not use `swap_failed`: retrying would call
                // `swap()` again and restore the original physical contents.
                RelaunchOutcome::Failed(machine.relaunch_failed_after_swap(crate::ui_msg::al_err(
                    "updater.relaunch_failed",
                    &[("detail", detail)],
                )))
            } else {
                RelaunchOutcome::Exit
            }
        }
    }
}

/// Decision core for reopening after a completed swap. States other than `Error(Reopen)` are
/// returned unchanged. Marker, path, or version validation failures use `updater.reopen_failed`,
/// while LaunchServices failures keep `updater.relaunch_failed`; both retain `retry: Reopen`.
pub(super) fn apply_reopen_outcome<T>(
    machine: &mut Machine,
    validate: impl FnOnce() -> Result<T, String>,
    open_new_bundle: impl FnOnce(&T) -> Result<(), String>,
) -> RelaunchOutcome {
    if !machine.is_awaiting_reopen() {
        return RelaunchOutcome::Failed(machine.snapshot());
    }

    let target = match validate() {
        Ok(target) => target,
        Err(detail) => {
            return RelaunchOutcome::Failed(machine.relaunch_failed_after_swap(
                crate::ui_msg::al_err("updater.reopen_failed", &[("detail", detail)]),
            ))
        }
    };

    match open_new_bundle(&target) {
        Ok(()) => RelaunchOutcome::Exit,
        Err(detail) => RelaunchOutcome::Failed(machine.relaunch_failed_after_swap(
            crate::ui_msg::al_err("updater.relaunch_failed", &[("detail", detail)]),
        )),
    }
}

/// Startup recovery gate shared by all checks. Automatic checks wait for recovery or timeout;
/// manual checks immediately return the current state while recovery is incomplete to avoid
/// blocking the UI. A revision CAS rejects late recovery after a timeout so it cannot overwrite
/// a completed transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryGate {
    Wait,
    Proceed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CheckRecoveryGate {
    Wait,
    ReturnCurrent,
    Proceed,
}

pub(super) fn recovery_gate_decision(
    done: bool,
    elapsed: Duration,
    timeout: Duration,
) -> RecoveryGate {
    if done || elapsed >= timeout {
        RecoveryGate::Proceed
    } else {
        RecoveryGate::Wait
    }
}

pub(super) fn check_recovery_gate_decision(
    done: bool,
    manual: bool,
    elapsed: Duration,
    timeout: Duration,
) -> CheckRecoveryGate {
    match recovery_gate_decision(done, elapsed, timeout) {
        RecoveryGate::Proceed => CheckRecoveryGate::Proceed,
        RecoveryGate::Wait if manual => CheckRecoveryGate::ReturnCurrent,
        RecoveryGate::Wait => CheckRecoveryGate::Wait,
    }
}
