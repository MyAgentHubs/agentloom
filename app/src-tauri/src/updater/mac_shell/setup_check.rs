use super::super::{CheckOutcome, DisabledReason, Machine, UpdaterSnapshot};
use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

/// `Machine` and the currently cached `Update` share the **same lock**. Previously,
/// `pending: Mutex<Option<Update>>` used a separate lock, so `on_check_result` published
/// `Available` before storing the `Update`. A manual refresh could then observe the new
/// version while pending still held the previous `Update`. Every operation that affects
/// both values (settling a check result, skipping, or taking a download) uses one `lock()`.
pub(in crate::updater) struct Runtime {
    pub(in crate::updater) machine: Machine,
    pub(in crate::updater) pending: Option<Update>,
    /// Set by `updater_mark_healthy` after React's first frame and critical initialization.
    /// This is process-local so a previous launch's healthy result cannot leak into this one.
    pub(super) healthy_confirmed: bool,
    /// Pending cleanup intent produced by healthy cleanup; both conditions converge
    /// with `healthy_confirmed` under the same runtime lock.
    pub(super) pending_cleanup: Option<PendingCleanupEntry>,
}

/// Managed state registered through `app.manage(...)`. **This type itself is `pub`** because
/// the module-level `#[tauri::command]` definitions live directly in `updater.rs`, not in
/// this submodule. This lets `tauri::generate_handler!` in `lib.rs` resolve the generated
/// `updater::__cmd__updater_xxx` sibling from `updater::updater_xxx`: `generate_handler!`
/// only replaces the path's final segment with `__cmd__<name>` and does not follow `pub use`
/// re-exports. Command definitions must therefore share a level with their public paths.
/// This submodule contains implementation details, while the top-level commands only forward.
pub struct UpdaterHandle {
    pub(in crate::updater) runtime: Mutex<Runtime>,
    /// Startup recovery completion signal shared by every check through the recovery gate.
    /// Manual checks return while recovery is incomplete; automatic checks wait or time out.
    /// No `Arc` is needed: Tauri's state container already manages `UpdaterHandle` through
    /// shared references, so `&AtomicBool` can naturally be accessed concurrently.
    pub(super) recovery_done: AtomicBool,
}

fn pubkey_value_configured(pubkey: Option<&str>) -> bool {
    pubkey
        .map(|s| {
            let trimmed = s.trim();
            !trimmed.is_empty() && trimmed != PLACEHOLDER_PUBKEY
        })
        .unwrap_or(false)
}

fn updater_config_pubkey_configured(value: Option<&serde_json::Value>) -> bool {
    pubkey_value_configured(value.and_then(|v| v.as_str()))
}

fn pubkey_configured(app: &AppHandle) -> bool {
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|v| v.get("pubkey"));

    updater_config_pubkey_configured(pubkey)
}

/// Under `cfg(debug_assertions)`, explicitly setting
/// `AGENTLOOM_UPDATER_FORCE_ENABLE=1` skips `Disabled{dev}` (debug builds only,
/// never in release). This exists solely so deterministic fault injection —
/// e.g. `AGENTLOOM_UPDATER_FAULT=relaunch` — can be exercised: those faults only
/// fire on the real swap/relaunch path, which is unreachable before `Ready`.
/// Without this escape hatch a debug build stays stuck at `Disabled{dev}` and the
/// relaunch/swap fault-injection code paths can never run. It does not affect
/// `Disabled{Unsigned}`, which still requires a real temporary signing key.
fn force_enabled_for_fault_injection() -> bool {
    if cfg!(debug_assertions) {
        std::env::var("AGENTLOOM_UPDATER_FORCE_ENABLE").as_deref() == Ok("1")
    } else {
        false
    }
}

pub(super) fn load_skipped_version(app: &AppHandle) -> Option<String> {
    let db = app.try_state::<crate::db::Db>()?;
    let conn = db.0.lock().ok()?;
    crate::db::get_app_setting(&conn, SKIPPED_VERSION_SETTING).ok()?
}

/// `current_exe()` looks like `AgentLoom.app/Contents/MacOS/AgentLoom`; walk up three
/// levels (MacOS → Contents → AgentLoom.app) to obtain the bundle root, then its realpath.
pub(in crate::updater) fn resolve_bundle_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| "cannot resolve .app bundle from current_exe()".to_string())?;
    std::fs::canonicalize(bundle).map_err(|e| e.to_string())
}

pub(in crate::updater) fn emit_state(app: &AppHandle, snapshot: &UpdaterSnapshot) {
    if let Err(e) = app.emit("updater://state", snapshot) {
        eprintln!("updater: emit updater://state 失败（忽略）：{e}");
    }
}

pub(in crate::updater) fn marker_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub(super) fn installed_target_awaiting_reopen_in(
    marker_dir: &Path,
    running_version: &str,
) -> bool {
    let Some(marker) = crate::updater_install::read_marker(marker_dir) else {
        return false;
    };
    let version = crate::updater_install::read_bundle_version(&marker.bundle_path);
    crate::updater_install::swapped_awaiting_reopen(
        Some(&marker),
        version.as_deref(),
        running_version,
    )
}

pub(in crate::updater) fn installed_target_awaiting_reopen(app: &AppHandle) -> bool {
    let running_version = app.package_info().version.to_string();
    marker_dir(app)
        .map(|dir| installed_target_awaiting_reopen_in(&dir, &running_version))
        .unwrap_or(false)
}

pub(in crate::updater) fn awaiting_reopen_snapshot(runtime: &mut Runtime) -> UpdaterSnapshot {
    runtime.pending = None;
    runtime
        .machine
        .relaunch_failed_after_swap(crate::ui_msg::al_err(
            "updater.relaunch_failed",
            &[(
                "detail",
                "the installed update is awaiting LaunchServices reopen".to_string(),
            )],
        ))
}

pub(super) fn cleanup_staged_path(staged_path: &str) -> Result<(), String> {
    let staged = Path::new(staged_path);
    let parent = staged
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "staged path has no application parent".to_string())?;
    crate::updater_install::cleanup_staged(parent, staged).map_err(|e| e.to_string())
}

/// Start by deciding whether the updater is `Disabled`, managing state, and starting the
/// scheduler task when enabled.
pub fn start(app: &AppHandle) {
    let disabled = if cfg!(debug_assertions) && !force_enabled_for_fault_injection() {
        Some(DisabledReason::Dev)
    } else if !pubkey_configured(app) {
        Some(DisabledReason::Unsigned)
    } else {
        None
    };

    let mut machine = Machine::new(disabled);
    if disabled.is_none() {
        machine.set_skipped_version(load_skipped_version(app));
    }

    app.manage(UpdaterHandle {
        runtime: Mutex::new(Runtime {
            machine,
            pending: None,
            healthy_confirmed: false,
            pending_cleanup: None,
        }),
        recovery_done: AtomicBool::new(false),
    });

    if disabled.is_some() {
        return;
    }

    let scheduler_app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SCHEDULER_INITIAL_DELAY).await;
        loop {
            perform_check(&scheduler_app, false).await;
            tokio::time::sleep(SCHEDULER_INTERVAL).await;
        }
    });
}

const RECOVERY_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
const RECOVERY_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Poll `UpdaterHandle.recovery_done` and use the pure
/// `super::super::check_recovery_gate_decision` function to decide whether to keep waiting
/// or proceed. If the recovery thread cannot yet obtain the managed `UpdaterHandle` (which
/// should not happen because `start()` finishes synchronously first), treat `done` as `false`.
/// This cannot deadlock because the 60-second timeout still allows the check to proceed.
async fn recovery_gate_allows_check(app: &AppHandle, manual: bool) -> bool {
    let started = Instant::now();
    loop {
        let done = app
            .try_state::<UpdaterHandle>()
            .map(|h| h.recovery_done.load(Ordering::SeqCst))
            .unwrap_or(false);
        match super::super::check_recovery_gate_decision(
            done,
            manual,
            started.elapsed(),
            RECOVERY_WAIT_TIMEOUT,
        ) {
            super::super::CheckRecoveryGate::Proceed => {
                if !done {
                    eprintln!(
                        "updater: 等启动期恢复完成超过 {}s，放行检查（恢复结果会用 revision CAS，不能覆盖这次检查）",
                        RECOVERY_WAIT_TIMEOUT.as_secs()
                    );
                }
                return true;
            }
            super::super::CheckRecoveryGate::ReturnCurrent => return false,
            super::super::CheckRecoveryGate::Wait => {
                tokio::time::sleep(RECOVERY_WAIT_POLL_INTERVAL).await
            }
        }
    }
}

pub(super) fn take_pending_cleanup_if_healthy(
    runtime: &mut Runtime,
) -> Option<PendingCleanupEntry> {
    if !runtime.healthy_confirmed {
        return None;
    }
    runtime.pending_cleanup.take()
}

/// Run only after both the healthy handshake and recovery intent are present. `take()` ensures
/// destructive cleanup runs at most once in this process despite concurrent or repeated triggers.
pub(super) fn maybe_run_pending_cleanup(app: &AppHandle, handle: &UpdaterHandle) {
    let pending = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        take_pending_cleanup_if_healthy(&mut rt)
    };
    let Some(pending) = pending else {
        return;
    };
    let dir = match marker_dir(app) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "updater: 延后清理拿不到 marker 目录（忽略·marker 保留，下次启动再算一次）：{e}"
            );
            return;
        }
    };
    let mut cleanup_fn = |parent: &Path, staged: &Path| -> Result<(), String> {
        crate::updater_install::cleanup_staged(parent, staged).map_err(|e| e.to_string())
    };
    let mut clear_fn = || {
        crate::updater_install::clear_marker(&dir);
    };
    run_pending_cleanup(
        &pending,
        &mut PendingCleanupFsOps {
            cleanup_staged: &mut cleanup_fn,
            clear_marker: &mut clear_fn,
        },
    );
}

pub fn get_state(app: &AppHandle, handle: &UpdaterHandle) -> UpdaterSnapshot {
    maybe_run_pending_cleanup(app, handle);
    handle
        .runtime
        .lock()
        .expect("updater runtime poisoned")
        .machine
        .snapshot()
}

pub fn mark_healthy(app: &AppHandle, handle: &UpdaterHandle) {
    {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        if !rt.healthy_confirmed {
            rt.healthy_confirmed = true;
        }
    }
    maybe_run_pending_cleanup(app, handle);
}

pub(super) struct CheckRun {
    pub(super) outcome: CheckOutcome,
    pub(super) update: Option<Update>,
}

async fn run_plugin_check(app: &AppHandle) -> CheckRun {
    let updater = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
        Ok(u) => u,
        Err(e) => {
            return CheckRun {
                outcome: CheckOutcome::Error(crate::ui_msg::al_err(
                    "updater.check_failed",
                    &[("detail", e.to_string())],
                )),
                update: None,
            }
        }
    };

    match updater.check().await {
        Ok(Some(update)) => CheckRun {
            outcome: CheckOutcome::Available {
                version: update.version.clone(),
                notes: update.body.clone(),
                pub_date: update.date.map(|d| d.to_string()),
            },
            update: Some(update),
        },
        Ok(None) => CheckRun {
            outcome: CheckOutcome::UpToDate,
            update: None,
        },
        Err(tauri_plugin_updater::Error::TargetsNotFound(_)) => CheckRun {
            outcome: CheckOutcome::TargetsNotFound,
            update: None,
        },
        Err(e) => CheckRun {
            outcome: CheckOutcome::Error(crate::ui_msg::al_err(
                "updater.check_failed",
                &[("detail", e.to_string())],
            )),
            update: None,
        },
    }
}

/// Core check execution after the recovery gate has opened. `awaiting_reopen` must be tested
/// before the state machine's `begin_check`; when true, even the `run_check` closure is not
/// used to construct a network request.
pub(super) async fn perform_check_after_recovery_gate<Run, RunFuture, Emit, Finish>(
    handle: &UpdaterHandle,
    manual: bool,
    awaiting_reopen: bool,
    run_check: Run,
    mut emit: Emit,
    finish: Finish,
) -> UpdaterSnapshot
where
    Run: FnOnce() -> RunFuture,
    RunFuture: std::future::Future<Output = CheckRun>,
    Emit: FnMut(&UpdaterSnapshot),
    Finish: FnOnce(&mut Machine, bool, CheckOutcome) -> UpdaterSnapshot,
{
    if awaiting_reopen {
        let snapshot = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            awaiting_reopen_snapshot(&mut rt)
        };
        emit(&snapshot);
        return snapshot;
    }

    let checking_snapshot = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        rt.machine.begin_check(manual)
    };
    let Some(checking_snapshot) = checking_snapshot else {
        return handle
            .runtime
            .lock()
            .expect("updater runtime poisoned")
            .machine
            .snapshot();
    };
    emit(&checking_snapshot);

    let run = run_check().await;
    let snapshot = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        let snap = finish(&mut rt.machine, manual, run.outcome);
        if super::super::should_retain_pending(&snap.state) {
            rt.pending = run.update;
        } else {
            rt.pending = None;
        }
        snap
    };
    emit(&snapshot);
    snapshot
}

async fn perform_check(app: &AppHandle, manual: bool) -> UpdaterSnapshot {
    let handle = app.state::<UpdaterHandle>();

    if !recovery_gate_allows_check(app, manual).await {
        return handle
            .runtime
            .lock()
            .expect("updater runtime poisoned")
            .machine
            .snapshot();
    }

    // When the marker and canonical bundle's actual version prove the swap completed, the only
    // sensible check retry is to open it again; evaluate this before ordinary Available/Ready gates.
    let awaiting_reopen = installed_target_awaiting_reopen(app);
    perform_check_after_recovery_gate(
        &handle,
        manual,
        awaiting_reopen,
        || run_plugin_check(app),
        |snapshot| emit_state(app, snapshot),
        |machine, manual, outcome| {
            let mut cleanup_fn = |staged_path: &str| cleanup_staged_path(staged_path);
            let mut clear_fn = || {
                let dir = marker_dir(app)?;
                crate::updater_install::clear_marker(&dir);
                Ok(())
            };
            super::super::finish_check_with_ready_cleanup(
                machine,
                manual,
                outcome,
                &mut super::super::ReadyCleanupFsOps {
                    cleanup_staged: &mut cleanup_fn,
                    clear_marker: &mut clear_fn,
                },
            )
        },
    )
    .await
}

pub async fn check(app: &AppHandle, manual: bool) -> UpdaterSnapshot {
    perform_check(app, manual).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn updater_config_pubkey_configuration_matches_json_value_type() {
        let cases = vec![
            (Some(json!(42)), false, "number"),
            (Some(json!(null)), false, "null"),
            (Some(json!(true)), false, "bool"),
            (Some(json!({"a": 1})), false, "object"),
            (Some(json!([1, 2, 3])), false, "array"),
            (
                Some(json!(
                    "MCowBQYDK2VwAyEAF7lX8JvXhYQ6x9iE5r2oN3sK4dP1aB0cTgU="
                )),
                true,
                "legitimate-looking string",
            ),
            (Some(json!("")), false, "empty string"),
            (Some(json!(PLACEHOLDER_PUBKEY)), false, "exact placeholder"),
            (None, false, "missing key"),
        ];

        for (value, expected, label) in cases {
            assert_eq!(
                updater_config_pubkey_configured(value.as_ref()),
                expected,
                "case failed: {label}"
            );
        }
    }

    #[test]
    fn legitimate_pubkey_is_configured() {
        assert!(pubkey_value_configured(Some(
            "MCowBQYDK2VwAyEAF7lX8JvXhYQ6x9iE5r2oN3sK4dP1aB0cTgU="
        )));
    }

    #[test]
    fn empty_pubkey_is_not_configured() {
        assert!(!pubkey_value_configured(Some("")));
    }

    #[test]
    fn whitespace_only_pubkey_is_not_configured() {
        assert!(!pubkey_value_configured(Some("   ")));
    }

    #[test]
    fn exact_placeholder_pubkey_is_not_configured() {
        assert!(!pubkey_value_configured(Some(PLACEHOLDER_PUBKEY)));
    }

    #[test]
    fn whitespace_wrapped_placeholder_pubkey_is_not_configured() {
        assert!(!pubkey_value_configured(Some(
            "  REPLACE_WITH_REAL_PUBKEY  "
        )));
    }

    #[test]
    fn case_varied_placeholder_pubkey_is_configured() {
        // This pins the existing case-sensitivity blind spot and is intentionally not fixed here.
        assert!(pubkey_value_configured(Some("replace_with_real_pubkey")));
    }

    #[test]
    fn missing_or_non_string_pubkey_is_not_configured() {
        assert!(!pubkey_value_configured(None));
    }
}
