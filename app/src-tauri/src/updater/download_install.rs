use super::mac_shell::{
    awaiting_reopen_snapshot, download_with_watchdog, emit_state, installed_target_awaiting_reopen,
    marker_dir, resolve_bundle_path, DownloadOutcome, UpdaterHandle,
};
use super::{UpdaterSnapshot, UpdaterState};
use std::path::PathBuf;
use tauri::AppHandle;
use tauri_plugin_updater::Update;

pub(super) struct PreparedDownload {
    snapshot: UpdaterSnapshot,
    gen: u64,
    real_bundle: PathBuf,
    update: Update,
}

pub(super) struct StagedDownload {
    gen: u64,
    real_bundle: PathBuf,
    update: Update,
    staged_path: PathBuf,
}

/// Validates the single-flight state and pending update, runs preflight, and atomically
/// settles on either Downloading or Error before releasing the runtime lock.
pub(super) fn prepare_download(
    app: &AppHandle,
    handle: &UpdaterHandle,
) -> Result<PreparedDownload, UpdaterSnapshot> {
    let gate_result: Result<PreparedDownload, UpdaterSnapshot> = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");

        let current_state = rt.machine.snapshot().state;
        if !matches!(current_state, UpdaterState::Available { .. }) {
            return Err(rt.machine.snapshot());
        }

        // Repeat the installed-target check after the state gate and before Downloading
        // to block older clients that still route retry to download_and_install.
        if installed_target_awaiting_reopen(app) {
            let snapshot = awaiting_reopen_snapshot(&mut rt);
            drop(rt);
            emit_state(app, &snapshot);
            return Err(snapshot);
        }

        let pending_version = rt.pending.as_ref().map(|u| u.version.as_str());
        if !super::pending_matches_available(&current_state, pending_version) {
            eprintln!(
                "updater: pending Update 缺失或版本与 Available 不一致，拒绝下载（防御性拦截，正常路径不该到这）"
            );
            return Err(rt.machine.snapshot());
        }

        let preflight = resolve_bundle_path()
            .and_then(|b| crate::updater_install::preflight(&b).map_err(|e| e.to_string()));
        let outcome = match &preflight {
            Ok(_) => super::begin_download_gate(&mut rt.machine, Ok(())),
            Err(reason) => super::begin_download_gate(&mut rt.machine, Err(reason.clone())),
        };

        let result = match outcome {
            super::DownloadGate::Proceed { snapshot, gen } => {
                let real_bundle = preflight.expect("Proceed implies preflight succeeded");
                let update = rt
                    .pending
                    .clone()
                    .expect("pending_matches_available checked above guarantees Some");
                Ok(PreparedDownload {
                    snapshot,
                    gen,
                    real_bundle,
                    update,
                })
            }
            other => Err(other.snapshot()),
        };

        if !super::should_retain_pending(&rt.machine.snapshot().state) {
            rt.pending = None;
        }
        result
    };

    match gate_result {
        Ok(prepared) => Ok(prepared),
        Err(snapshot) => {
            emit_state(app, &snapshot);
            Err(snapshot)
        }
    }
}

/// Downloads the verified update payload, enters Staging, and writes the staged bundle.
pub(super) async fn download_and_stage(
    app: &AppHandle,
    handle: &UpdaterHandle,
    prepared: PreparedDownload,
) -> Result<StagedDownload, UpdaterSnapshot> {
    emit_state(app, &prepared.snapshot);
    let outcome = download_with_watchdog(prepared.update.clone(), app.clone(), prepared.gen).await;

    let bytes = match outcome {
        DownloadOutcome::Bytes(bytes) => bytes,
        DownloadOutcome::WatchdogTimeout => {
            let snapshot = {
                let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                rt.machine
                    .on_download_error(
                        prepared.gen,
                        crate::ui_msg::al_err("updater.download_timeout", &[]),
                    )
                    .unwrap_or_else(|| rt.machine.snapshot())
            };
            emit_state(app, &snapshot);
            return Err(snapshot);
        }
        DownloadOutcome::PluginError(detail) => {
            let snapshot = {
                let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                rt.machine
                    .on_download_error(
                        prepared.gen,
                        crate::ui_msg::al_err("updater.check_failed", &[("detail", detail)]),
                    )
                    .unwrap_or_else(|| rt.machine.snapshot())
            };
            emit_state(app, &snapshot);
            return Err(snapshot);
        }
    };

    let staging_snapshot = {
        let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
        rt.machine
            .begin_staging(prepared.gen)
            .unwrap_or_else(|| rt.machine.snapshot())
    };
    emit_state(app, &staging_snapshot);

    let version = prepared.update.version.clone();
    let bundle_for_stage = prepared.real_bundle.clone();
    let old_marker_dir = marker_dir(app);
    let stage_result = tauri::async_runtime::spawn_blocking(move || {
        let parent = bundle_for_stage
            .parent()
            .ok_or_else(|| "bundle path has no parent directory".to_string())?;
        super::stage_after_old_marker_lookup(old_marker_dir, parent, || {
            crate::updater_install::stage_bytes(
                &bundle_for_stage,
                &bytes,
                &version,
                &crate::updater_install::default_verify,
            )
            .map_err(|e| e.to_string())
        })
    })
    .await;

    let staged_path = match stage_result {
        Ok(Ok(path)) => path,
        Ok(Err(install_err)) => {
            let snapshot = {
                let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                rt.machine
                    .on_download_error(
                        prepared.gen,
                        crate::ui_msg::al_err("updater.stage_failed", &[("detail", install_err)]),
                    )
                    .unwrap_or_else(|| rt.machine.snapshot())
            };
            emit_state(app, &snapshot);
            return Err(snapshot);
        }
        Err(join_err) => {
            let snapshot = {
                let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                rt.machine
                    .on_download_error(
                        prepared.gen,
                        crate::ui_msg::al_err(
                            "updater.stage_failed",
                            &[("detail", join_err.to_string())],
                        ),
                    )
                    .unwrap_or_else(|| rt.machine.snapshot())
            };
            emit_state(app, &snapshot);
            return Err(snapshot);
        }
    };

    Ok(StagedDownload {
        gen: prepared.gen,
        real_bundle: prepared.real_bundle,
        update: prepared.update,
        staged_path,
    })
}

/// Writes the transaction marker and settles on Ready, or cleans up and reports Error.
pub(super) fn finalize_staged(
    app: &AppHandle,
    handle: &UpdaterHandle,
    staged: StagedDownload,
) -> UpdaterSnapshot {
    let marker = crate::updater_install::TxnMarker {
        target_version: staged.update.version.clone(),
        bundle_path: staged.real_bundle.clone(),
        staged_path: staged.staged_path.clone(),
        stage: crate::updater_install::Stage::Staged,
    };

    // A marker write failure must clean the staged bundle and must never report Ready.
    let marker_outcome = super::finalize_marker(
        || {
            marker_dir(app).and_then(|dir| {
                crate::updater_install::write_marker(&dir, &marker)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
        },
        || {
            staged
                .real_bundle
                .parent()
                .ok_or_else(|| "bundle path has no parent directory".to_string())
                .and_then(|parent| {
                    crate::updater_install::cleanup_staged(parent, &staged.staged_path)
                        .map_err(|e| e.to_string())
                })
        },
    );

    match marker_outcome {
        super::MarkerOutcome::Written => {
            let snapshot = {
                let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                rt.machine
                    .on_staged(
                        staged.gen,
                        staged.update.version.clone(),
                        staged.staged_path.display().to_string(),
                    )
                    .unwrap_or_else(|| rt.machine.snapshot())
            };
            emit_state(app, &snapshot);
            snapshot
        }
        super::MarkerOutcome::Failed {
            write_error,
            cleanup_ok,
        } => {
            if !cleanup_ok {
                eprintln!("updater: marker 写失败且清理暂存也失败，遗留暂存目录待下次启动人工核实");
            }
            let snapshot = {
                let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                rt.machine
                    .on_download_error(
                        staged.gen,
                        crate::ui_msg::al_err("updater.stage_failed", &[("detail", write_error)]),
                    )
                    .unwrap_or_else(|| rt.machine.snapshot())
            };
            emit_state(app, &snapshot);
            snapshot
        }
    }
}
