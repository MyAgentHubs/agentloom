#![cfg(test)]

use super::*;

#[test]
fn member_failure_messages_keep_zh_and_render_en() {
    assert_eq!(
        stage1_failure_message(crate::Locale::Zh, Stage1Failure::DirtyTail("branch-a")),
        "Stage① 接力失败：worker 自 commit 但留未提交脏尾·改动未落地会话（member=branch-a）"
    );
    assert_eq!(
            stage1_failure_message(crate::Locale::En, Stage1Failure::DirtyTail("branch-a")),
            "Stage 1 relay failed: worker committed changes but left an uncommitted dirty tail; changes were not relayed to the session (member=branch-a)"
        );
    assert_eq!(
        stage1_failure_message(crate::Locale::Zh, Stage1Failure::Finalize("io error")),
        "Stage① 接力失败：git 状态不可接力·改动仍留在 member 工作区：io error"
    );
    assert_eq!(
            stage1_failure_message(crate::Locale::En, Stage1Failure::Finalize("io error")),
            "Stage 1 relay failed: git state cannot be relayed; changes remain in the member workspace: io error"
        );
    assert_eq!(
        stage1_failure_message(crate::Locale::Zh, Stage1Failure::NotFastForward("branch-a")),
        "Stage① 接力失败：非 ff（会话 tip 已前移·stale base·member=branch-a）"
    );
    assert_eq!(
            stage1_failure_message(
                crate::Locale::En,
                Stage1Failure::NotFastForward("branch-a")
            ),
            "Stage 1 relay failed: non-fast-forward (session tip advanced; stale base; member=branch-a)"
        );
    assert_eq!(
        stage1_failure_message(crate::Locale::Zh, Stage1Failure::SessionMerge("rejected")),
        "Stage① 接力失败：session-merge 拒合（fail-closed）：rejected"
    );
    assert_eq!(
        stage1_failure_message(crate::Locale::En, Stage1Failure::SessionMerge("rejected")),
        "Stage 1 relay failed: session merge rejected (fail-closed): rejected"
    );
    assert_eq!(
        blocking_write_failure_message(crate::Locale::Zh, "permission denied"),
        "worker 干净退出但未产生任何文件改动，且输出含失败标记：permission denied"
    );
    assert_eq!(
            blocking_write_failure_message(crate::Locale::En, "permission denied"),
            "Worker exited cleanly without producing any file changes, and its output contained a failure marker: permission denied"
        );
}

/// 本刀新增：`overridden_error_lead_in` 双语引导词直接断言——只贴在「被抢占的引擎 Error
/// 原文追加段」前面，不动 `blocked_message` 那段（那段保持裸拼，见函数文档跨刀契约）。
#[test]
fn overridden_error_lead_in_renders_zh_and_en() {
    assert_eq!(overridden_error_lead_in(crate::Locale::Zh), "引擎另报：");
    assert_eq!(
        overridden_error_lead_in(crate::Locale::En),
        "Engine also reported: "
    );
}

#[test]
fn member_failure_reason_keeps_worker_start_locale_snapshot() {
    let ui_locale = crate::UiLocale::default();
    assert_eq!(*ui_locale.0.read().unwrap(), crate::Locale::Zh);

    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'permission denied\n'"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let locale_snapshot = *ui_locale.0.read().unwrap();
    *ui_locale.0.write().unwrap() = crate::Locale::En;
    assert_eq!(*ui_locale.0.read().unwrap(), crate::Locale::En);

    fn line_parser(s: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::TextDelta { text: s.into() }]
    }
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader_for_locale(
        child,
        None,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        line_parser,
        None,
        locale_snapshot,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let failure_reason = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result
                .as_ref()
                .and_then(|result| result.failure_reason.as_deref()),
            _ => None,
        })
        .expect("blocking marker should freeze a failure_reason");
    assert_eq!(
        failure_reason,
        "worker 干净退出但未产生任何文件改动，且输出含失败标记：permission denied"
    );
}

#[test]
fn auth_retry_member_recovers_after_transient_401() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("attempts");
    let command = auth_retry_member_command(
        &marker,
        "Failed to authenticate. API Error: 401 Invalid authentication credentials",
    );
    let tr = TeamRunning::default();
    tr.init_run("run1", 1);
    let mut emitted = Vec::new();

    let result = run_single_worker_inner(
        &tr,
        "s1",
        "run1",
        spec(),
        command,
        crate::agent_event::parse_claude_line,
        TextGranularity::Line,
        tmp.path().to_path_buf(),
        String::new(),
        &mut |dispatch, event| emitted.push((dispatch, event)),
        None,
    )
    .expect("auth retry should return the recovered member result");

    assert_eq!(std::fs::read_to_string(marker).unwrap().trim(), "2");
    assert_eq!(result.status, "done");
    assert_eq!(result.final_text_ref.as_deref(), Some("recovered"));
    assert_eq!(
        emitted.last().unwrap().0.status_transition,
        Some(StatusTransition::Done)
    );
    assert!(!emitted
        .iter()
        .any(|(_, event)| matches!(event, AgentEvent::Error { .. })));
}

#[test]
fn auth_retry_member_skips_non_auth_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("attempts");
    let command = auth_retry_member_command(&marker, "connection refused");
    let tr = TeamRunning::default();
    tr.init_run("run1", 1);
    let mut emitted = Vec::new();

    let result = run_single_worker_inner(
        &tr,
        "s1",
        "run1",
        spec(),
        command,
        crate::agent_event::parse_claude_line,
        TextGranularity::Line,
        tmp.path().to_path_buf(),
        String::new(),
        &mut |dispatch, event| emitted.push((dispatch, event)),
        None,
    )
    .expect("non-auth process failure should still produce a member result");

    assert_eq!(std::fs::read_to_string(marker).unwrap().trim(), "1");
    assert_eq!(result.status, "failed");
    assert_eq!(result.failure_reason.as_deref(), Some("connection refused"));
    assert_eq!(
        emitted.last().unwrap().0.status_transition,
        Some(StatusTransition::Failed)
    );
}

/// 探针 F 连带收益钉子（本刀顺手修好·此前零覆盖）：401 auth Error 后面紧跟一条空串
/// Error——旧写法（`failure_reason` 无条件覆盖）下，第二条空串会把已经记下的 401 文本
/// 抹成 None，`run_single_worker_attempt_loop` 里判断要不要重试的 `auth_failed` 闸门
/// （约 2175-2178 行：`terminal_status(...) == Failed && attempt.failure_reason.as_deref()
/// .is_some_and(is_auth_error)`）拿到的是 None，`is_some_and` 恒假，整条 401 自动重试
/// 链路被空串尾巴打断——重试根本不会触发（旧写法下会静默直接判 Failed，spawn 计数停在
/// 1，不会有第二次尝试）。本刀改成「非空 wins」后，空串不再抹掉 401 原文，`auth_failed`
/// 判据正常命中，重试正常触发并在第二次尝试里恢复成功。
#[test]
fn auth_retry_member_recovers_after_401_with_trailing_empty_error() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("attempts");
    let command = auth_retry_member_command_with_trailing_empty_error(
        &marker,
        "Failed to authenticate. API Error: 401 Invalid authentication credentials",
    );
    let tr = TeamRunning::default();
    tr.init_run("run1", 1);
    let mut emitted = Vec::new();

    let result = run_single_worker_inner(
        &tr,
        "s1",
        "run1",
        spec(),
        command,
        crate::agent_event::parse_claude_line,
        TextGranularity::Line,
        tmp.path().to_path_buf(),
        String::new(),
        &mut |dispatch, event| emitted.push((dispatch, event)),
        None,
    )
    .expect("401 + 尾随空串 Error 仍应触发重试并恢复");

    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "2",
        "尾随空串 Error 不该打断 401 自动重试链路：spawn 计数应为 2（首次失败 + 重试一次）"
    );
    assert_eq!(result.status, "done");
    assert_eq!(result.final_text_ref.as_deref(), Some("recovered"));
    assert_eq!(
        emitted.last().unwrap().0.status_transition,
        Some(StatusTransition::Done)
    );
}
