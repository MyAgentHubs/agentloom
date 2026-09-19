#![cfg(test)]

use super::*;

// --- 下载 / 暂存 / 单飞 -----------------------------------------------

#[test]
fn begin_download_from_available_transitions_to_downloading_zero_progress() {
    let mut m = available("0.3.0");
    let (snap, gen) = m.begin_download().expect("Available 应允许开始下载");
    assert_eq!(
        snap.state,
        UpdaterState::Downloading {
            downloaded: 0,
            total: None
        }
    );
    assert_eq!(gen, 1, "第一次下载的世代号应为 1");
}

#[test]
fn begin_download_rejected_from_every_non_available_state_without_advancing_revision() {
    for state in all_non_available_states() {
        let mut m = Machine::in_state(state.clone());
        let before = m.snapshot();
        let err = m.begin_download().unwrap_err();
        assert_eq!(err, state, "拒绝时应原样带回当前状态：{state:?}");
        assert_eq!(
            m.snapshot(),
            before,
            "被拒绝的 begin_download 不应产生任何迁移：{state:?}"
        );
    }
}

#[test]
fn begin_download_single_flight_rejects_second_call_while_downloading() {
    let mut m = available("0.3.0");
    m.begin_download().unwrap();
    let snapshot_before_retry = m.snapshot();
    let err = m.begin_download().unwrap_err();
    assert!(matches!(err, UpdaterState::Downloading { .. }));
    assert_eq!(
        m.snapshot(),
        snapshot_before_retry,
        "重复点击「下载并安装」不应产生第二次迁移"
    );
}

#[test]
fn on_progress_updates_downloading_payload_and_advances_revision_each_call() {
    let mut m = available("0.3.0");
    let (_, gen) = m.begin_download().unwrap();
    let r1 = m.on_progress(gen, 1024, Some(4096)).unwrap().revision;
    let snap2 = m.on_progress(gen, 2048, Some(4096)).unwrap();
    assert_eq!(snap2.revision, r1 + 1);
    assert_eq!(
        snap2.state,
        UpdaterState::Downloading {
            downloaded: 2048,
            total: Some(4096)
        }
    );
}

#[test]
fn full_happy_path_available_to_ready_via_staging() {
    let mut m = available("0.3.0");
    let (_, gen) = m.begin_download().unwrap();
    m.on_progress(gen, 4096, Some(4096));
    let staging = m.begin_staging(gen).expect("gen 匹配应允许进 Staging");
    assert_eq!(staging.state, UpdaterState::Staging);
    let ready = m
        .on_staged(
            gen,
            "0.3.0".into(),
            "/tmp/.agentloom-update-x/AgentLoom.app".into(),
        )
        .expect("gen 匹配应允许进 Ready");
    assert_eq!(
        ready.state,
        UpdaterState::Ready {
            version: "0.3.0".into(),
            staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
            last_error: None,
        }
    );
    assert_eq!(m.pending_version(), None);
}

#[test]
fn on_download_error_releases_guard_so_checks_are_allowed_again() {
    let mut m = available("0.3.0");
    let (_, gen) = m.begin_download().unwrap();
    assert!(!m.can_check(true), "下载中不该允许检查");
    let snap = m
        .on_download_error(gen, "boom".into())
        .expect("gen 匹配应允许转 Error");
    assert!(matches!(snap.state, UpdaterState::Error { .. }));
    assert!(
        m.can_check(true) && m.can_check(false),
        "下载失败后 guard 必须释放，Error 状态允许重新检查（含看门狗超时场景）"
    );
}

#[test]
fn download_error_from_staging_also_releases_guard() {
    let mut m = available("0.3.0");
    let (_, gen) = m.begin_download().unwrap();
    m.begin_staging(gen);
    assert!(!m.can_check(true));
    m.on_download_error(gen, "stage failed".into());
    assert!(m.can_check(true));
}

// --- U3 返工 P1：下载世代号 --------------------------------------------

#[test]
fn stale_generation_progress_is_ignored() {
    let mut m = available("0.3.0");
    let (_, gen1) = m.begin_download().unwrap();
    // 第一次下载失败 → Error（guard 释放）。
    m.on_download_error(gen1, "timeout".into());
    // 重新走一遍：check → Available → 第二次下载，拿到新世代号。
    m.on_check_result(
        true,
        CheckOutcome::Available {
            version: "0.3.0".into(),
            notes: None,
            pub_date: None,
        },
    );
    let (_, gen2) = m.begin_download().unwrap();
    assert_ne!(gen1, gen2, "两次下载的世代号必须不同");

    let before = m.snapshot();
    // 模拟第一次下载「迟到」的进度回调——即使真实实现已经用 P1 的真取消
    // 挡住了这种情况，Machine 这一层仍然要独立防住：旧世代号必须被忽略。
    assert!(
        m.on_progress(gen1, 999, Some(999)).is_none(),
        "旧世代号的进度必须被忽略"
    );
    assert_eq!(m.snapshot(), before, "旧世代号的进度不应产生任何迁移");

    // 新世代号仍然正常生效。
    assert!(m.on_progress(gen2, 10, Some(100)).is_some());
}

#[test]
fn progress_after_error_does_not_change_state_even_with_matching_generation() {
    let mut m = available("0.3.0");
    let (_, gen) = m.begin_download().unwrap();
    m.on_download_error(gen, "timeout".into());
    let after_error = m.snapshot();
    assert!(matches!(after_error.state, UpdaterState::Error { .. }));

    // 即使 gen 恰好还是同一个（比如没有发起过第二次下载），Error 之后的
    // progress 也必须被状态检查挡住——不能把 Error 又扒回 Downloading。
    assert!(
        m.on_progress(gen, 123, Some(456)).is_none(),
        "Error 之后同 gen 的 progress 也必须被忽略"
    );
    assert_eq!(m.snapshot(), after_error);
}

#[test]
fn stale_generation_staging_and_staged_and_error_are_all_ignored() {
    let mut m = available("0.3.0");
    let (_, stale_gen) = m.begin_download().unwrap();
    m.on_download_error(stale_gen, "timeout".into());

    assert!(m.begin_staging(stale_gen).is_none());
    assert!(m
        .on_staged(stale_gen, "0.3.0".into(), "/tmp/x.app".into())
        .is_none());
    // 已经在 Error 里，旧 gen 的 on_download_error 也不该再迁移一次。
    let before = m.snapshot();
    assert!(m.on_download_error(stale_gen, "again".into()).is_none());
    assert_eq!(m.snapshot(), before);
}

// --- U3 返工 P2-1：preflight 先于 Downloading（原子闸门）----------------

#[test]
fn preflight_failure_never_transitions_through_downloading() {
    let mut m = available("0.3.0");
    let before_revision = m.snapshot().revision;
    match begin_download_gate(&mut m, Err("parent directory not writable".into())) {
        DownloadGate::Rejected(snap) => {
            assert_eq!(
                snap.revision,
                before_revision + 1,
                "应当只发生一次迁移（Available 直接到 Error），不经过 Downloading——\
                 revision 只 +1 就是「emit 序列里没有 Downloading」的证明"
            );
            match snap.state {
                UpdaterState::Error { msg, .. } => assert!(msg.contains("not_installable")),
                other => panic!("expected Error, got {other:?}"),
            }
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert!(
        m.can_check(false),
        "preflight 失败后 guard 必须释放（回到 Error，允许重新检查）"
    );
}

#[test]
fn preflight_success_transitions_straight_to_downloading_with_fresh_generation() {
    let mut m = available("0.3.0");
    match begin_download_gate(&mut m, Ok(())) {
        DownloadGate::Proceed { snapshot, gen } => {
            assert_eq!(
                snapshot.state,
                UpdaterState::Downloading {
                    downloaded: 0,
                    total: None
                }
            );
            assert_eq!(gen, 1);
        }
        other => panic!("expected Proceed, got {other:?}"),
    }
}

#[test]
fn download_gate_busy_when_not_available_and_causes_no_transition() {
    for state in all_non_available_states() {
        let mut m = Machine::in_state(state.clone());
        let before = m.snapshot();
        match begin_download_gate(&mut m, Ok(())) {
            DownloadGate::Busy(snap) => assert_eq!(snap, before, "state={state:?}"),
            other => panic!("expected Busy for {state:?}, got {other:?}"),
        }
    }
}

// --- U3 返工 P2-2：pending 归属的纯逻辑 --------------------------------

#[test]
fn pending_matches_available_requires_exact_version_match() {
    let available_state = UpdaterState::Available {
        version: "0.3.0".into(),
        notes: None,
        pub_date: None,
    };
    assert!(pending_matches_available(&available_state, Some("0.3.0")));
    assert!(!pending_matches_available(&available_state, Some("0.2.9")));
    assert!(!pending_matches_available(&available_state, None));
}

#[test]
fn pending_matches_available_is_false_for_any_non_available_state() {
    for state in all_non_available_states() {
        assert!(
            !pending_matches_available(&state, Some("anything")),
            "非 Available 状态下 pending 永远谈不上匹配：{state:?}"
        );
    }
}

#[test]
fn should_retain_pending_only_when_available() {
    assert!(should_retain_pending(&UpdaterState::Available {
        version: "1".into(),
        notes: None,
        pub_date: None,
    }));
    for state in all_non_available_states() {
        assert!(
            !should_retain_pending(&state),
            "非 Available 状态都不该保留 pending：{state:?}"
        );
    }
}

// --- U3 返工 P2-3：marker 写失败必须清暂存、绝不进 Ready ----------------

#[test]
fn finalize_marker_success_never_calls_cleanup() {
    let cleanup_called = std::cell::Cell::new(false);
    let outcome = finalize_marker(
        || Ok(()),
        || {
            cleanup_called.set(true);
            Ok(())
        },
    );
    assert_eq!(outcome, MarkerOutcome::Written);
    assert!(!cleanup_called.get(), "marker 写成功不该触发清理");
}

#[test]
fn finalize_marker_write_failure_cleans_up_and_never_reports_written() {
    let cleanup_called = std::cell::Cell::new(false);
    let outcome = finalize_marker(
        || Err("disk full".to_string()),
        || {
            cleanup_called.set(true);
            Ok(())
        },
    );
    assert!(cleanup_called.get(), "marker 写失败必须触发暂存清理");
    match outcome {
        MarkerOutcome::Failed {
            write_error,
            cleanup_ok,
        } => {
            assert_eq!(write_error, "disk full");
            assert!(cleanup_ok);
        }
        MarkerOutcome::Written => {
            panic!("marker 写失败绝不能报 Written（也就是绝不能进 Ready）")
        }
    }
}

#[test]
fn finalize_marker_write_failure_and_cleanup_failure_both_surface() {
    let outcome = finalize_marker(
        || Err("disk full".to_string()),
        || Err("cleanup also failed".to_string()),
    );
    match outcome {
        MarkerOutcome::Failed {
            write_error,
            cleanup_ok,
        } => {
            assert_eq!(write_error, "disk full");
            assert!(!cleanup_ok);
        }
        MarkerOutcome::Written => panic!("marker 写失败绝不能报 Written"),
    }
}

#[test]
fn old_staging_is_cleaned_and_marker_cleared_before_new_stage() {
    let calls = std::cell::RefCell::new(Vec::new());
    let result = stage_after_old_staging_cleanup(
        true,
        || {
            calls.borrow_mut().push("cleanup");
            Ok(())
        },
        || {
            calls.borrow_mut().push("clear_marker");
            Ok(())
        },
        || {
            calls.borrow_mut().push("stage");
            Ok("staged")
        },
    );
    assert_eq!(result.unwrap(), "staged");
    assert_eq!(*calls.borrow(), ["cleanup", "clear_marker", "stage"]);
}

#[test]
fn old_staging_cleanup_failure_prevents_new_stage() {
    let mut staged = false;
    let result = stage_after_old_staging_cleanup(
        true,
        || Err("old staging cleanup failed".into()),
        || panic!("清理失败不应清 marker"),
        || {
            staged = true;
            Ok(())
        },
    );
    assert!(result.unwrap_err().contains("old staging cleanup failed"));
    assert!(!staged);
}

#[test]
fn old_staging_cleanup_failure_transitions_download_to_error_check() {
    let mut machine = available("0.3.0");
    let (_, gen) = machine.begin_download().unwrap();
    let detail = stage_after_old_staging_cleanup(
        true,
        || Err("old staging cleanup failed".into()),
        || Ok(()),
        || Ok(()),
    )
    .unwrap_err();
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
fn no_old_marker_stages_directly() {
    let mut staged = false;
    stage_after_old_staging_cleanup(
        false,
        || panic!("无旧 marker 不应清理"),
        || panic!("无旧 marker 不应清 marker"),
        || {
            staged = true;
            Ok(())
        },
    )
    .unwrap();
    assert!(staged);
}
