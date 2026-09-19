#![cfg(test)]

use super::*;

#[test]
fn member_transport_preserves_dispatch_and_flushes_terminal_last() {
    let root = tempfile::tempdir().unwrap();
    let transport = crate::event_transport::EventTransport::new_for_test(root.path().to_path_buf());
    let payloads = Arc::new(Mutex::new(Vec::new()));
    let recorded = payloads.clone();
    transport.install_emitter_for_test(move |payload| recorded.lock().unwrap().push(payload));
    let spec = spec();
    let lane_id = register_member_transport(
        &transport,
        "session-1",
        "run1",
        &spec,
        TextGranularity::Line,
        false,
    )
    .unwrap();
    let mut pending = Vec::new();
    let (open_meta, open_event) = member_open_event("run1", &spec);
    emit_member_transport_event(
        &transport,
        &lane_id,
        &mut pending,
        open_meta.clone(),
        open_event,
    );
    let stream_meta = member_dispatch_meta("run1", &spec, None);
    emit_member_transport_event(
        &transport,
        &lane_id,
        &mut pending,
        stream_meta.clone(),
        AgentEvent::TextDelta {
            text: "answer".into(),
        },
    );
    emit_member_transport_event(
        &transport,
        &lane_id,
        &mut pending,
        stream_meta.clone(),
        AgentEvent::Error {
            message: "provider failed".into(),
        },
    );
    let (terminal_meta, terminal_event) =
        member_terminal_event("run1", &spec, None, StatusTransition::Failed, None, None);
    emit_member_transport_event(
        &transport,
        &lane_id,
        &mut pending,
        terminal_meta.clone(),
        terminal_event,
    );

    let payloads = payloads.lock().unwrap();
    assert_eq!(
        payloads.len(),
        1,
        "member terminal uses one barrier payload"
    );
    let batches = &payloads[0].batches;
    assert_eq!(batches.len(), 3);
    assert_eq!(batches[0].dispatch, Some(open_meta));
    assert_eq!(batches[1].dispatch, Some(stream_meta));
    assert_eq!(batches[2].dispatch, Some(terminal_meta));
    assert!(matches!(
        batches.last().unwrap().events.last().unwrap().event,
        AgentEvent::Completed { .. }
    ));
}

#[test]
fn member_dispatch_meta_tags_run_assignment_participant() {
    let m = member_dispatch_meta("run1", &spec(), None);
    assert_eq!(m.run_id.as_deref(), Some("run1"));
    assert_eq!(m.assignment_id.as_deref(), Some("run1-a1"));
    assert_eq!(m.origin_participant_id.as_deref(), Some("worker-1"));
    assert_eq!(m.member_name.as_deref(), Some("Claude"));
    assert_eq!(m.task_id.as_deref(), Some("run1-task-1"));
    assert!(m.status_transition.is_none());
    assert!(m.task_pack.is_none());
}

#[test]
fn member_dispatch_meta_carries_status_transition_on_terminal() {
    let m = member_dispatch_meta("run1", &spec(), Some(StatusTransition::Done));
    assert_eq!(m.status_transition, Some(StatusTransition::Done));
    assert_eq!(m.assignment_id.as_deref(), Some("run1-a1"));
}

#[test]
fn team_goal_event_is_goal_declared_with_lead_and_run_scope_meta() {
    let (meta, ev) = team_goal_event("run1", "实现 stage 2", "Claude", &[]);
    // 开场事件只挂 run_id（不挂 assignment）——与 fake_runner goal_event 一致
    assert_eq!(meta.run_id.as_deref(), Some("run1"));
    assert!(meta.assignment_id.is_none());
    match ev {
        AgentEvent::GoalDeclared {
            goal,
            status,
            lead,
            criteria,
        } => {
            assert_eq!(goal, "实现 stage 2");
            assert_eq!(status, "frozen");
            assert_eq!(lead.as_deref(), Some("Claude"));
            assert!(
                criteria.is_empty(),
                "M1b 无 Plan&Acceptance Gate → criteria 空（M2 填）"
            );
        }
        other => panic!("expected GoalDeclared, got {other:?}"),
    }
}

// codex P1-4：队员开场事件必须是 Dispatched + TextDelta(subtask)——否则前端
// teamReducer 不填 m.sub（teamReducer.ts:137 只认 dispatched 的 text_delta）、卡片缺子任务。
#[test]
fn member_open_event_uses_short_subtask_not_full_prompt() {
    let mut s = spec();
    s.subtask = "看下 AI News".into();
    s.prompt = "## 总目标\n一大坨 TaskPack\n## 你的子任务\n看下 AI News\n".into();
    let (meta, ev) = member_open_event("run1", &s);
    assert_eq!(meta.assignment_id.as_deref(), Some("run1-a1"));
    assert_eq!(meta.status_transition, Some(StatusTransition::Dispatched));
    assert_eq!(meta.task_pack.as_deref(), Some(s.prompt.as_str()));
    assert!(meta.task_pack.unwrap().contains("总目标"));
    match ev {
        AgentEvent::TextDelta { text } => {
            assert_eq!(text, "看下 AI News");
            assert!(!text.contains("总目标"));
        }
        other => panic!("expected TextDelta(subtask), got {other:?}"),
    }
}

#[test]
fn member_open_event_orchestrated_field_is_set_by_run_single_worker() {
    // 验证 member_open_event 本身不带 orchestrated（由 run_single_worker 打标）
    let s = MemberSpec {
        participant_id: "p1".into(),
        assignment_id: "a1".into(),
        task_id: "t1".into(),
        agent_id: "ag1".into(),
        provider: "codex".into(),
        agent_name: "Agent1".into(),
        subtask: "do stuff".into(),
        prompt: "do stuff".into(),
    };
    let (meta, _ev) = member_open_event("run1", &s);
    // member_open_event 本身不打标，由调用方 run_single_worker 打
    assert!(
        meta.orchestrated.is_none(),
        "member_open_event 不应自己设 orchestrated"
    );
}

#[test]
fn stamp_orchestrated_sets_orchestrated_flag() {
    let s = MemberSpec {
        participant_id: "p1".into(),
        assignment_id: "a1".into(),
        task_id: "t1".into(),
        agent_id: "ag1".into(),
        provider: "codex".into(),
        agent_name: "Agent1".into(),
        subtask: "do stuff".into(),
        prompt: "do stuff".into(),
    };
    let meta = member_dispatch_meta("run1", &s, None);
    let stamped = stamp_orchestrated(meta);
    assert_eq!(
        stamped.orchestrated,
        Some(true),
        "stamp_orchestrated 应设 orchestrated=true"
    );
}

#[test]
fn emit_terminal_failed_orchestrated_emits_failed_with_correct_meta() {
    let s = spec();
    let mut collected = Vec::new();
    let mut emit = |meta, event| collected.push((meta, event));

    emit_terminal_failed_orchestrated("run1", &s, "spawn 失败：找不到二进制", &mut emit);

    assert_eq!(collected.len(), 1);
    let (meta, event) = &collected[0];
    assert_eq!(meta.orchestrated, Some(true));
    assert_eq!(
        meta.assignment_id.as_deref(),
        Some(s.assignment_id.as_str())
    );
    assert_eq!(meta.status_transition, Some(StatusTransition::Failed));
    // P1 钉子：终态事件必须带非空 failure_reason——零原因路径（洞②）不许回归。
    match event {
        AgentEvent::Completed {
            result: Some(result),
            ..
        } => {
            assert_eq!(
                result.failure_reason.as_deref(),
                Some("spawn 失败：找不到二进制")
            );
        }
        other => panic!("expected Completed with result, got {other:?}"),
    }
}

/// P1 钉子（洞②·零原因路径）：`build_failure_only_member_result` 是
/// `emit_terminal_failed_orchestrated` 与 `emit_single_worker_failure_on_lane_best_effort`
/// 共用的构造点——直接钉住它本身必产非空 failure_reason，两个调用方各自的接线正确性
/// 由上面那条 + 下面 `emit_single_worker_failure_on_lane_best_effort_carries_reason` 分别兜底。
#[test]
fn build_failure_only_member_result_always_carries_reason() {
    let s = spec();
    let result = build_failure_only_member_result(&s, "member.spawnFailed: boom");
    assert_eq!(result.status, "failed");
    assert_eq!(
        result.failure_reason.as_deref(),
        Some("member.spawnFailed: boom")
    );
    assert_eq!(result.failure_kind.as_deref(), Some("env"));
    assert!(result.changed_files.is_empty());
}

#[test]
fn emit_single_worker_failure_on_lane_best_effort_carries_reason() {
    let root = tempfile::tempdir().unwrap();
    let transport = crate::event_transport::EventTransport::new_for_test(root.path().to_path_buf());
    let payloads = Arc::new(Mutex::new(Vec::new()));
    let recorded = payloads.clone();
    transport.install_emitter_for_test(move |payload| recorded.lock().unwrap().push(payload));
    let s = spec();
    let lane_id = register_member_transport(
        &transport,
        "session-1",
        "run1",
        &s,
        TextGranularity::Line,
        true,
    )
    .unwrap();

    emit_single_worker_failure_on_lane_best_effort(
        &transport,
        "session-1",
        "run1",
        &s,
        &lane_id,
        "member.spawnFailed: 找不到二进制",
    );

    let payloads = payloads.lock().unwrap();
    let last_payload = payloads.last().expect("应至少发出一个 payload");
    let last_batch = last_payload.batches.last().expect("应至少一个 batch");
    let last_event = &last_batch.events.last().expect("应至少一个事件").event;
    match last_event {
        AgentEvent::Completed {
            result: Some(result),
            ..
        } => {
            assert_eq!(
                result.failure_reason.as_deref(),
                Some("member.spawnFailed: 找不到二进制"),
                "洞②：spawn/setup 早退 best-effort 终态不许再落 result=None"
            );
        }
        other => panic!("expected Completed with result, got {other:?}"),
    }
}

// (2) 纯函数：终态 Completed 透传真 token + 无 commit 字段（M2）
#[test]
fn member_terminal_event_carries_real_tokens_no_commit_fields() {
    let buffered = Some(AgentEvent::Completed {
        cost_usd: Some(0.12),
        input_tokens: Some(100),
        output_tokens: Some(50),
        final_text: Some("done".into()),
        result: None,
        run_id: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: None,
    });
    let (meta, ev) = member_terminal_event(
        "run1",
        &spec(),
        buffered,
        StatusTransition::Done,
        None,
        None,
    );
    assert_eq!(meta.status_transition, Some(StatusTransition::Done));
    assert_eq!(meta.assignment_id.as_deref(), Some("run1-a1"));
    match ev {
        AgentEvent::Completed {
            input_tokens,
            commit_sha,
            interrupted,
            ..
        } => {
            assert_eq!(input_tokens, Some(100)); // 真 token 透传
            assert!(commit_sha.is_none()); // 无 auto-commit（M2）
            assert_eq!(interrupted, Some(false));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[test]
fn member_terminal_event_carries_result_and_aggregates() {
    let changed_files = vec![
        ChangedFile {
            path: "src/lib.rs".into(),
            insertions: 3,
            deletions: 1,
        },
        ChangedFile {
            path: "README.md".into(),
            insertions: 5,
            deletions: 0,
        },
    ];
    let anchor = ResultAnchor {
        base_sha: "base123".into(),
        head_sha: None,
        diff_ref: None,
        generated_from: "worktree_diff".into(),
    };
    let result = build_member_result(
        &spec(),
        StatusTransition::Done,
        changed_files,
        anchor,
        vec![],
        None,
    );

    let (_meta, ev) = member_terminal_event(
        "run1",
        &spec(),
        None,
        StatusTransition::Done,
        Some(result),
        None,
    );

    match ev {
        AgentEvent::Completed {
            result,
            files_changed,
            insertions,
            deletions,
            commit_sha,
            ..
        } => {
            let result = result.expect("Completed.result should carry MemberResult");
            assert_eq!(result.changed_files.len(), 2);
            assert_eq!(files_changed, Some(2));
            assert_eq!(insertions, Some(8));
            assert_eq!(deletions, Some(1));
            assert_eq!(commit_sha, None);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[test]
fn member_terminal_event_reports_session_head_sha() {
    let (_meta, ev) = member_terminal_event(
        "r1",
        &spec(),
        None,
        StatusTransition::Done,
        None,
        Some("deadbeef".into()),
    );
    match ev {
        AgentEvent::Completed { commit_sha, .. } => {
            assert_eq!(commit_sha.as_deref(), Some("deadbeef"))
        }
        _ => panic!("应 Completed"),
    }
}
