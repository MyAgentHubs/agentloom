#![cfg(test)]

use super::*;

// --- T3c：begin_swap / swap_failed / apply_relaunch_outcome / recover_into

impl Machine {
    /// T3c：启动期恢复专用——跳过正常迁移规则，直接把状态摆成恢复判定算出
    /// 来的目标状态（`Ready`/`RecoveryOffered`）。这不是用户触发的迁移，是
    /// 「进程刚起来、状态机还没对外发布过任何快照时」的一次性初始化；
    /// `revision` 依然递增，前端「只接受更大 revision」这条不变量不受影响。
    /// 调用方（`mac_shell::recover_on_startup`）负责保证只在非 `Disabled`
    /// 时调用——`Disabled` 状态机永不产生迁移这条规则不能被恢复逻辑破坏。
    fn recover_into(&mut self, state: UpdaterState) -> UpdaterSnapshot {
        self.bump(state)
    }
}

#[test]
fn begin_swap_from_ready_transitions_to_swapping() {
    let mut m = ready("0.3.0", "/tmp/x.app");
    let before_revision = m.snapshot().revision;
    let snap = m.begin_swap().expect("Ready 应允许进入 Swapping");
    assert_eq!(snap.state, UpdaterState::Swapping);
    assert_eq!(snap.revision, before_revision + 1);
}

#[test]
fn begin_swap_rejected_from_every_non_ready_state_without_advancing_revision() {
    for state in all_non_ready_states() {
        let mut m = Machine::in_state(state.clone());
        let before = m.snapshot();
        let err = m.begin_swap().unwrap_err();
        assert_eq!(err, state, "拒绝时应原样带回当前状态：{state:?}");
        assert_eq!(
            m.snapshot(),
            before,
            "被拒绝的 begin_swap 不应产生任何迁移：{state:?}"
        );
    }
}

#[test]
fn begin_recovery_swap_from_recovery_offered_transitions_to_swapping() {
    let mut m = Machine::in_state(UpdaterState::RecoveryOffered {
        bundle_path: "/tmp/AgentLoom.app".into(),
        staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
        target_version: "0.3.0".into(),
        last_error: None,
    });
    let snap = m
        .begin_recovery_swap()
        .expect("RecoveryOffered 应允许进入 Swapping");
    assert_eq!(snap.state, UpdaterState::Swapping);
}

#[test]
fn begin_recovery_swap_rejected_from_ready_too() {
    // `begin_recovery_swap` 与 `begin_swap` 是两把不同触发状态的闸门，
    // 互不越权：`Ready` 走不了 `begin_recovery_swap`。
    let mut m = ready("0.3.0", "/tmp/x.app");
    let before = m.snapshot();
    let err = m.begin_recovery_swap().unwrap_err();
    assert!(matches!(err, UpdaterState::Ready { .. }));
    assert_eq!(m.snapshot(), before);
}

#[test]
fn begin_recovery_swap_rejected_from_every_non_recovery_offered_state_without_advancing_revision() {
    // U4 返工 P2-5：`begin_swap` 已经有全态矩阵，`begin_recovery_swap`
    // 补齐同款——覆盖 `Ready`/`Available`/所有 busy 态/`Disabled` 三种
    // 原因，全部原样拒绝、不产生迁移。
    for state in all_non_recovery_offered_states() {
        let mut m = Machine::in_state(state.clone());
        let before = m.snapshot();
        let err = m.begin_recovery_swap().unwrap_err();
        assert_eq!(err, state, "拒绝时应原样带回当前状态：{state:?}");
        assert_eq!(
            m.snapshot(),
            before,
            "被拒绝的 begin_recovery_swap 不应产生任何迁移：{state:?}"
        );
    }
}

#[test]
fn swap_failed_moves_swapping_back_to_ready_with_last_error_and_allows_retry() {
    // U4 返工 P1-1：交换失败不再是死胡同——`begin_swap`（Ready 起点）触
    // 发的失败退回 `Ready`（带 `last_error`），而不是 `Error`；用户能再
    // 点一次「重启以更新」，不用重新走一遍下载。
    let mut m = ready("0.3.0", "/tmp/x.app");
    m.begin_swap().unwrap();
    assert!(!m.can_check(true), "Swapping 中不该允许检查");
    let snap = m.swap_failed("boom".into());
    match snap.state {
        UpdaterState::Ready {
            version,
            staged_path,
            last_error,
        } => {
            assert_eq!(version, "0.3.0");
            assert_eq!(staged_path, "/tmp/x.app");
            assert_eq!(last_error.as_deref(), Some("boom"));
        }
        other => panic!("expected Ready with last_error, got {other:?}"),
    }
    assert!(m.can_check(true), "退回 Ready 后仍应允许手动检查修复版");
    assert!(!m.can_check(false), "退回 Ready 后自动检查仍应被拒");
    m.begin_swap()
        .expect("交换失败落回 Ready 后必须仍允许再次点『重启以更新』");
}

#[test]
fn recovery_swap_failure_returns_to_recovery_offered_with_last_error_and_allows_retry() {
    let mut m = Machine::in_state(UpdaterState::RecoveryOffered {
        bundle_path: "/tmp/AgentLoom.app".into(),
        staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
        target_version: "0.3.0".into(),
        last_error: None,
    });
    m.begin_recovery_swap().unwrap();
    let snap = m.swap_failed("boom".into());
    match snap.state {
        UpdaterState::RecoveryOffered {
            bundle_path,
            staged_path,
            target_version,
            last_error,
        } => {
            assert_eq!(bundle_path, "/tmp/AgentLoom.app");
            assert_eq!(staged_path, "/tmp/.agentloom-update-x/AgentLoom.app");
            assert_eq!(target_version, "0.3.0");
            assert_eq!(last_error.as_deref(), Some("boom"));
        }
        other => panic!("expected retryable RecoveryOffered, got {other:?}"),
    }
    m.begin_recovery_swap()
        .expect("反向物理交换失败后必须允许再次换回");
}

#[test]
fn begin_swap_allows_retry_from_ready_that_already_carries_a_last_error() {
    // `begin_swap` 只认 `Ready`，带没带 `last_error` 都算——用户点『重启
    // 以更新』重试时，Machine 侧不应该因为上一次失败留下的 `last_error`
    // 而多拒一次。
    let mut m = ready_with_error("0.3.0", "/tmp/x.app", "AL_ERR:updater.swap_failed:{}");
    let snap = m.begin_swap().expect("带 last_error 的 Ready 应仍允许重试");
    assert_eq!(snap.state, UpdaterState::Swapping);
}

#[test]
fn apply_relaunch_outcome_swap_error_never_attempts_open_and_returns_to_ready_with_last_error() {
    let mut m = ready("0.3.0", "/tmp/x.app");
    m.begin_swap().unwrap();
    let outcome = apply_relaunch_outcome(&mut m, Err("renameatx_np failed: EPERM".into()), || {
        panic!("交换失败时绝不该去尝试打开新版")
    });
    match outcome {
        RelaunchOutcome::Failed(snap) => match &snap.state {
            UpdaterState::Ready {
                version,
                staged_path,
                last_error,
            } => {
                assert_eq!(version, "0.3.0");
                assert_eq!(staged_path, "/tmp/x.app");
                let msg = last_error.as_deref().expect("last_error 必须非空");
                assert!(msg.contains("updater.swap_failed"), "msg={msg}");
                assert!(
                    msg.contains("renameatx_np failed"),
                    "detail 必须保留原因：{msg}"
                );
            }
            other => panic!("expected Ready with last_error, got {other:?}"),
        },
        RelaunchOutcome::Exit => panic!("交换失败不该走 Exit 分支"),
    }
    assert!(
        m.can_check(true),
        "带 last_error 的 Ready 也必须能手检修复版"
    );
    assert!(!m.can_check(false), "带 last_error 的 Ready 仍拒绝自动检查");
    m.begin_swap()
        .expect("交换失败落回 Ready 后必须仍允许再次点『重启以更新』");
}

#[test]
fn apply_relaunch_outcome_swap_ok_and_open_ok_yields_exit_without_extra_transition() {
    let mut m = ready("0.3.0", "/tmp/x.app");
    m.begin_swap().unwrap();
    let before = m.snapshot();
    let outcome = apply_relaunch_outcome(&mut m, Ok(()), || Ok(()));
    assert_eq!(outcome, RelaunchOutcome::Exit);
    assert_eq!(
        m.snapshot(),
        before,
        "Exit 分支不应再产生额外迁移——调用方随即 app.exit(0)，不回业务 UI"
    );
}

#[test]
fn apply_relaunch_outcome_swap_ok_but_open_fails_yields_relaunch_failed_error() {
    let mut m = ready("0.3.0", "/tmp/x.app");
    m.begin_swap().unwrap();
    let outcome = apply_relaunch_outcome(&mut m, Ok(()), || {
        Err("open exited with code 1: launch services rejected bundle".into())
    });
    match outcome {
        RelaunchOutcome::Failed(snap) => match &snap.state {
            UpdaterState::Error { msg, retry, .. } => {
                assert!(msg.contains("updater.relaunch_failed"), "msg={msg}");
                assert_eq!(*retry, ErrorRetry::Reopen);
                assert!(msg.contains("code 1"), "detail 必须保留退出码：{msg}");
                assert!(
                    msg.contains("launch services rejected bundle"),
                    "detail 必须保留 stderr 摘要：{msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        },
        RelaunchOutcome::Exit => panic!("打开新版失败不该走 Exit 分支"),
    }
}

#[test]
fn recovery_swap_open_failure_is_error_and_cannot_retry_exchange() {
    let mut m = Machine::in_state(UpdaterState::RecoveryOffered {
        bundle_path: "/tmp/AgentLoom.app".into(),
        staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
        target_version: "0.3.0".into(),
        last_error: None,
    });
    m.begin_recovery_swap().unwrap();
    let outcome = apply_relaunch_outcome(&mut m, Ok(()), || {
        Err("open exited with code 2: bad bundle".into())
    });
    let RelaunchOutcome::Failed(snap) = outcome else {
        panic!("open 失败不应退出")
    };
    assert!(matches!(snap.state, UpdaterState::Error { .. }));
    assert!(
        m.begin_recovery_swap().is_err(),
        "交换已成功但 open 失败后绝不能再次执行 RENAME_SWAP"
    );
}

#[test]
fn recover_into_bumps_revision_and_sets_given_state() {
    let mut m = idle();
    let before_revision = m.snapshot().revision;
    let target = UpdaterState::Ready {
        version: "0.4.0".into(),
        staged_path: "/tmp/y.app".into(),
        last_error: None,
    };
    let snap = m.recover_into(target.clone());
    assert_eq!(snap.revision, before_revision + 1);
    assert_eq!(snap.state, target);
}

#[test]
fn recover_into_if_revision_abandons_stale_recovery_result() {
    let mut m = idle();
    let recovery_revision = m.snapshot().revision;
    m.begin_check(true).unwrap();
    let before = m.snapshot();
    let recovered = m.recover_into_if_revision(
        recovery_revision,
        UpdaterState::Ready {
            version: "0.4.0".into(),
            staged_path: "/tmp/y.app".into(),
            last_error: None,
        },
    );
    assert_eq!(recovered, None);
    assert_eq!(
        m.snapshot(),
        before,
        "恢复期间已有迁移时，迟到恢复态不得覆盖当前状态"
    );
}

#[test]
fn reopen_rejects_non_reopen_state_without_attempting_validation_or_open() {
    let mut machine = idle();
    let before = machine.snapshot();
    let outcome = apply_reopen_outcome(
        &mut machine,
        || -> Result<(), String> { panic!("非 Reopen 态不应读取 marker") },
        |_| panic!("非 Reopen 态不应 open"),
    );
    assert_eq!(outcome, RelaunchOutcome::Failed(before.clone()));
    assert_eq!(machine.snapshot(), before);
}

#[test]
fn reopen_rejects_non_swapped_marker_and_keeps_reopen_retry() {
    let mut machine = Machine::in_state(UpdaterState::Error {
        msg: "previous relaunch failure".into(),
        checked_at: 0,
        retry: ErrorRetry::Reopen,
    });
    let outcome = apply_reopen_outcome(
        &mut machine,
        || Err("update marker is not swapped".into()),
        |_: &()| panic!("marker 非 Swapped 不应 open"),
    );
    let RelaunchOutcome::Failed(snapshot) = outcome else {
        panic!("校验失败不应退出")
    };
    match snapshot.state {
        UpdaterState::Error { msg, retry, .. } => {
            assert_eq!(retry, ErrorRetry::Reopen);
            assert!(msg.contains("updater.reopen_failed"));
            assert!(msg.contains("not swapped"));
        }
        other => panic!("expected Error(Reopen), got {other:?}"),
    }
}

#[test]
fn reopen_rejects_bundle_version_mismatch_without_open() {
    let mut machine = Machine::in_state(UpdaterState::Error {
        msg: "previous relaunch failure".into(),
        checked_at: 0,
        retry: ErrorRetry::Reopen,
    });
    let outcome = apply_reopen_outcome(
        &mut machine,
        || Err("bundle version mismatch: expected 0.3.0, found 0.2.9".into()),
        |_: &()| panic!("版本不符不应 open"),
    );
    let RelaunchOutcome::Failed(snapshot) = outcome else {
        panic!("校验失败不应退出")
    };
    assert!(matches!(
        snapshot.state,
        UpdaterState::Error {
            retry: ErrorRetry::Reopen,
            ..
        }
    ));
}

#[test]
fn reopen_open_failure_keeps_reopen_retry_and_relaunch_envelope() {
    let mut machine = Machine::in_state(UpdaterState::Error {
        msg: "previous relaunch failure".into(),
        checked_at: 0,
        retry: ErrorRetry::Reopen,
    });
    let outcome = apply_reopen_outcome(
        &mut machine,
        || Ok("/Applications/AgentLoom.app"),
        |_| Err("open exited with code 1".into()),
    );
    let RelaunchOutcome::Failed(snapshot) = outcome else {
        panic!("open 失败不应退出")
    };
    match snapshot.state {
        UpdaterState::Error { msg, retry, .. } => {
            assert_eq!(retry, ErrorRetry::Reopen);
            assert!(msg.contains("updater.relaunch_failed"));
        }
        other => panic!("expected Error(Reopen), got {other:?}"),
    }
}

#[test]
fn reopen_open_success_yields_exit() {
    let mut machine = Machine::in_state(UpdaterState::Error {
        msg: "previous relaunch failure".into(),
        checked_at: 0,
        retry: ErrorRetry::Reopen,
    });
    assert_eq!(
        apply_reopen_outcome(&mut machine, || Ok("bundle"), |_| Ok(())),
        RelaunchOutcome::Exit
    );
}

// --- U4 返工 P2-4：recovery_gate_decision ------------------------------

#[test]
fn recovery_gate_waits_while_not_done_and_within_timeout() {
    assert_eq!(
        recovery_gate_decision(false, Duration::from_secs(0), Duration::from_secs(60)),
        RecoveryGate::Wait,
        "恢复未完成、也没超时——不该放行自动检查"
    );
    assert_eq!(
        recovery_gate_decision(false, Duration::from_secs(59), Duration::from_secs(60)),
        RecoveryGate::Wait
    );
}

#[test]
fn recovery_gate_proceeds_immediately_once_done_flips_true() {
    // 核心诉求：恢复一完成就该立刻放行，不用等满超时。
    assert_eq!(
        recovery_gate_decision(true, Duration::from_secs(0), Duration::from_secs(60)),
        RecoveryGate::Proceed
    );
}

#[test]
fn recovery_gate_proceeds_after_timeout_even_if_still_not_done() {
    // 超时兜底：恢复线程万一卡住也不能让调度永远等下去。
    assert_eq!(
        recovery_gate_decision(false, Duration::from_secs(60), Duration::from_secs(60)),
        RecoveryGate::Proceed
    );
    assert_eq!(
        recovery_gate_decision(false, Duration::from_secs(61), Duration::from_secs(60)),
        RecoveryGate::Proceed
    );
}

#[test]
fn manual_and_auto_checks_share_gate_but_manual_returns_without_waiting() {
    let timeout = Duration::from_secs(60);
    assert_eq!(
        check_recovery_gate_decision(false, true, Duration::ZERO, timeout),
        CheckRecoveryGate::ReturnCurrent
    );
    assert_eq!(
        check_recovery_gate_decision(false, false, Duration::ZERO, timeout),
        CheckRecoveryGate::Wait
    );
    assert_eq!(
        check_recovery_gate_decision(true, true, Duration::ZERO, timeout),
        CheckRecoveryGate::Proceed
    );
    assert_eq!(
        check_recovery_gate_decision(false, true, timeout, timeout),
        CheckRecoveryGate::Proceed,
        "超时后手动与自动检查都可放行；迟到恢复由 revision CAS 拦截"
    );
}
