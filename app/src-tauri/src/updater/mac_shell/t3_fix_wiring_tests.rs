#![cfg(test)]

use super::*;
use crate::updater::ErrorRetry;
use crate::updater_install::{Stage, TxnMarker};
use std::cell::Cell;
use std::fs;

fn make_bundle(parent: &Path, version: &str) -> PathBuf {
    let bundle = parent.join("AgentLoom.app");
    let contents = bundle.join("Contents");
    fs::create_dir_all(&contents).unwrap();
    let mut info = plist::Dictionary::new();
    info.insert(
        "CFBundleShortVersionString".to_string(),
        plist::Value::String(version.to_string()),
    );
    plist::Value::Dictionary(info)
        .to_file_xml(contents.join("Info.plist"))
        .unwrap();
    fs::canonicalize(bundle).unwrap()
}

fn marker(bundle: &Path, staged: PathBuf, version: &str) -> TxnMarker {
    TxnMarker {
        target_version: version.to_string(),
        bundle_path: bundle.to_path_buf(),
        staged_path: staged,
        stage: Stage::Swapped,
    }
}

fn handle_in_state(state: UpdaterState) -> UpdaterHandle {
    UpdaterHandle {
        runtime: Mutex::new(Runtime {
            machine: Machine::in_state(state),
            pending: None,
            healthy_confirmed: false,
            pending_cleanup: None,
        }),
        recovery_done: AtomicBool::new(true),
    }
}

#[test]
fn swapped_marker_with_old_running_version_preempts_checks() {
    let tmp = tempfile::tempdir().unwrap();
    let install_parent = tmp.path().join("Applications");
    fs::create_dir_all(&install_parent).unwrap();
    let install_parent = fs::canonicalize(install_parent).unwrap();
    let bundle = make_bundle(&install_parent, "0.3.0");
    let staged = install_parent
        .join(".agentloom-update-old")
        .join("AgentLoom.app");
    let marker = marker(&bundle, staged.clone(), "0.3.0");
    crate::updater_install::write_marker(tmp.path(), &marker).unwrap();
    assert!(installed_target_awaiting_reopen_in(tmp.path(), "0.2.9"));

    for manual in [false, true] {
        for state in [
            UpdaterState::Available {
                version: "0.3.0".into(),
                notes: None,
                pub_date: None,
            },
            UpdaterState::Ready {
                version: "0.3.0".into(),
                staged_path: staged.display().to_string(),
                last_error: None,
            },
        ] {
            let handle = handle_in_state(state);
            let network_calls = Cell::new(0);
            let emit_calls = Cell::new(0);
            let snapshot = tauri::async_runtime::block_on(perform_check_after_recovery_gate(
                &handle,
                manual,
                installed_target_awaiting_reopen_in(tmp.path(), "0.2.9"),
                || async {
                    network_calls.set(network_calls.get() + 1);
                    CheckRun {
                        outcome: CheckOutcome::UpToDate,
                        update: None,
                    }
                },
                |_| emit_calls.set(emit_calls.get() + 1),
                |_, _, _| panic!("awaiting-reopen 命中后不应落定网络结果"),
            ));
            assert!(matches!(
                snapshot.state,
                UpdaterState::Error {
                    retry: ErrorRetry::Reopen,
                    ..
                }
            ));
            assert_eq!(network_calls.get(), 0, "已交换待重开时禁止联网检查");
            assert_eq!(emit_calls.get(), 1, "Error(Reopen) 快照必须 emit");
        }
    }
}

#[test]
fn swapped_marker_in_healthy_window_allows_manual_check() {
    let tmp = tempfile::tempdir().unwrap();
    let install_parent = tmp.path().join("Applications");
    fs::create_dir_all(&install_parent).unwrap();
    let install_parent = fs::canonicalize(install_parent).unwrap();
    let bundle = make_bundle(&install_parent, "0.3.0");
    let staged = install_parent
        .join(".agentloom-update-old")
        .join("AgentLoom.app");
    let marker = marker(&bundle, staged, "0.3.0");
    crate::updater_install::write_marker(tmp.path(), &marker).unwrap();
    assert!(!installed_target_awaiting_reopen_in(tmp.path(), "0.3.0"));

    let handle = handle_in_state(UpdaterState::Idle);
    let check_calls = Cell::new(0);
    let snapshot = tauri::async_runtime::block_on(perform_check_after_recovery_gate(
        &handle,
        true,
        installed_target_awaiting_reopen_in(tmp.path(), "0.3.0"),
        || async {
            check_calls.set(check_calls.get() + 1);
            CheckRun {
                outcome: CheckOutcome::UpToDate,
                update: None,
            }
        },
        |_| {},
        |machine, manual, outcome| machine.on_check_result(manual, outcome),
    ));

    assert_eq!(check_calls.get(), 1, "健康窗口必须继续走正常检查路径");
    assert!(matches!(snapshot.state, UpdaterState::UpToDate { .. }));
    assert!(!matches!(
        snapshot.state,
        UpdaterState::Error {
            retry: ErrorRetry::Reopen,
            ..
        }
    ));
}

#[test]
fn marker_with_missing_leaf_cleans_layer_clears_marker_and_stages() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("Applications");
    let marker_dir = tmp.path().join("marker");
    fs::create_dir_all(&parent).unwrap();
    fs::create_dir_all(&marker_dir).unwrap();
    let parent = fs::canonicalize(parent).unwrap();
    let bundle = make_bundle(&parent, "0.2.9");
    let layer = parent.join(".agentloom-update-missing-leaf");
    fs::create_dir_all(&layer).unwrap();
    let marker = marker(&bundle, layer.join("AgentLoom.app"), "0.3.0");
    crate::updater_install::write_marker(&marker_dir, &marker).unwrap();
    let staged = Cell::new(false);

    super::super::stage_after_old_marker_cleanup(Some((&marker_dir, &marker)), &parent, || {
        staged.set(true);
        Ok(())
    })
    .unwrap();

    assert!(!layer.exists());
    assert!(crate::updater_install::read_marker(&marker_dir).is_none());
    assert!(staged.get());
}

#[test]
fn marker_with_missing_layer_and_leaf_clears_marker_and_stages() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("Applications");
    let marker_dir = tmp.path().join("marker");
    fs::create_dir_all(&parent).unwrap();
    fs::create_dir_all(&marker_dir).unwrap();
    let parent = fs::canonicalize(parent).unwrap();
    let bundle = make_bundle(&parent, "0.2.9");
    let layer = parent.join(".agentloom-update-already-gone");
    let marker = marker(&bundle, layer.join("AgentLoom.app"), "0.3.0");
    crate::updater_install::write_marker(&marker_dir, &marker).unwrap();
    let staged = Cell::new(false);

    super::super::stage_after_old_marker_cleanup(Some((&marker_dir, &marker)), &parent, || {
        staged.set(true);
        Ok(())
    })
    .unwrap();

    assert!(crate::updater_install::read_marker(&marker_dir).is_none());
    assert!(staged.get());
}

#[test]
fn marker_outside_bundle_parent_is_rejected_without_stage_or_marker_clear() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("Applications");
    let marker_dir = tmp.path().join("marker");
    fs::create_dir_all(&parent).unwrap();
    fs::create_dir_all(&marker_dir).unwrap();
    let parent = fs::canonicalize(parent).unwrap();
    let bundle = make_bundle(&parent, "0.2.9");
    let layer = outside.path().join(".agentloom-update-escape");
    let staged_path = layer.join("AgentLoom.app");
    fs::create_dir_all(&staged_path).unwrap();
    let marker = marker(&bundle, staged_path, "0.3.0");
    crate::updater_install::write_marker(&marker_dir, &marker).unwrap();
    let staged = Cell::new(false);

    let detail =
        super::super::stage_after_old_marker_cleanup(Some((&marker_dir, &marker)), &parent, || {
            staged.set(true);
            Ok(())
        })
        .unwrap_err();
    assert!(!staged.get());
    assert!(crate::updater_install::read_marker(&marker_dir).is_some());

    let mut machine = Machine::in_state(UpdaterState::Available {
        version: "0.3.0".into(),
        notes: None,
        pub_date: None,
    });
    let (_, gen) = machine.begin_download().unwrap();
    machine.begin_staging(gen).unwrap();
    let snapshot = machine
        .on_download_error(
            gen,
            crate::ui_msg::al_err("updater.stage_failed", &[("detail", detail)]),
        )
        .unwrap();
    match snapshot.state {
        UpdaterState::Error { msg, retry, .. } => {
            assert_eq!(retry, ErrorRetry::Check);
            assert!(msg.contains("updater.stage_failed"));
        }
        other => panic!("expected Error(Check), got {other:?}"),
    }
}

#[test]
fn download_wiring_uses_validated_cleanup_and_preserves_outside_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("Applications");
    let marker_dir = tmp.path().join("marker");
    fs::create_dir_all(&parent).unwrap();
    fs::create_dir_all(&marker_dir).unwrap();
    let parent = fs::canonicalize(parent).unwrap();
    let bundle = make_bundle(&parent, "0.2.9");
    let outside_layer = outside.path().join(".agentloom-update-do-not-delete");
    let outside_staged = outside_layer.join("AgentLoom.app");
    fs::create_dir_all(&outside_staged).unwrap();
    let marker = marker(&bundle, outside_staged, "0.3.0");
    crate::updater_install::write_marker(&marker_dir, &marker).unwrap();

    let result =
        super::super::stage_after_old_marker_lookup(Ok(marker_dir.clone()), &parent, || Ok(()));

    assert!(result.is_err());
    assert!(
        outside_layer.exists(),
        "裸 remove_dir_all 会误删这个外部目录"
    );
    assert!(crate::updater_install::read_marker(&marker_dir).is_some());
}

#[test]
fn unreadable_old_marker_blocks_stage_and_keeps_stage_failed_retry_check() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("Applications");
    let marker_dir = tmp.path().join("marker");
    fs::create_dir_all(&parent).unwrap();
    fs::create_dir_all(&marker_dir).unwrap();
    let parent = fs::canonicalize(parent).unwrap();
    fs::write(marker_dir.join("updater-txn.json"), b"{ broken json").unwrap();
    let staged = Cell::new(false);

    let detail =
        super::super::stage_after_old_marker_lookup(Ok(marker_dir.clone()), &parent, || {
            staged.set(true);
            Ok(())
        })
        .unwrap_err();

    assert!(!staged.get());
    assert!(marker_dir.join("updater-txn.json").exists());
    let mut machine = Machine::in_state(UpdaterState::Available {
        version: "0.3.0".into(),
        notes: None,
        pub_date: None,
    });
    let (_, gen) = machine.begin_download().unwrap();
    machine.begin_staging(gen).unwrap();
    let snapshot = machine
        .on_download_error(
            gen,
            crate::ui_msg::al_err("updater.stage_failed", &[("detail", detail)]),
        )
        .unwrap();
    assert!(matches!(
        snapshot.state,
        UpdaterState::Error {
            retry: ErrorRetry::Check,
            ..
        }
    ));
}

#[test]
fn marker_directory_error_blocks_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = Cell::new(false);
    let result = super::super::stage_after_old_marker_lookup(
        Err("app data directory unavailable".into()),
        tmp.path(),
        || {
            staged.set(true);
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!staged.get());
}

#[test]
fn dangling_symlink_marker_is_preserved_and_blocks_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let marker_dir = tmp.path().join("marker");
    fs::create_dir_all(&marker_dir).unwrap();
    let marker_path = marker_dir.join("updater-txn.json");
    std::os::unix::fs::symlink(marker_dir.join("missing-target"), &marker_path).unwrap();
    let staged = Cell::new(false);

    let result = super::super::stage_after_old_marker_lookup(Ok(marker_dir), tmp.path(), || {
        staged.set(true);
        Ok(())
    });

    assert!(result.is_err());
    assert!(!staged.get());
    assert!(marker_path.symlink_metadata().is_ok());
}

#[test]
fn production_entrypoints_remain_wired_to_t3_fix_cores() {
    let source = include_str!("../../updater.rs");
    let check_body = source
        .split("async fn perform_check(app:")
        .nth(1)
        .unwrap()
        .split("pub async fn check(")
        .next()
        .unwrap();
    assert!(check_body.contains("perform_check_after_recovery_gate("));
    assert!(check_body.contains("installed_target_awaiting_reopen(app)"));

    let download_body = source
        .split("pub async fn download_and_install(")
        .nth(1)
        .unwrap()
        .split("fn open_failure_detail(")
        .next()
        .unwrap();
    assert!(download_body.contains("installed_target_awaiting_reopen(app)"));
    assert!(download_body.contains("super::stage_after_old_marker_lookup("));
    assert!(!download_body.contains("remove_dir_all"));

    let reopen_body = source
        .split("pub fn reopen(app:")
        .nth(1)
        .unwrap()
        .split("fn store_swap_back_skip(")
        .next()
        .unwrap();
    assert!(reopen_body.contains("apply_reopen_command("));
}

#[test]
fn reopen_command_validates_version_before_opening() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("Applications");
    fs::create_dir_all(&parent).unwrap();
    let parent = fs::canonicalize(parent).unwrap();
    let bundle = make_bundle(&parent, "0.2.9");
    let marker = marker(
        &bundle,
        parent.join(".agentloom-update-old").join("AgentLoom.app"),
        "0.3.0",
    );
    let mut machine = Machine::in_state(UpdaterState::Error {
        msg: "previous relaunch failure".into(),
        checked_at: 0,
        retry: ErrorRetry::Reopen,
    });
    let open_calls = Cell::new(0);

    let outcome = apply_reopen_command(
        &mut machine,
        || Ok(marker),
        |_| {
            open_calls.set(open_calls.get() + 1);
            Ok(())
        },
    );

    assert!(matches!(outcome, super::super::RelaunchOutcome::Failed(_)));
    assert!(matches!(
        machine.snapshot().state,
        UpdaterState::Error {
            retry: ErrorRetry::Reopen,
            ..
        }
    ));
    assert_eq!(open_calls.get(), 0);
}

#[test]
fn reopen_command_rejects_bundle_outside_recorded_parent_without_opening() {
    let recorded_parent = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let bundle = make_bundle(outside.path(), "0.3.0");
    let marker = marker(
        &bundle,
        recorded_parent
            .path()
            .join(".agentloom-update-old")
            .join("AgentLoom.app"),
        "0.3.0",
    );
    let mut machine = Machine::in_state(UpdaterState::Error {
        msg: "previous relaunch failure".into(),
        checked_at: 0,
        retry: ErrorRetry::Reopen,
    });
    let open_calls = Cell::new(0);

    let outcome = apply_reopen_command(
        &mut machine,
        || Ok(marker),
        |_| {
            open_calls.set(open_calls.get() + 1);
            Ok(())
        },
    );

    assert!(matches!(outcome, super::super::RelaunchOutcome::Failed(_)));
    assert_eq!(open_calls.get(), 0);
}
