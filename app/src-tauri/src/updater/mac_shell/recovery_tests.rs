#![cfg(test)]

use super::*;
use crate::updater_install::{Stage, TxnMarker};
use std::collections::HashMap;

fn write_test_marker(dir: &Path, marker: &TxnMarker) {
    crate::updater_install::write_marker(dir, marker).unwrap();
}

fn version_reader(map: HashMap<PathBuf, String>) -> impl Fn(&Path) -> Option<String> {
    move |p: &Path| map.get(p).cloned()
}

/// U4 健康清理仍只返回 `PendingCleanup`；R1 新增的
/// skipped-version `TreatAsStaged` 分支会立即调用 cleanup。recorder
/// 同时记录 cleanup 与 clear，以锁住两条路径各自的副作用边界。
struct RecoveryRecorder {
    cleanup_calls: Vec<(PathBuf, PathBuf)>,
    clear_calls: u32,
}

/// 跑一次 `apply_recovery`，把 `clear_marker` 调用记进
/// `RecoveryRecorder` 里回传。
fn run(
    marker_dir: &Path,
    running_exe_bundle: &Path,
    path_exists: &dyn Fn(&Path) -> bool,
    read_version: &dyn Fn(&Path) -> Option<String>,
) -> (RecoveryOutcome, RecoveryRecorder) {
    run_with_skipped(
        marker_dir,
        running_exe_bundle,
        path_exists,
        read_version,
        None,
    )
}

fn run_with_skipped(
    marker_dir: &Path,
    running_exe_bundle: &Path,
    path_exists: &dyn Fn(&Path) -> bool,
    read_version: &dyn Fn(&Path) -> Option<String>,
    skipped_version: Option<&str>,
) -> (RecoveryOutcome, RecoveryRecorder) {
    let mut cleanup_calls = Vec::new();
    let mut clear_calls = 0;
    let outcome = {
        let mut cleanup_fn = |parent: &Path, staged: &Path| -> Result<(), String> {
            cleanup_calls.push((parent.to_path_buf(), staged.to_path_buf()));
            Ok(())
        };
        let mut clear_fn = || {
            clear_calls += 1;
        };
        apply_recovery(
            marker_dir,
            running_exe_bundle,
            path_exists,
            read_version,
            skipped_version,
            &mut RecoveryFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        )
    };
    (
        outcome,
        RecoveryRecorder {
            cleanup_calls,
            clear_calls,
        },
    )
}

#[test]
fn no_marker_is_idle_and_touches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (outcome, rec) = run(
        tmp.path(),
        Path::new("/Applications/AgentLoom.app"),
        &|_| true,
        &|_| None,
    );
    assert_eq!(outcome, RecoveryOutcome::Idle);
    assert_eq!(rec.clear_calls, 0);
}

#[test]
fn healthy_cleanup_returns_pending_cleanup_intent_without_touching_fs_ops() {
    // U4 返工 P1-2 核心断言：`apply_recovery` 返回的是「待清理意
    // 图」，不是直接删——`fs_ops`（这里只剩 `clear_marker`）在这条
    // 分支上必须零调用；真正的删除延后到 `run_pending_cleanup`
    // （由健康握手与恢复意图两侧条件齐备时触发）。
    let tmp = tempfile::tempdir().unwrap();
    let bundle = PathBuf::from("/Applications/AgentLoom.app");
    let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: bundle.clone(),
        staged_path: staged.clone(),
        stage: Stage::Swapped,
    };
    write_test_marker(tmp.path(), &marker);

    let versions = version_reader(HashMap::from([
        (bundle.clone(), "0.3.0".to_string()),
        (staged.clone(), "0.2.9".to_string()),
    ]));
    let (outcome, rec) = run(tmp.path(), &bundle, &|_| true, &versions);

    assert_eq!(
        outcome,
        RecoveryOutcome::PendingCleanup {
            parent: bundle.parent().unwrap().to_path_buf(),
            staged: staged.clone(),
        }
    );
    assert_eq!(
        rec.clear_calls, 0,
        "健康清理算出来的是待清理意图，恢复线程本身不该动手清 marker"
    );
}

#[test]
fn running_from_staged_offers_recovery_and_touches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = PathBuf::from("/Applications/AgentLoom.app");
    let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: bundle.clone(),
        staged_path: staged.clone(),
        stage: Stage::Swapped,
    };
    write_test_marker(tmp.path(), &marker);

    // 自身正跑在 staged_path 上（用户手动打开了旧版）。
    let (outcome, rec) = run(tmp.path(), &staged, &|_| true, &|_| None);

    assert_eq!(
        outcome,
        RecoveryOutcome::RecoveryOffered {
            bundle_path: bundle.display().to_string(),
            staged_path: staged.display().to_string(),
            target_version: "0.3.0".into(),
        }
    );
    assert_eq!(rec.clear_calls, 0, "提供一键换回前不该动 marker");
}

#[test]
fn orphan_marker_with_missing_staged_path_is_cleared() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = PathBuf::from("/Applications/AgentLoom.app");
    let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: bundle.clone(),
        staged_path: staged.clone(),
        stage: Stage::Staged,
    };
    write_test_marker(tmp.path(), &marker);

    // 暂存路径已经不存在了。
    let (outcome, rec) = run(tmp.path(), &bundle, &|_| false, &|_| None);

    assert_eq!(outcome, RecoveryOutcome::Idle);
    assert_eq!(rec.clear_calls, 1, "孤儿 marker 必须被清掉");
}

#[test]
fn swap_ambiguity_not_yet_swapped_initializes_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = PathBuf::from("/Applications/AgentLoom.app");
    let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: bundle.clone(),
        staged_path: staged.clone(),
        // marker 写的是 swapping，但两侧实际版本表明交换其实没发
        // 生——`plan_recovery` 完全不看这个字段，这里刻意让它跟实
        // 际版本矩阵对不上，验证确实是按版本矩阵判的。
        stage: Stage::Swapping,
    };
    write_test_marker(tmp.path(), &marker);

    let versions = version_reader(HashMap::from([
        (bundle.clone(), "0.2.9".to_string()),
        (staged.clone(), "0.3.0".to_string()),
    ]));
    // 跑在一个既不是 bundle 也不是 staged 的第三方路径上（Elsewhere）。
    let elsewhere = PathBuf::from("/tmp/somewhere-else/AgentLoom.app");
    let (outcome, rec) = run(tmp.path(), &elsewhere, &|_| true, &versions);

    assert_eq!(
        outcome,
        RecoveryOutcome::Ready {
            version: "0.3.0".into(),
            staged_path: staged.display().to_string(),
        }
    );
    assert_eq!(rec.clear_calls, 0, "未交换分支必须保留 marker");
}

#[test]
fn swap_back_skip_then_treat_as_staged_cleans_bad_version_instead_of_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = PathBuf::from("/Applications/AgentLoom.app");
    let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: bundle.clone(),
        staged_path: staged.clone(),
        // 成功 swap_back 的持久化终态正是 Staged：bundle 已换回旧版，
        // staged 又装着刚才起不来的目标版本。
        stage: Stage::Staged,
    };
    write_test_marker(tmp.path(), &marker);

    let mut persisted_skip = None;
    store_swap_back_skip(&marker.target_version, |version| {
        persisted_skip = Some(version.to_string());
        Ok(())
    })
    .expect("swap_back 成功后应写 skipped_version");

    let versions = version_reader(HashMap::from([
        (bundle.clone(), "0.2.9".to_string()),
        (staged.clone(), "0.3.0".to_string()),
    ]));
    let (outcome, rec) = run_with_skipped(
        tmp.path(),
        &bundle,
        &|_| true,
        &versions,
        persisted_skip.as_deref(),
    );

    assert_eq!(outcome, RecoveryOutcome::Idle, "坏版本不得重建 Ready");
    assert_eq!(
        rec.cleanup_calls,
        vec![(bundle.parent().unwrap().to_path_buf(), staged.clone())]
    );
    assert_eq!(rec.clear_calls, 1, "清理成功后必须清 marker");
}

#[test]
fn treat_as_staged_with_different_skipped_version_still_initializes_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = PathBuf::from("/Applications/AgentLoom.app");
    let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: bundle.clone(),
        staged_path: staged.clone(),
        stage: Stage::Staged,
    };
    write_test_marker(tmp.path(), &marker);
    let versions = version_reader(HashMap::from([
        (bundle.clone(), "0.2.9".to_string()),
        (staged.clone(), "0.3.0".to_string()),
    ]));

    let (outcome, rec) = run_with_skipped(tmp.path(), &bundle, &|_| true, &versions, Some("0.4.0"));

    assert_eq!(
        outcome,
        RecoveryOutcome::Ready {
            version: "0.3.0".into(),
            staged_path: staged.display().to_string(),
        }
    );
    assert!(rec.cleanup_calls.is_empty());
    assert_eq!(rec.clear_calls, 0, "正常 Ready 路径必须保留 marker");
}

#[test]
fn swap_ambiguity_already_swapped_cleans_up_only_when_running_version_matches_target() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = PathBuf::from("/Applications/AgentLoom.app");
    let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: bundle.clone(),
        staged_path: staged.clone(),
        stage: Stage::Swapping,
    };
    write_test_marker(tmp.path(), &marker);
    let elsewhere = PathBuf::from("/tmp/somewhere-else/AgentLoom.app");

    // 子用例 a：当前实际运行版本 == target → 健康清理。
    {
        let mut versions_map = HashMap::from([
            (bundle.clone(), "0.3.0".to_string()),
            (staged.clone(), "0.2.9".to_string()),
            (elsewhere.clone(), "0.3.0".to_string()),
        ]);
        let versions = version_reader(std::mem::take(&mut versions_map));
        let (outcome, rec) = run(tmp.path(), &elsewhere, &|_| true, &versions);
        assert_eq!(
            outcome,
            RecoveryOutcome::PendingCleanup {
                parent: bundle.parent().unwrap().to_path_buf(),
                staged: staged.clone(),
            },
            "运行版本已是目标版本，应当报出待清理意图（不是直接删）"
        );
        assert_eq!(
            rec.clear_calls, 0,
            "待清理意图不该在恢复线程这一步就清 marker"
        );
    }

    // 子用例 b：当前实际运行版本 != target → 不确定，保留 marker。
    {
        let versions = version_reader(HashMap::from([
            (bundle.clone(), "0.3.0".to_string()),
            (staged.clone(), "0.2.9".to_string()),
            (elsewhere.clone(), "0.2.9".to_string()),
        ]));
        let (outcome, rec) = run(tmp.path(), &elsewhere, &|_| true, &versions);
        assert_eq!(outcome, RecoveryOutcome::Idle);
        assert_eq!(
            rec.clear_calls, 0,
            "运行版本不是目标版本时不能贸然删——保留 marker"
        );
    }
}

#[test]
fn unknown_staged_version_unreadable_keeps_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = PathBuf::from("/Applications/AgentLoom.app");
    let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: bundle.clone(),
        staged_path: staged.clone(),
        stage: Stage::Staged,
    };
    write_test_marker(tmp.path(), &marker);

    // 暂存路径存在，但 Info.plist 读不出版本；bundle 也不是目标版本；
    // 跑在第三方路径上。
    let elsewhere = PathBuf::from("/tmp/somewhere-else/AgentLoom.app");
    let versions = version_reader(HashMap::from([(bundle.clone(), "0.2.9".to_string())]));
    let (outcome, rec) = run(tmp.path(), &elsewhere, &|_| true, &versions);

    assert_eq!(outcome, RecoveryOutcome::Idle);
    assert_eq!(rec.clear_calls, 0, "Unknown 必须保留 marker、交还人工核实");
}

// --- U4 返工 P2-1：run_pending_cleanup --------------------------

fn runtime_with_cleanup(pending_cleanup: Option<PendingCleanupEntry>) -> Runtime {
    Runtime {
        machine: Machine::new(None),
        pending: None,
        healthy_confirmed: false,
        pending_cleanup,
    }
}

#[test]
fn pending_cleanup_waits_for_health_handshake_and_is_taken_once() {
    let pending = PendingCleanupEntry {
        parent: PathBuf::from("/Applications"),
        staged: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
    };
    let mut runtime = runtime_with_cleanup(Some(pending.clone()));
    assert_eq!(take_pending_cleanup_if_healthy(&mut runtime), None);
    assert_eq!(runtime.pending_cleanup, Some(pending.clone()));

    runtime.healthy_confirmed = true;
    assert_eq!(take_pending_cleanup_if_healthy(&mut runtime), Some(pending));
    assert_eq!(
        take_pending_cleanup_if_healthy(&mut runtime),
        None,
        "重复触发不能再次执行破坏性清理"
    );
}

#[test]
fn pending_cleanup_runs_when_recovery_arrives_after_health_handshake() {
    let pending = PendingCleanupEntry {
        parent: PathBuf::from("/Applications"),
        staged: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
    };
    let mut runtime = runtime_with_cleanup(None);
    runtime.healthy_confirmed = true;
    assert_eq!(take_pending_cleanup_if_healthy(&mut runtime), None);
    runtime.pending_cleanup = Some(pending.clone());
    assert_eq!(
        take_pending_cleanup_if_healthy(&mut runtime),
        Some(pending),
        "恢复线程迟到写入意图时也必须由它自己的触发点启动清理"
    );
}

#[test]
fn run_pending_cleanup_success_clears_marker() {
    // 这正是健康握手与恢复意图齐备后要触发的那一步——cleanup
    // 成功，marker 才清。
    let pending = PendingCleanupEntry {
        parent: PathBuf::from("/Applications"),
        staged: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
    };
    let mut cleanup_calls: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut clear_calls = 0u32;
    {
        let mut cleanup_fn = |parent: &Path, staged: &Path| -> Result<(), String> {
            cleanup_calls.push((parent.to_path_buf(), staged.to_path_buf()));
            Ok(())
        };
        let mut clear_fn = || {
            clear_calls += 1;
        };
        run_pending_cleanup(
            &pending,
            &mut PendingCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        );
    }
    assert_eq!(
        cleanup_calls,
        vec![(pending.parent.clone(), pending.staged.clone())]
    );
    assert_eq!(clear_calls, 1, "cleanup 成功后才能清 marker");
}

#[test]
fn run_pending_cleanup_failure_keeps_marker() {
    // U4 返工 P2-1 核心断言：cleanup 失败——marker 绝不能被清掉（下
    // 次启动 `plan_recovery` 会按两路径实际版本重新算出同一个待清
    // 理意图，靠这份没被清掉的 marker 再试一次）。
    let pending = PendingCleanupEntry {
        parent: PathBuf::from("/Applications"),
        staged: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
    };
    let mut clear_calls = 0u32;
    {
        let mut cleanup_fn = |_parent: &Path, _staged: &Path| -> Result<(), String> {
            Err("permission denied".to_string())
        };
        let mut clear_fn = || {
            clear_calls += 1;
        };
        run_pending_cleanup(
            &pending,
            &mut PendingCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        );
    }
    assert_eq!(clear_calls, 0, "cleanup 失败绝不能连带清掉 marker");
}

// --- U4 返工 P2-5：relaunch 目标路径回归 --------------------------

#[test]
fn relaunch_target_path_is_always_bundle_path_never_staged_path() {
    // 把 `relaunch_target_path` 悄悄改成返回 `staged_path` 会让这
    // 条测试红——不管正向 `relaunch` 还是反向 `swap_back`，
    // LaunchServices 打开的都必须是规范安装路径 `bundle_path`。
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: PathBuf::from("/Applications/AgentLoom.app"),
        staged_path: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
        stage: Stage::Swapped,
    };
    let target = relaunch_target_path(&marker);
    assert_eq!(target, marker.bundle_path.as_path());
    assert_ne!(
        target,
        marker.staged_path.as_path(),
        "打开的绝不能是 staged_path"
    );
}

#[test]
fn launch_services_open_uses_exact_program_and_marker_bundle_argument() {
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: PathBuf::from("/Applications/AgentLoom.app"),
        staged_path: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
        stage: Stage::Swapped,
    };
    let calls = std::cell::RefCell::new(Vec::new());

    launch_services_open_with(relaunch_target_path(&marker), |program, args| {
        calls.borrow_mut().push((
            program.to_string(),
            args.iter()
                .map(|arg| (*arg).to_os_string())
                .collect::<Vec<_>>(),
        ));
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    })
    .expect("mocked LaunchServices open should succeed");

    let calls = calls.into_inner();
    assert_eq!(calls.len(), 1, "启动器必须且只能执行一次命令");
    assert_eq!(calls[0].0, "/usr/bin/open", "程序名必须锁定为 open");
    assert_eq!(
        calls[0].1,
        vec![
            std::ffi::OsString::from("-n"),
            marker.bundle_path.as_os_str().to_os_string(),
        ],
        "参数必须且只能按顺序为 -n 与 marker.bundle_path"
    );
    assert_ne!(
        calls[0].1[1],
        marker.staged_path.as_os_str(),
        "LaunchServices 绝不能打开 marker.staged_path"
    );
}

#[test]
fn open_failure_detail_keeps_exit_code_and_stderr_summary() {
    let detail = open_failure_detail(Some(7), b"LaunchServices rejected bundle\n");
    assert_eq!(
        detail,
        "/usr/bin/open exit code 7: LaunchServices rejected bundle"
    );
}
