#![cfg(test)]

use super::*;

// --- can_check 矩阵 ---------------------------------------------------

#[test]
fn can_check_true_for_idle_uptodate_error_both_manual_and_auto() {
    let mut m = idle();
    assert!(m.can_check(false));
    assert!(m.can_check(true));

    m.on_check_result(false, CheckOutcome::UpToDate);
    assert!(m.can_check(false));
    assert!(m.can_check(true));

    m.on_check_result(true, CheckOutcome::Error("boom".into()));
    assert!(m.can_check(false));
    assert!(m.can_check(true));
}

#[test]
fn can_check_available_manual_yes_auto_no() {
    let m = available("0.3.0");
    assert!(m.can_check(true), "手动检查允许 Available 时刷新清单");
    assert!(!m.can_check(false), "自动检查不该在已点亮时再查一次");
}

#[test]
fn can_check_ready_manual_yes_auto_no() {
    let m = ready("0.3.0", "/tmp/staged.app");
    assert!(m.can_check(true), "Ready 必须保留手动检查修复版的逃生口");
    assert!(!m.can_check(false), "Ready 自动检查仍不打扰用户");
}

#[test]
fn can_check_false_for_all_busy_states_both_manual_and_auto() {
    // Checking
    let mut m = idle();
    m.begin_check(true);
    assert!(!m.can_check(true));
    assert!(!m.can_check(false));

    // Downloading
    let mut m = available("0.3.0");
    m.begin_download().unwrap();
    assert!(!m.can_check(true));
    assert!(!m.can_check(false));

    // Staging
    let mut m = available("0.3.0");
    let (_, gen) = m.begin_download().unwrap();
    m.begin_staging(gen);
    assert!(!m.can_check(true));
    assert!(!m.can_check(false));

    // Swapping（U3 返工 P3：这个状态没有公开方法能产生，用 `in_state` 直接
    // 摆进去，覆盖之前漏掉的这一格）。
    let m = Machine::in_state(UpdaterState::Swapping);
    assert!(!m.can_check(true), "Swapping 手动也不该允许检查");
    assert!(!m.can_check(false), "Swapping 自动也不该允许检查");

    // RecoveryOffered（T3c 新增态：启动期恢复提供一键换回，检查同样恒拒）。
    let m = Machine::in_state(UpdaterState::RecoveryOffered {
        bundle_path: "/tmp/AgentLoom.app".into(),
        staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
        target_version: "0.3.0".into(),
        last_error: None,
    });
    assert!(!m.can_check(true), "RecoveryOffered 手动也不该允许检查");
    assert!(!m.can_check(false), "RecoveryOffered 自动也不该允许检查");
}

#[test]
fn can_check_false_for_disabled_any_reason() {
    for reason in [
        DisabledReason::Dev,
        DisabledReason::Platform,
        DisabledReason::Unsigned,
    ] {
        let m = Machine::new(Some(reason));
        assert!(!m.can_check(true), "{reason:?} 手动也不该允许检查");
        assert!(!m.can_check(false), "{reason:?} 自动也不该允许检查");
    }
}

// --- begin_check ---------------------------------------------------

#[test]
fn begin_check_advances_revision_and_state_when_allowed() {
    let mut m = idle();
    let snap = m.begin_check(false).expect("Idle 应允许自动检查");
    assert_eq!(snap.revision, 1);
    assert_eq!(snap.state, UpdaterState::Checking);
}

#[test]
fn begin_check_returns_none_and_does_not_advance_revision_when_downloading() {
    let mut m = available("0.3.0");
    let (before, _gen) = m.begin_download().unwrap();
    assert_eq!(before.revision, 2); // Idle(0) -> Available(1) -> Downloading(2)
    assert!(
        m.begin_check(false).is_none(),
        "Downloading 期间自动检查应直接被拒"
    );
    assert!(
        m.begin_check(true).is_none(),
        "Downloading 期间手动检查也应直接被拒（不打断下载）"
    );
    assert_eq!(m.snapshot().revision, 2, "被拒的检查请求不应推进 revision");
}

#[test]
fn begin_check_manual_allows_refresh_while_available() {
    let mut m = available("0.3.0");
    let base_revision = m.snapshot().revision;
    let snap = m
        .begin_check(true)
        .expect("Available 时手动检查应允许刷新清单");
    assert_eq!(snap.revision, base_revision + 1);
    assert_eq!(snap.state, UpdaterState::Checking);
}

#[test]
fn ready_manual_check_with_higher_version_cleans_old_stage_then_enters_available() {
    let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
    m.begin_check(true).expect("Ready 手检必须放行");
    let cleanup_calls = std::cell::RefCell::new(Vec::new());
    let clear_calls = std::cell::Cell::new(0u32);
    let mut cleanup_fn = |staged_path: &str| -> Result<(), String> {
        cleanup_calls.borrow_mut().push(staged_path.to_string());
        Ok(())
    };
    let mut clear_fn = || -> Result<(), String> {
        clear_calls.set(clear_calls.get() + 1);
        Ok(())
    };
    let snap = finish_check_with_ready_cleanup(
        &mut m,
        true,
        CheckOutcome::Available {
            version: "0.4.0".into(),
            notes: Some("fix".into()),
            pub_date: None,
        },
        &mut ReadyCleanupFsOps {
            cleanup_staged: &mut cleanup_fn,
            clear_marker: &mut clear_fn,
        },
    );
    assert!(matches!(
        snap.state,
        UpdaterState::Available { ref version, .. } if version == "0.4.0"
    ));
    assert_eq!(
        cleanup_calls.into_inner(),
        vec!["/Applications/.agentloom-update-old/AgentLoom.app"]
    );
    assert_eq!(clear_calls.get(), 1, "旧 marker 必须与旧暂存一起清掉");
}

#[test]
fn ready_manual_check_up_to_date_keeps_ready_and_does_not_touch_stage() {
    let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
    m.begin_check(true).expect("Ready 手检必须放行");
    let cleanup_calls = std::cell::Cell::new(0u32);
    let clear_calls = std::cell::Cell::new(0u32);
    let mut cleanup_fn = |_staged_path: &str| -> Result<(), String> {
        cleanup_calls.set(cleanup_calls.get() + 1);
        Ok(())
    };
    let mut clear_fn = || -> Result<(), String> {
        clear_calls.set(clear_calls.get() + 1);
        Ok(())
    };
    let snap = finish_check_with_ready_cleanup(
        &mut m,
        true,
        CheckOutcome::UpToDate,
        &mut ReadyCleanupFsOps {
            cleanup_staged: &mut cleanup_fn,
            clear_marker: &mut clear_fn,
        },
    );
    assert_eq!(
        snap.state,
        UpdaterState::Ready {
            version: "0.3.0".into(),
            staged_path: "/Applications/.agentloom-update-old/AgentLoom.app".into(),
            last_error: None,
        }
    );
    assert_eq!(cleanup_calls.get(), 0, "已是最新不能删除暂存包");
    assert_eq!(clear_calls.get(), 0, "已是最新不能清 marker");
}

#[test]
fn ready_manual_check_returning_same_available_version_also_keeps_ready() {
    // 插件以“当前已安装版本”为比较基准，所以磁盘上已有 0.3.0 暂存包
    // 时，服务器仍可能返回 Available(0.3.0)，而不是 UpToDate。对用户而
    // 言这同样是“没有更高修复版”，必须保留现有 Ready。
    let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
    m.begin_check(true).expect("Ready 手检必须放行");
    let mut cleanup_fn =
        |_staged_path: &str| -> Result<(), String> { panic!("相同版本不能删除暂存包") };
    let mut clear_fn = || -> Result<(), String> { panic!("相同版本不能清 marker") };
    let snap = finish_check_with_ready_cleanup(
        &mut m,
        true,
        CheckOutcome::Available {
            version: "0.3.0".into(),
            notes: None,
            pub_date: None,
        },
        &mut ReadyCleanupFsOps {
            cleanup_staged: &mut cleanup_fn,
            clear_marker: &mut clear_fn,
        },
    );
    assert_eq!(
        snap.state,
        UpdaterState::Ready {
            version: "0.3.0".into(),
            staged_path: "/Applications/.agentloom-update-old/AgentLoom.app".into(),
            last_error: None,
        }
    );
}

#[test]
fn semver_comparison_only_replaces_with_strictly_higher_version() {
    assert!(is_version_newer("0.3.1", "0.3.0"));
    assert!(is_version_newer("0.4.0-beta.1", "0.3.9"));
    assert!(is_version_newer("0.4.0", "0.4.0-rc.1"));
    assert!(!is_version_newer("0.3.0", "0.3.0"));
    assert!(!is_version_newer("0.2.9", "0.3.0"));
    assert!(!is_version_newer("not-semver", "0.3.0"));
}

#[test]
fn discard_ready_update_cleans_then_clears_marker_and_returns_idle() {
    let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
    let cleanup_calls = std::cell::Cell::new(0u32);
    let clear_calls = std::cell::Cell::new(0u32);
    let mut cleanup_fn = |_staged_path: &str| -> Result<(), String> {
        cleanup_calls.set(cleanup_calls.get() + 1);
        Ok(())
    };
    let mut clear_fn = || -> Result<(), String> {
        clear_calls.set(clear_calls.get() + 1);
        Ok(())
    };
    let snap = discard_ready_update(
        &mut m,
        &mut ReadyCleanupFsOps {
            cleanup_staged: &mut cleanup_fn,
            clear_marker: &mut clear_fn,
        },
    )
    .expect("Ready 应允许放弃更新");
    assert_eq!(snap.state, UpdaterState::Idle);
    assert_eq!(cleanup_calls.get(), 1);
    assert_eq!(clear_calls.get(), 1);
}

#[test]
fn discard_ready_update_rejects_every_non_ready_state_without_fs_calls() {
    for state in all_non_ready_states() {
        let mut m = Machine::in_state(state.clone());
        let before = m.snapshot();
        let mut cleanup_fn =
            |_staged_path: &str| -> Result<(), String> { panic!("非 Ready 不得调用 cleanup") };
        let mut clear_fn = || -> Result<(), String> { panic!("非 Ready 不得清 marker") };
        let error = discard_ready_update(
            &mut m,
            &mut ReadyCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        )
        .unwrap_err();
        assert_eq!(error, state);
        assert_eq!(m.snapshot(), before, "拒绝时不得迁移：{state:?}");
    }
}

#[test]
fn discard_ready_cleanup_failure_enters_error_and_keeps_marker() {
    let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
    let clear_calls = std::cell::Cell::new(0u32);
    let mut cleanup_fn =
        |_staged_path: &str| -> Result<(), String> { Err("permission denied".into()) };
    let mut clear_fn = || -> Result<(), String> {
        clear_calls.set(clear_calls.get() + 1);
        Ok(())
    };
    let snap = discard_ready_update(
        &mut m,
        &mut ReadyCleanupFsOps {
            cleanup_staged: &mut cleanup_fn,
            clear_marker: &mut clear_fn,
        },
    )
    .expect("合法状态的文件失败应落 Error 快照");
    match snap.state {
        UpdaterState::Error { msg, .. } => assert!(msg.contains("updater.discard_failed")),
        other => panic!("expected Error, got {other:?}"),
    }
    assert_eq!(clear_calls.get(), 0, "cleanup 失败时 marker 必须保留");
}

// --- on_check_result 折叠规则 ---------------------------------------

#[test]
fn auto_available_matching_skipped_version_folds_into_uptodate() {
    let mut m = idle();
    m.skip("0.3.0".into());
    let snap = m.on_check_result(
        false,
        CheckOutcome::Available {
            version: "0.3.0".into(),
            notes: None,
            pub_date: None,
        },
    );
    assert!(matches!(snap.state, UpdaterState::UpToDate { .. }));
    assert_eq!(m.pending_version(), None);
}

#[test]
fn manual_available_ignores_skipped_version() {
    let mut m = idle();
    m.skip("0.3.0".into());
    let snap = m.on_check_result(
        true,
        CheckOutcome::Available {
            version: "0.3.0".into(),
            notes: None,
            pub_date: None,
        },
    );
    assert!(
        matches!(snap.state, UpdaterState::Available { ref version, .. } if version == "0.3.0"),
        "手动检查应忽略跳过记录，用户主动查就该看到"
    );
}

#[test]
fn available_with_non_skipped_version_shows_available_for_auto_and_manual() {
    for manual in [false, true] {
        let mut m = idle();
        m.skip("0.2.0".into());
        let snap = m.on_check_result(
            manual,
            CheckOutcome::Available {
                version: "0.3.0".into(),
                notes: Some("notes".into()),
                pub_date: Some("2026-01-01".into()),
            },
        );
        assert!(
            matches!(snap.state, UpdaterState::Available { ref version, .. } if version == "0.3.0")
        );
    }
}

#[test]
fn auto_check_error_is_swallowed_into_idle() {
    let mut m = idle();
    let snap = m.on_check_result(false, CheckOutcome::Error("network down".into()));
    assert_eq!(snap.state, UpdaterState::Idle);
}

#[test]
fn manual_check_error_is_shown_verbatim() {
    let mut m = idle();
    let snap = m.on_check_result(true, CheckOutcome::Error("network down".into()));
    match snap.state {
        UpdaterState::Error { msg, .. } => assert_eq!(msg, "network down"),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn auto_targets_not_found_logs_and_returns_idle_not_uptodate() {
    let mut m = idle();
    let snap = m.on_check_result(false, CheckOutcome::TargetsNotFound);
    assert_eq!(
        snap.state,
        UpdaterState::Idle,
        "TargetsNotFound 是发布错误，不该伪装成已是最新"
    );
}

#[test]
fn manual_targets_not_found_is_a_visible_error() {
    let mut m = idle();
    let snap = m.on_check_result(true, CheckOutcome::TargetsNotFound);
    match snap.state {
        UpdaterState::Error { msg, .. } => {
            assert!(msg.contains("updater.targets_not_found"))
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

// --- skip -------------------------------------------------------------

#[test]
fn skip_current_available_version_immediately_hides_it_this_session() {
    let mut m = available("0.3.0");
    let snap = m.skip("0.3.0".into());
    assert!(matches!(snap.state, UpdaterState::UpToDate { .. }));
}

#[test]
fn skip_unrelated_version_does_not_disturb_current_state() {
    let mut m = available("0.3.0");
    let before = m.snapshot();
    let after = m.skip("0.2.9".into());
    assert_eq!(before, after, "跳过一个不相关的版本不该动当前显示的状态");
}

#[test]
fn skip_then_later_auto_check_of_same_version_folds() {
    let mut m = idle();
    m.skip("0.4.0".into());
    let snap = m.on_check_result(
        false,
        CheckOutcome::Available {
            version: "0.4.0".into(),
            notes: None,
            pub_date: None,
        },
    );
    assert!(matches!(snap.state, UpdaterState::UpToDate { .. }));
}

// --- revision 单调 ------------------------------------------------------

#[test]
fn revision_is_strictly_monotonic_across_a_long_sequence() {
    fn assert_advanced(last: &mut u64, snap: &UpdaterSnapshot) {
        assert!(snap.revision > *last, "revision 必须严格递增");
        *last = snap.revision;
    }

    let mut m = idle();
    let mut last = m.snapshot().revision;

    let s = m.begin_check(false).unwrap();
    assert_advanced(&mut last, &s);
    let s = m.on_check_result(
        false,
        CheckOutcome::Available {
            version: "0.5.0".into(),
            notes: None,
            pub_date: None,
        },
    );
    assert_advanced(&mut last, &s);
    let (s, gen) = m.begin_download().unwrap();
    assert_advanced(&mut last, &s);
    let s = m.on_progress(gen, 10, Some(100)).unwrap();
    assert_advanced(&mut last, &s);
    let s = m.begin_staging(gen).unwrap();
    assert_advanced(&mut last, &s);
    let s = m
        .on_staged(gen, "0.5.0".into(), "/tmp/x.app".into())
        .unwrap();
    assert_advanced(&mut last, &s);
}

#[test]
fn disabled_machine_never_transitions() {
    let mut m = Machine::new(Some(DisabledReason::Unsigned));
    let before = m.snapshot();
    assert!(m.begin_check(true).is_none());
    assert!(m.begin_check(false).is_none());
    assert!(m.begin_download().is_err());
    assert_eq!(m.snapshot(), before, "Disabled 状态机不应产生任何迁移");
}
