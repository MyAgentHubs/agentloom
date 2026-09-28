use super::super::{UpdaterSnapshot, UpdaterState};
use super::*;
use crate::updater::diag_log::updater_diag;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub fn skip_version(
    app: &AppHandle,
    handle: &UpdaterHandle,
    version: String,
) -> Result<UpdaterSnapshot, String> {
    if let Some(db) = app.try_state::<crate::db::Db>() {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        crate::db::set_app_setting(&conn, SKIPPED_VERSION_SETTING, &version)
            .map_err(|e| e.to_string())?;
    }
    let snap = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        let snap = rt.machine.skip(version);
        // If settling the skip leaves the state outside Available, clear the old pending Update
        // as well; otherwise a later check that skips the version leaves it downloadable.
        if !super::super::should_retain_pending(&snap.state) {
            rt.pending = None;
        }
        snap
    };
    emit_state(app, &snap);
    Ok(snap)
}

// -------------------------------------------------------------------
// Startup recovery
// -------------------------------------------------------------------

/// The return value of `apply_recovery` describes only the state in which the machine should
/// next initialize, or a deferred cleanup to perform. Cleanable `HealthyCleanup` and
/// `TreatAsSwapped` outcomes are no longer deleted immediately by the recovery thread. That
/// destructive operation removes the entire staging layer, including the old `.app`, and must
/// wait for evidence that the UI is alive; see `PendingCleanup` and `run_pending_cleanup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecoveryOutcome {
    /// No state-machine change or cleanup is needed: the orphan marker was cleared here, the
    /// degraded case where neither side is the target has no clear action, or the result is
    /// `Unknown` and retains the marker while logging a warning.
    Idle,
    Ready {
        version: String,
        staged_path: String,
    },
    RecoveryOffered {
        bundle_path: String,
        staged_path: String,
        target_version: String,
    },
    /// Cleanup intent from a healthy-cleanup or cleanable `TreatAsSwapped` branch. Actual
    /// `cleanup_staged` and `clear_marker` work is deferred to `run_pending_cleanup`, where the
    /// healthy handshake, recovery write, and get_state triggers converge.
    PendingCleanup { parent: PathBuf, staged: PathBuf },
}

/// Injection points for cleanup side effects used by `apply_recovery`. Apart from orphan
/// markers, only `TreatAsStaged` matching `skipped_version` (a staging version the user explicitly
/// abandoned) is deleted immediately by the recovery thread. Cleaning the old bundle after a
/// healthy update still waits in `PendingCleanup` for the UI healthy handshake. Every branch
/// clears the marker only after cleanup succeeds.
pub(super) struct RecoveryFsOps<'a> {
    pub(super) cleanup_staged: &'a mut dyn FnMut(&Path, &Path) -> Result<(), String>,
    pub(super) clear_marker: &'a mut dyn FnMut(),
}

/// Pure decision core for startup recovery, independent of `AppHandle` and `Machine`: read the
/// marker, call `updater_install::plan_recovery`, then clean up or report the state in which the
/// machine should initialize. Ordinary `#[test]` cases can drive it with a marker in a tempdir
/// without starting a Tauri app.
pub(super) fn apply_recovery(
    marker_dir: &Path,
    running_exe_bundle: &Path,
    path_exists: &dyn Fn(&Path) -> bool,
    read_version: &dyn Fn(&Path) -> Option<String>,
    skipped_version: Option<&str>,
    fs_ops: &mut RecoveryFsOps,
) -> RecoveryOutcome {
    let marker = crate::updater_install::read_marker(marker_dir);
    let plan = crate::updater_install::plan_recovery(
        marker.as_ref(),
        running_exe_bundle,
        path_exists,
        read_version,
    );

    match plan {
        crate::updater_install::RecoveryPlan::None => RecoveryOutcome::Idle,

        // Healthy: this process is running from the new bundle. Return the intent to delete the
        // old staged version and marker, deferring execution until evidence shows the UI is alive.
        crate::updater_install::RecoveryPlan::HealthyCleanup { staged_old } => {
            match marker.as_ref().and_then(|m| m.bundle_path.parent()) {
                Some(parent) => RecoveryOutcome::PendingCleanup {
                    parent: parent.to_path_buf(),
                    staged: staged_old,
                },
                // In nearly impossible degraded cases such as a bundle_path without a parent or
                // a missing marker, conservatively retain the marker and defer to manual review.
                // This is safer than discarding it merely because cleanup cannot be proven safe.
                None => RecoveryOutcome::Idle,
            }
        }

        // The staging path no longer exists, so this is an orphan marker with no directory to
        // delete. Clearing it is non-destructive and can happen immediately.
        crate::updater_install::RecoveryPlan::ClearStaleMarker => {
            (fs_ops.clear_marker)();
            RecoveryOutcome::Idle
        }

        // The unswapped branch of an ambiguous swapping state is treated as Staged: retain the
        // marker and initialize the machine as `Ready` so the user can request a restart again.
        crate::updater_install::RecoveryPlan::TreatAsStaged => match &marker {
            Some(m) if skipped_version == Some(m.target_version.as_str()) => {
                // `TreatAsStaged` proves the staged version is the target. If that target also
                // matches the skipped_version persisted after a successful swap_back, this is the
                // bad version that just failed to launch. Clean it up as explicitly abandoned
                // instead of rebuilding Ready. Retain the marker after cleanup failure for retry
                // on the next launch; never leave an unknown staging layer without its marker.
                match m.bundle_path.parent() {
                    Some(parent) => {
                        match (fs_ops.cleanup_staged)(parent, &m.staged_path) {
                            Ok(()) => (fs_ops.clear_marker)(),
                            Err(e) => updater_diag!(
                                "updater: 清理已跳过的暂存版本失败（marker 保留，下次启动重试）：{e}"
                            ),
                        }
                        RecoveryOutcome::Idle
                    }
                    None => RecoveryOutcome::Idle,
                }
            }
            Some(m) => RecoveryOutcome::Ready {
                version: m.target_version.clone(),
                staged_path: m.staged_path.display().to_string(),
            },
            None => RecoveryOutcome::Idle,
        },

        // Treat the swapped branch of an ambiguous swapping state as healthy cleanup, also
        // deferred, but report cleanup intent only when the running version really equals the
        // target. Otherwise retain the marker rather than risk deleting the wrong bundle.
        crate::updater_install::RecoveryPlan::TreatAsSwapped => match &marker {
            Some(m) => {
                let running_version = read_version(running_exe_bundle);
                if running_version.as_deref() == Some(m.target_version.as_str()) {
                    match m.bundle_path.parent() {
                        Some(parent) => RecoveryOutcome::PendingCleanup {
                            parent: parent.to_path_buf(),
                            staged: m.staged_path.clone(),
                        },
                        None => RecoveryOutcome::Idle,
                    }
                } else {
                    RecoveryOutcome::Idle
                }
            }
            None => RecoveryOutcome::Idle,
        },

        // The staging directory exists but its version cannot be read. Retain the marker, log a
        // warning, and leave the uncertain case for manual review.
        crate::updater_install::RecoveryPlan::Unknown { reason } => {
            updater_diag!("updater: 启动恢复判定 Unknown（{reason}），保留 marker，人工核实");
            RecoveryOutcome::Idle
        }

        // The user manually opened the old version at the staging path. Offer one-click rollback
        // and retain the marker unchanged so `updater_swap_back` can read it again.
        crate::updater_install::RecoveryPlan::RunningFromStaged { bundle_path } => match &marker {
            Some(m) => RecoveryOutcome::RecoveryOffered {
                bundle_path: bundle_path.display().to_string(),
                staged_path: m.staged_path.display().to_string(),
                target_version: m.target_version.clone(),
            },
            None => RecoveryOutcome::Idle,
        },
    }
}

/// Pending cleanup intent produced by healthy cleanup and stored in `Runtime`; execute it only
/// after this process receives the `updater_mark_healthy` handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingCleanupEntry {
    pub(super) parent: PathBuf,
    pub(super) staged: PathBuf,
}

/// Injection points used by `run_pending_cleanup`. These are separate from `RecoveryFsOps`
/// because `cleanup_staged` is a fallible destructive operation here, and failure must not also
/// clear the marker. `RecoveryFsOps.clear_marker` has no `Result` and cannot express that rule.
pub(super) struct PendingCleanupFsOps<'a> {
    pub(super) cleanup_staged: &'a mut dyn FnMut(&Path, &Path) -> Result<(), String>,
    pub(super) clear_marker: &'a mut dyn FnMut(),
}

/// Clear the marker only after successfully deleting the staging layer. On deletion failure,
/// log the error and retain the marker unchanged. The retained marker guarantees a real retry on
/// the next launch, when `plan_recovery` recalculates the same `HealthyCleanup` or
/// `TreatAsSwapped` result from both paths' actual versions and `apply_recovery` reports it again
/// as `PendingCleanup` intent.
pub(super) fn run_pending_cleanup(pending: &PendingCleanupEntry, fs_ops: &mut PendingCleanupFsOps) {
    match (fs_ops.cleanup_staged)(&pending.parent, &pending.staged) {
        Ok(()) => (fs_ops.clear_marker)(),
        Err(e) => {
            updater_diag!("updater: 延后清理暂存失败（忽略·marker 保留，下次启动再算一次）：{e}");
        }
    }
}

/// If the recovery thread decides the state machine must be replaced before `start()` manages
/// `UpdaterHandle`, wait for it to appear. Because `start()` runs synchronously on the setup thread
/// with no yield before `app.manage(...)`, normal scheduling should almost never wait here. This
/// loop is only a generous safety net for an extreme race, capped at one second.
fn wait_for_updater_handle(app: &AppHandle) -> Option<tauri::State<'_, UpdaterHandle>> {
    for _ in 0..50 {
        if let Some(h) = app.try_state::<UpdaterHandle>() {
            return Some(h);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    updater_diag!("updater: 启动恢复等不到 UpdaterHandle（忽略）");
    None
}

fn recover_on_startup_blocking(app: &AppHandle) {
    let Some(handle) = wait_for_updater_handle(app) else {
        return;
    };
    let recovery_revision = {
        let rt = handle.runtime.lock().expect("updater runtime poisoned");
        if matches!(rt.machine.snapshot().state, UpdaterState::Disabled { .. }) {
            return;
        }
        rt.machine.snapshot().revision
    };

    let dir = match marker_dir(app) {
        Ok(d) => d,
        Err(e) => {
            updater_diag!("updater: 启动恢复读不到 marker 目录（忽略）：{e}");
            return;
        }
    };
    let running_exe_bundle = match resolve_bundle_path() {
        Ok(p) => p,
        Err(e) => {
            updater_diag!("updater: 启动恢复解析当前 bundle 路径失败（忽略）：{e}");
            return;
        }
    };

    // The old bundle from a healthy new version remains only pending cleanup intent. Immediate
    // cleanup is injected here solely for an explicitly skipped TreatAsStaged version; see
    // `RecoveryFsOps`.
    let mut cleanup_fn = |parent: &Path, staged: &Path| -> Result<(), String> {
        crate::updater_install::cleanup_staged(parent, staged).map_err(|e| e.to_string())
    };
    let dir_for_clear = dir.clone();
    let mut clear_fn = move || {
        crate::updater_install::clear_marker(&dir_for_clear);
    };
    let skipped_version = load_skipped_version(app);

    let outcome = apply_recovery(
        &dir,
        &running_exe_bundle,
        &|p: &Path| p.symlink_metadata().is_ok(),
        &crate::updater_install::read_bundle_version,
        skipped_version.as_deref(),
        &mut RecoveryFsOps {
            cleanup_staged: &mut cleanup_fn,
            clear_marker: &mut clear_fn,
        },
    );

    // `target_state` (whether to replace the state machine) and `pending_cleanup` (whether to
    // store cleanup intent in `Runtime`) are independent. The `PendingCleanup` branch produces
    // no target state and records only the intent.
    let (target_state, pending_cleanup): (Option<UpdaterState>, Option<PendingCleanupEntry>) =
        match outcome {
            RecoveryOutcome::Idle => (None, None),
            RecoveryOutcome::Ready {
                version,
                staged_path,
            } => (
                Some(UpdaterState::Ready {
                    version,
                    staged_path,
                    last_error: None,
                }),
                None,
            ),
            RecoveryOutcome::RecoveryOffered {
                bundle_path,
                staged_path,
                target_version,
            } => (
                Some(UpdaterState::RecoveryOffered {
                    bundle_path,
                    staged_path,
                    target_version,
                    last_error: None,
                }),
                None,
            ),
            RecoveryOutcome::PendingCleanup { parent, staged } => {
                (None, Some(PendingCleanupEntry { parent, staged }))
            }
        };

    if target_state.is_none() && pending_cleanup.is_none() {
        return;
    }

    let (snap_to_emit, recovery_state_abandoned, cleanup_added) = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        if matches!(rt.machine.snapshot().state, UpdaterState::Disabled { .. }) {
            updater_diag!(
                "updater: 启动恢复算出需要覆盖状态机/待清理，但当前构建 Disabled，跳过（Disabled 状态机不应产生任何迁移）"
            );
            return;
        }
        let cleanup_added = pending_cleanup.is_some();
        if let Some(pending) = pending_cleanup {
            rt.pending_cleanup = Some(pending);
        }
        let (snap, abandoned) = match target_state {
            Some(state) => match rt
                .machine
                .recover_into_if_revision(recovery_revision, state)
            {
                Some(snap) => (Some(snap), false),
                None => (None, true),
            },
            None => (None, false),
        };
        (snap, abandoned, cleanup_added)
    };
    if recovery_state_abandoned {
        updater_diag!(
            "updater: 启动恢复结果已过期（revision 从 {recovery_revision} 发生变化），放弃覆盖当前状态"
        );
    }
    if let Some(snap) = snap_to_emit {
        emit_state(app, &snap);
    }
    if cleanup_added {
        maybe_run_pending_cleanup(app, &handle);
    }
}

/// `lib.rs` calls this once from `.setup()` before `updater::start`. It **immediately spawns a
/// separate thread** because reading the marker and `Info.plist` performs file I/O. Similar
/// keychain reads have previously blocked the calling thread by opening a system authorization
/// dialog, so recovery must never block the `.setup()` main thread.
pub fn recover_on_startup(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        recover_on_startup_blocking(&app);
        // Regardless of which early-return path `recover_on_startup_blocking` takes (no marker,
        // no UpdaterHandle, or a required replacement blocked by Disabled), completion should
        // tell the scheduler that this recovery pass ended. Reuse `wait_for_updater_handle` to
        // locate the handle; if it remains unavailable, the scheduler's 60-second timeout still
        // allows progress and prevents a permanent stall.
        if let Some(handle) = wait_for_updater_handle(&app) {
            handle.recovery_done.store(true, Ordering::SeqCst);
        }
    });
}
