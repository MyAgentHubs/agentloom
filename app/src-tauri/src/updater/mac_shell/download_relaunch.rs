use super::super::{Machine, UpdaterSnapshot, UpdaterState};
use super::*;
use crate::updater::diag_log::updater_diag;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::Update;

pub fn discard_update(app: &AppHandle, handle: &UpdaterHandle) -> Result<UpdaterSnapshot, String> {
    let snap = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        let mut cleanup_fn = |staged_path: &str| cleanup_staged_path(staged_path);
        let mut clear_fn = || {
            let dir = marker_dir(app)?;
            crate::updater_install::clear_marker(&dir);
            Ok(())
        };
        super::super::discard_ready_update(
            &mut rt.machine,
            &mut super::super::ReadyCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        )
        .map_err(|state| {
            crate::ui_msg::al_err(
                "updater.discard_failed",
                &[("detail", format!("wrong updater state: {state:?}"))],
            )
        })?
    };
    emit_state(app, &snap);
    match &snap.state {
        UpdaterState::Error { msg, .. } => Err(msg.clone()),
        _ => Ok(snap),
    }
}

pub(in crate::updater) enum DownloadOutcome {
    Bytes(Vec<u8>),
    WatchdogTimeout,
    PluginError(String),
}

/// Download with a no-progress watchdog. This deliberately does **not spawn**: the download
/// future is pinned directly into the current task with `std::pin::pin!`, and `select!` polls
/// it through `&mut`. When the watchdog wins, the function ends immediately because `select!`
/// is its final expression. The pinned future, including its reqwest response stream and
/// `on_chunk` closure, is dropped on return and can **never be polled again**, so the closure
/// physically cannot run. Merely abandoning an awaited spawned `JoinHandle` would instead
/// detach it under Tokio semantics and let the download continue.
pub(in crate::updater) async fn download_with_watchdog(
    mut update: Update,
    app: AppHandle,
    gen: u64,
) -> DownloadOutcome {
    update.timeout = Some(DOWNLOAD_TIMEOUT);

    let last_progress = Arc::new(std::sync::Mutex::new(Instant::now()));
    let downloaded_total = Arc::new(AtomicU64::new(0));

    let chunk_progress = Arc::clone(&last_progress);
    let chunk_downloaded = Arc::clone(&downloaded_total);
    let chunk_app = app.clone();

    let future = async {
        update
            .download(
                move |chunk_len, total| {
                    if let Ok(mut t) = chunk_progress.lock() {
                        *t = Instant::now();
                    }
                    let downloaded = chunk_downloaded
                        .fetch_add(chunk_len as u64, Ordering::Relaxed)
                        + chunk_len as u64;
                    let handle = chunk_app.state::<UpdaterHandle>();
                    let snap = {
                        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                        rt.machine.on_progress(gen, downloaded, total)
                    };
                    if let Some(snap) = snap {
                        emit_state(&chunk_app, &snap);
                    }
                },
                || {},
            )
            .await
    };
    let mut future = std::pin::pin!(future);

    let watchdog = async {
        loop {
            tokio::time::sleep(DOWNLOAD_WATCHDOG_POLL).await;
            let idle = last_progress
                .lock()
                .map(|t| t.elapsed())
                .unwrap_or(Duration::ZERO);
            if idle > DOWNLOAD_WATCHDOG_IDLE {
                break;
            }
        }
    };

    tokio::select! {
        result = &mut future => {
            match result {
                Ok(bytes) => DownloadOutcome::Bytes(bytes),
                Err(plugin_err) => DownloadOutcome::PluginError(plugin_err.to_string()),
            }
        }
        _ = watchdog => {
            DownloadOutcome::WatchdogTimeout
        }
    }
}

pub async fn download_and_install(app: &AppHandle) -> UpdaterSnapshot {
    let handle = app.state::<UpdaterHandle>();
    let prepared = match super::super::download_install::prepare_download(app, &handle) {
        Ok(prepared) => prepared,
        Err(snapshot) => return snapshot,
    };
    let staged =
        match super::super::download_install::download_and_stage(app, &handle, prepared).await {
            Ok(staged) => staged,
            Err(snapshot) => return snapshot,
        };
    super::super::download_install::finalize_staged(app, &handle, staged)
}

/// Restart through LaunchServices with `open -n <bundle_path>` instead of `app.restart()`.
/// Tauri 2.11.2 `process::restart` spawns from a dying process, allowing the new process to
/// inherit dead stdio or the process group and abort during launch; see upstream issue #15742.
/// Preserve the exit code and stderr excerpt on failure. `bundle_path` must already be a
/// realpath (`TxnMarker.bundle_path` stores one throughout staging and swapping), so no second
/// canonicalization is needed here.
pub(super) fn open_failure_detail(code: Option<i32>, stderr: &[u8]) -> String {
    let code = code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "terminated by signal".to_string());
    let stderr = String::from_utf8_lossy(stderr);
    let stderr_summary: String = stderr.trim().chars().take(500).collect();
    if stderr_summary.is_empty() {
        format!("/usr/bin/open exit code {code}")
    } else {
        format!("/usr/bin/open exit code {code}: {stderr_summary}")
    }
}

pub(super) fn launch_services_open_with(
    bundle_path: &Path,
    execute: impl FnOnce(&str, &[&std::ffi::OsStr]) -> std::io::Result<std::process::Output>,
) -> Result<(), String> {
    let args = [std::ffi::OsStr::new("-n"), bundle_path.as_os_str()];
    let output = execute("/usr/bin/open", &args)
        .map_err(|e| format!("failed to execute /usr/bin/open: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(open_failure_detail(output.status.code(), &output.stderr))
}

fn launch_services_open(bundle_path: &Path) -> Result<(), String> {
    launch_services_open_with(bundle_path, |program, args| {
        crate::proc::command(program).args(args).output()
    })
}

/// After a swap, the target passed to `open -n` is always `marker.bundle_path`, the canonical
/// installation location, and **never** `marker.staged_path`. For both forward `relaunch` and
/// reverse `swap_back`, LaunchServices, Dock, and Finder recognize this canonical path; the
/// physical contents were already exchanged by `do_swap`. Keep this in a small dedicated
/// function so the wrong field cannot be wired silently; the regression test fails if it is
/// changed to `staged_path`.
pub(super) fn relaunch_target_path(marker: &crate::updater_install::TxnMarker) -> &Path {
    &marker.bundle_path
}

/// Every successful LaunchServices-open path shares this exit: exit immediately on success;
/// on failure, only emit the error snapshot and return the wire message without further work.
fn finish_relaunch_outcome(
    app: &AppHandle,
    outcome: super::super::RelaunchOutcome,
) -> Result<(), String> {
    match outcome {
        super::super::RelaunchOutcome::Exit => {
            app.exit(0);
            Ok(())
        }
        super::super::RelaunchOutcome::Failed(snap) => {
            let msg = match &snap.state {
                UpdaterState::Error { msg, .. }
                | UpdaterState::Ready {
                    last_error: Some(msg),
                    ..
                }
                | UpdaterState::RecoveryOffered {
                    last_error: Some(msg),
                    ..
                } => msg.clone(),
                _ => crate::ui_msg::al_err(
                    "updater.relaunch_failed",
                    &[("detail", "missing relaunch failure detail".to_string())],
                ),
            };
            emit_state(app, &snap);
            Err(msg)
        }
    }
}

/// Shared executor for `updater_relaunch` (`Ready → Swapping`) and `updater_swap_back`
/// (`RecoveryOffered → Swapping`): pass the gate into `Swapping`, read the marker, invoke the
/// injected `do_swap` (`updater_install::swap` or `swap_back`, chosen by the caller), open the
/// new target bundle, then call `app.exit(0)` or settle in `Error`. The commands differ only in
/// their gate method and swap direction, so one function avoids two nearly identical versions.
fn perform_swap_and_relaunch(
    app: &AppHandle,
    handle: &UpdaterHandle,
    gate: impl FnOnce(&mut Machine) -> Result<UpdaterSnapshot, UpdaterState>,
    do_swap: impl FnOnce(
        &Path,
        &crate::updater_install::TxnMarker,
    ) -> Result<
        crate::updater_install::SwapOutcome,
        crate::updater_install::InstallError,
    >,
    after_swap: impl FnOnce(&crate::updater_install::TxnMarker) -> Result<(), String>,
) -> Result<(), String> {
    {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        match gate(&mut rt.machine) {
            Ok(snap) => {
                drop(rt);
                emit_state(app, &snap);
            }
            Err(state) => {
                return Err(crate::ui_msg::al_err(
                    "updater.relaunch_wrong_state",
                    &[("state", format!("{state:?}"))],
                ));
            }
        }
    }

    // Read the marker and perform the actual swap. Collapse any failure (unavailable marker
    // directory, missing marker, or `renameatx_np` failure) into one reason string and pass it
    // to `apply_relaunch_outcome` as the same `updater.swap_failed` `Error`. The caller need not
    // branch here; the state machine distinguishes only a failed swap from a successful swap
    // followed by failure to open the new version.
    let swap_result: Result<PathBuf, String> = (|| {
        let dir = marker_dir(app)?;
        let marker = crate::updater_install::read_marker(&dir)
            .ok_or_else(|| "update marker missing before swap".to_string())?;
        let bundle_path = relaunch_target_path(&marker).to_path_buf();
        do_swap(&dir, &marker).map_err(|e| e.to_string())?;
        // After the reverse swap succeeds, persist the bad version as skipped before open+exit.
        // This must not happen before swapping, or a failed swap would skip a working version.
        // A database error cannot masquerade as a swap failure because the physical exchange
        // already happened; at most log it and continue opening the restored old version. The
        // marker remains so the next launch retains its recovery anchor.
        if let Err(e) = after_swap(&marker) {
            updater_diag!("updater: 交换成功后的持久化动作失败（继续重启，marker 保留）：{e}");
        }
        Ok(bundle_path)
    })();

    let outcome = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        match swap_result {
            Err(reason) => {
                super::super::apply_relaunch_outcome(&mut rt.machine, Err(reason), || {
                    unreachable!("交换失败时不会调用 open")
                })
            }
            Ok(bundle_path) => {
                super::super::apply_relaunch_outcome(&mut rt.machine, Ok(()), || {
                    // Inject a relaunch fault after a successful swap but before LaunchServices
                    // opens the bundle. This applies only to debug builds with
                    // `AGENTLOOM_UPDATER_FAULT=relaunch`; `injected_fault()` is always `None`
                    // in release builds.
                    if crate::updater_install::injected_fault()
                        == Some(crate::updater_install::Fault::Relaunch)
                    {
                        Err("injected relaunch fault".to_string())
                    } else {
                        launch_services_open(&bundle_path)
                    }
                })
            }
        }
    };

    finish_relaunch_outcome(app, outcome)
}

/// `Ready` → validate → `Swapping` → `updater_install::swap` (forward
/// `Staged -> Swapping -> Swapped`) → open the new version with LaunchServices → exit.
pub fn relaunch(app: &AppHandle, handle: &UpdaterHandle) -> Result<(), String> {
    perform_swap_and_relaunch(
        app,
        handle,
        Machine::begin_swap,
        crate::updater_install::swap,
        |_| Ok(()),
    )
}

/// Command core for `updater_reopen`: read the marker, confirm it is swapped, and call the
/// installer's reopen validator. Trigger the injected LaunchServices open only after all succeed.
pub(super) fn apply_reopen_command(
    machine: &mut Machine,
    read_marker: impl FnOnce() -> Result<crate::updater_install::TxnMarker, String>,
    open_bundle: impl FnOnce(&PathBuf) -> Result<(), String>,
) -> super::super::RelaunchOutcome {
    super::super::apply_reopen_outcome(
        machine,
        || {
            let marker = read_marker()?;
            if marker.stage != crate::updater_install::Stage::Swapped {
                return Err(format!(
                    "update marker is not swapped (found {:?})",
                    marker.stage
                ));
            }
            crate::updater_install::validate_reopen_bundle(&marker).map_err(|e| e.to_string())
        },
        open_bundle,
    )
}

/// Dedicated retry when the swap completed and only opening the new version through
/// LaunchServices remains. This does not download, swap, or clean up any directory.
pub fn reopen(app: &AppHandle, handle: &UpdaterHandle) -> Result<(), String> {
    let outcome = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        if !rt.machine.is_awaiting_reopen() {
            return Err(crate::ui_msg::al_err(
                "updater.reopen_failed",
                &[(
                    "detail",
                    format!("wrong updater state: {:?}", rt.machine.snapshot().state),
                )],
            ));
        }
        apply_reopen_command(
            &mut rt.machine,
            || {
                let dir = marker_dir(app)?;
                crate::updater_install::read_marker(&dir)
                    .ok_or_else(|| "update marker missing before reopen".to_string())
            },
            |bundle_path| launch_services_open(bundle_path),
        )
    };
    finish_relaunch_outcome(app, outcome)
}

pub(super) fn store_swap_back_skip(
    target_version: &str,
    persist: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    persist(target_version)
}

fn persist_swap_back_skip(
    app: &AppHandle,
    handle: &UpdaterHandle,
    target_version: &str,
) -> Result<(), String> {
    let db = app
        .try_state::<crate::db::Db>()
        .ok_or_else(|| "database state unavailable".to_string())?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    store_swap_back_skip(target_version, |version| {
        crate::db::set_app_setting(&conn, SKIPPED_VERSION_SETTING, version)
            .map_err(|e| e.to_string())
    })?;
    drop(conn);
    handle
        .runtime
        .lock()
        .expect("updater runtime poisoned")
        .machine
        .set_skipped_version(Some(target_version.to_string()));
    Ok(())
}

/// Recovery path: `RecoveryOffered` → validate → `Swapping` →
/// `updater_install::swap_back` (reverse `Swapped -> Swapping -> Staged`) → open the original
/// `bundle_path`, which now contains the old version, through LaunchServices → exit.
pub fn swap_back(app: &AppHandle, handle: &UpdaterHandle) -> Result<(), String> {
    perform_swap_and_relaunch(
        app,
        handle,
        Machine::begin_recovery_swap,
        crate::updater_install::swap_back,
        |marker| persist_swap_back_skip(app, handle, &marker.target_version),
    )
}
