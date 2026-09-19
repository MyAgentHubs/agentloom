#![cfg(test)]

use super::*;

#[test]
fn run_member_reader_completed_exit0_downgrades_error_to_transient_report_note() {
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "base"]);
    let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.txt"), "base\ncompleted work\n").unwrap();

    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'old-error\nerror\ncompleted\n'"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn line_parser(line: &str) -> Vec<AgentEvent> {
        match line {
            "old-error" => vec![AgentEvent::Error {
                message: "superseded transient error".into(),
            }],
            "error" => vec![AgentEvent::Error {
                message: format!("temporary patch hook rejection: {}", "界".repeat(220)),
            }],
            "completed" => vec![AgentEvent::Completed {
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                final_text: Some("retry succeeded".into()),
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }],
            _ => vec![],
        }
    }
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        tmp.path(),
        &base_sha,
        line_parser,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let (last_meta, last_event) = emitted.last().unwrap();
    assert_eq!(last_meta.status_transition, Some(StatusTransition::Done));
    let AgentEvent::Completed {
        result: Some(result),
        ..
    } = last_event
    else {
        panic!("expected Completed with MemberResult, got {last_event:?}");
    };
    assert_eq!(result.status, "done");
    assert_eq!(result.failure_reason, None);
    let note = result
        .risks
        .iter()
        .find(|risk| risk.id == "transient_error")
        .map(|risk| risk.text.as_str())
        .expect("Done MemberResult should retain the transient error note");
    let error = note
        .strip_prefix("transient_errors: ")
        .expect("transient note should carry a stable report label");
    assert_eq!(error.chars().count(), 200);
    assert!(error.ends_with('…'));
    assert!(!note.contains("superseded transient error"));

    let conn = crate::test_support::mem_db();
    assert!(
        persist_member_result_message(&conn, "s1", "run1", "agent-claude", "Claude", result,)
            .unwrap()
    );
    let messages = crate::db::get_messages(&conn, "s1").unwrap();
    let crate::db::Block::Text { text: report } = &messages[0].content[0] else {
        panic!("worker report must use a normal text block");
    };
    assert!(report.contains(note), "ledger report should retain: {note}");
    assert!(!report.contains("failure_reason:"));
}

// (4) 非零退出 → Failed（codex P1-6）
#[test]
fn run_member_reader_nonzero_exit_is_failed() {
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'x\\n'; exit 3"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn line_parser(s: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::TextDelta { text: s.into() }]
    }
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        line_parser,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    assert_eq!(
        emitted.last().unwrap().0.status_transition,
        Some(StatusTransition::Failed)
    );
}

#[test]
fn run_member_reader_nonzero_exit_surfaces_stderr_tail() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let old_home = std::env::var_os("HOME");
    let temp_home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", temp_home.path());

    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'insufficient quota\\n' >&2; exit 3"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn line_parser(_s: &str) -> Vec<AgentEvent> {
        vec![]
    }
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        line_parser,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let error_message = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Error { message } => Some(message.as_str()),
            _ => None,
        })
        .expect("nonzero CLI exit should emit a visible error");
    assert!(
        error_message.contains("insufficient quota"),
        "{error_message}"
    );
    let failure_reason = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result
                .as_ref()
                .and_then(|result| result.failure_reason.as_deref()),
            _ => None,
        })
        .expect("member result should persist the CLI failure reason");
    assert!(
        failure_reason.contains("insufficient quota"),
        "{failure_reason}"
    );
    // P2-7 钉子：Failed 终态才该带 exit_code/stderr_tail 诊断素材。
    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("should synthesize a MemberResult");
    assert_eq!(result.exit_code, Some(3));
    assert!(
        result
            .stderr_tail
            .as_deref()
            .is_some_and(|s| s.contains("insufficient quota")),
        "{:?}",
        result.stderr_tail
    );

    match old_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

/// P1 钉子（洞①·退出码契约）：harness 解析层见过 `run.blocked`（myagent 契约退出码 3
/// 的正常收工）时，member 收尾必须诚实措辞（含「不是环境故障」），绝不再合成
/// 「请检查 CLI 登录、额度、模型和网络」那条环境假错误——这条误导过真实用户
/// （GLM/myagent worker 退出码 4、零 stderr 案例）。
#[test]
fn run_member_reader_harness_blocked_exit3_is_honest_not_environment_failure() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.blocked",
        "payload": { "reason": "blocked_questions" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 3"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    assert_eq!(
        emitted.last().unwrap().0.status_transition,
        Some(StatusTransition::Failed),
        "队员没完成任务终究是 Failed（不新造状态枚举）"
    );
    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("blocked 收工也该带 result（不许回退到零原因）");
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("blocked 收工也该带 failure_reason（不许回退到零原因）");
    assert!(
        failure_reason.contains("不是环境故障"),
        "应诚实标注非环境故障：{failure_reason}"
    );
    assert!(
        !failure_reason.contains("请检查 CLI 登录"),
        "harness saw_blocked 不该再合成假环境故障文案：{failure_reason}"
    );
    // D3（delta 复审）：本刀核心机制——按真实 saw_blocked/saw_needs_decision 判 stalled
    // vs env——之前后端一条测试都没盖到 failure_kind 字段本身，改值/改 None 全绿。
    assert_eq!(result.failure_kind.as_deref(), Some("stalled"));
}

/// P1 钉子：同上，覆盖 NeedsDecision（scope_change·契约退出码 4）分支。
#[test]
fn run_member_reader_harness_needs_decision_exit4_is_honest_not_environment_failure() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "scope_change",
            "changes": [{
                "proposal_id": "p1",
                "kind": "scope",
                "detail": { "text": "把后端接口也纳入改动" }
            }]
        },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("needs_decision 收工也该带 result（不许回退到零原因）");
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("needs_decision 收工也该带 failure_reason（不许回退到零原因）");
    assert!(
        failure_reason.contains("不是环境故障"),
        "应诚实标注非环境故障：{failure_reason}"
    );
    assert!(
        !failure_reason.contains("请检查 CLI 登录"),
        "harness saw_needs_decision 不该再合成假环境故障文案：{failure_reason}"
    );
    // D3（delta 复审）：同上——盖住 saw_needs_decision → failure_kind="stalled" 这条腿。
    assert_eq!(result.failure_kind.as_deref(), Some("stalled"));
}

/// 本刀钉子：budget_exhausted_still_progressing（harness 触发·白名单内）必须走新的
/// "budget_exhausted" failure_kind，诚实文案里不能出现 stalled 那句「有问题在等回答，
/// 或执行被阻塞」（对「预算耗尽但仍在推进」是谎报——它没卡住，也没有问题在等回答）。
#[test]
fn run_member_reader_harness_budget_exhausted_still_progressing_routes_to_budget_exhausted_kind() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "blocked_questions",
            "blocked_reason": "budget_exhausted_still_progressing",
            "trigger": "harness",
        },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("budget_exhausted 收工也该带 result（不许回退到零原因）");
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("budget_exhausted 收工也该带 failure_reason（不许回退到零原因）");
    assert!(
            !failure_reason.contains("有问题在等回答，或执行被阻塞"),
            "budget_exhausted 不该沿用 stalled 那句「有问题在等回答，或执行被阻塞」措辞：{failure_reason}"
        );
    assert!(
        !failure_reason.contains("question pending"),
        "budget_exhausted 不该沿用 stalled 的英文措辞：{failure_reason}"
    );
    assert!(
        failure_reason.contains("预算") || failure_reason.contains("budget"),
        "budget_exhausted 文案应点明预算耗尽：{failure_reason}"
    );
    assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
}

/// 防扩面回归：no_progress（同一白名单、同样 harness 触发）不在本刀分流范围内，必须仍走
/// 老的 "stalled" 桶——别把白名单里其余两种（no_progress/stuck_repeating）顺手扩进
/// budget_exhausted。
#[test]
fn run_member_reader_harness_no_progress_still_routes_to_stalled_kind() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "blocked_questions",
            "blocked_reason": "no_progress",
            "trigger": "harness",
        },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("no_progress 收工也该带 result（不许回退到零原因）");
    assert_eq!(
        result.failure_kind.as_deref(),
        Some("stalled"),
        "no_progress 不属于本刀分流范围，必须仍走 stalled 老路（防扩面回归）"
    );
}

/// 对抗审补丁回归钉子：一个 run 里先收到带结构化 reason 的 budget_exhausted
/// Blocked（NeedsDecision），随后又收到一条 run.interrupted（同样是 Blocked 事件，
/// message 非空，但 agent_event.rs 对这条协议路径恒填 reason=None）——旧写法拿
/// `!message.trim().is_empty()` 当 blocked_reason 的更新 guard，后到的 None 会把
/// 已经拿到的 budget_exhausted 抹掉，误降回 "stalled"（reviewer 探针实证）。改成
/// blocked_reason 非空 wins 后，这里必须仍是 "budget_exhausted"。
#[test]
fn run_member_reader_budget_exhausted_then_run_interrupted_reason_survives() {
    let needs_decision_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "blocked_questions",
            "blocked_reason": "budget_exhausted_still_progressing",
            "trigger": "harness",
        },
    })
    .to_string();
    let interrupted_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.interrupted",
        "payload": {},
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$INTERRUPTED_LINE\"; exit 3",
        ])
        .env("NEEDS_DECISION_LINE", &needs_decision_line)
        .env("INTERRUPTED_LINE", &interrupted_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert_eq!(
        result.failure_kind.as_deref(),
        Some("budget_exhausted"),
        "后到的 run.interrupted（reason=None）不该抹掉先前 budget_exhausted 的结构化 reason"
    );
}

/// 本刀钉子（第四类·context_exhausted）：单轮上下文（token）预算耗尽——payload 没有
/// blocked_reason/trigger 字段，顶层 reason 直接是硬编码字面量
/// "context_budget_exhausted"（真实 emit 点：harness-agent run_loop.rs 的
/// fit_to_budget 溢出分支，发生在模型这一轮被调用之前）。必须走新的 "context_exhausted"
/// failure_kind，诚实文案里既不能出现 stalled 那句「有问题在等回答，或执行被阻塞」，也
/// 不能出现 budget_exhausted 那句「在正常推进」/「可以再派一单接着干」（没有推进证据，
/// 原样重派大概率再死——这是它跟 budget_exhausted 文案的关键差别）。
#[test]
fn run_member_reader_harness_context_budget_exhausted_routes_to_context_exhausted_kind() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "context_budget_exhausted",
            "turn": 3,
            "estimate_tokens": 200_000,
            "budget_tokens": 180_000,
            "next_step": "拆小任务 / 换更大上下文的模型",
        },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("context_exhausted 收工也该带 result（不许回退到零原因）");
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("context_exhausted 收工也该带 failure_reason（不许回退到零原因）");
    assert!(
            !failure_reason.contains("有问题在等回答，或执行被阻塞"),
            "context_exhausted 不该沿用 stalled 那句「有问题在等回答，或执行被阻塞」措辞：{failure_reason}"
        );
    assert!(
        !failure_reason.contains("question pending"),
        "context_exhausted 不该沿用 stalled 的英文措辞：{failure_reason}"
    );
    assert!(
            !failure_reason.contains("在正常推进") && !failure_reason.contains("normal progress"),
            "context_exhausted 没有推进证据，不该沿用 budget_exhausted 那句「在正常推进」：{failure_reason}"
        );
    assert!(
        !failure_reason.contains("再派一单接着干")
            && !failure_reason.contains("dispatch another task to continue"),
        "context_exhausted 不该建议原样续派（大概率再死）：{failure_reason}"
    );
    assert!(
        failure_reason.contains("上下文") || failure_reason.contains("context"),
        "context_exhausted 文案应点明上下文窗口装不下：{failure_reason}"
    );
    assert_eq!(result.failure_kind.as_deref(), Some("context_exhausted"));
}

/// 伪造面探针：agent 主动触发 block_with_questions 时，把 blocked_reason 字面写成
/// "context_budget_exhausted" 来碰瓷（trigger="agent"）——顶层 reason 仍恒是硬编码的
/// "blocked_questions"（模型自由文本进不了顶层 reason 字段），所以这里必须仍落回老的
/// "stalled" 桶，不能被误判成 context_exhausted。
#[test]
fn run_member_reader_agent_triggered_lookalike_cannot_forge_context_exhausted_kind() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "blocked_questions",
            "blocked_reason": "context_budget_exhausted",
            "trigger": "agent",
            "questions": ["要不要继续？"],
        },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert_eq!(
        result.failure_kind.as_deref(),
        Some("stalled"),
        "agent 自触发 block_with_questions 即便 blocked_reason 字面碰瓷 \
             context_budget_exhausted，也不能伪造出 context_exhausted——顶层 reason 恒 \
             \"blocked_questions\"，模型没有输入通道能改写它"
    );
}

/// P2-6 钉子：harness_blocked_message 已经把真实缘由渲成人话了——终态 failure_reason
/// 应该把它拼进去，用户不用自己翻 trace 才知道具体卡在哪。
#[test]
fn run_member_reader_harness_blocked_message_appends_real_harness_reason() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.blocked",
        "payload": { "reason": "waiting_for_credentials" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 3"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
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
        .expect("blocked 收工也该带 failure_reason");
    assert!(failure_reason.contains("不是环境故障"), "{failure_reason}");
    assert!(
        failure_reason.contains("waiting_for_credentials"),
        "真实 harness 缘由该拼进去、别让用户自己翻 trace：{failure_reason}"
    );
}

/// P2-6 钉子：`run.interrupted` 也走 Blocked 事件（saw_blocked=true），但那是「运行被
/// 中断」而不是「有问题在等回答」——泛化框架文案本身措辞不准，靠拼上真实 harness 文案
/// （里面明说「运行已中断」）把中断跟真正等回答的停摆区分开。
#[test]
fn run_member_reader_harness_interrupted_appends_run_interrupted_wording() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.interrupted",
        "payload": {},
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 3"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
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
        .expect("interrupted 收工也该带 failure_reason");
    assert!(
            failure_reason.contains("运行已中断"),
            "run.interrupted 该带上真实「运行已中断」措辞，别只留泛化的「被阻塞」框架：{failure_reason}"
        );
}

/// D6 钉子（delta 复审·实证反例）：agent 自报 Error 是最常见的失败形态——harness 先发
/// run.blocked（saw_blocked=true）再发 run.failed（saw_error=true，failure_reason
/// 被 agent 自己的 Error 抢先填非空）——这明明是诚实停摆（有问题在等回答），却因为
/// failure_kind 曾经嵌在「要不要合成兜底文案」那个 `if failure_reason.is_none()` 分支
/// 里、被 agent 抢跑的 Error 短路掉，落不到 stalled，前端最终会显示成「env 环境故障」。
/// 这条钉住修复：即便 agent 抢先报了 Error，只要真见过 Blocked，failure_kind 仍是
/// "stalled"（failure_reason 会是 agent 自己的 Error 文本，不是本刀合成的诚实句子——
/// 这也证明 failure_kind 判定确实跟「消息合成」解耦了，不是从文案里反推）。
#[test]
fn run_member_reader_harness_blocked_then_agent_reported_error_still_stalled() {
    let blocked_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.blocked",
        "payload": { "reason": "blocked_questions" },
    })
    .to_string();
    let failed_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "agent 自报：等待用户回答后中止" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s\\n' \"$BLOCKED_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 3",
        ])
        .env("BLOCKED_LINE", &blocked_line)
        .env("FAILED_LINE", &failed_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    // failure_reason 是 agent 自己报的 Error 文本（没被本刀的诚实合成覆盖）——
    // 证明 failure_kind 判定不是从 failure_reason 文案里反推出来的。
    assert_eq!(
        result.failure_reason.as_deref(),
        Some("agent 自报：等待用户回答后中止")
    );
    assert_eq!(
        result.failure_kind.as_deref(),
        Some("stalled"),
        "agent 抢先报 Error 不该抹掉之前真实见过的 Blocked 事件"
    );
}
