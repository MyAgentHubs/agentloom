#![cfg(test)]

use super::*;
use crate::agent_event::{
    AgentEvent, CardKind, ChangedFile, GoalCriterion, ResultAnchor, StatusTransition, ToolStatus,
};

fn strip_comments_and_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'/') {
            while let Some(&nc) = chars.peek() {
                if nc == '\n' {
                    break;
                }
                chars.next();
            }
            out.push_str("/*stripped-comment*/");
        } else if c == '"' {
            while let Some(nc) = chars.next() {
                if nc == '\\' {
                    chars.next();
                    continue;
                }
                if nc == '"' {
                    break;
                }
            }
            out.push_str("\"stripped-string\"");
        } else {
            out.push(c);
        }
    }
    out
}

fn extract_fn_body<'a>(stripped_source: &'a str, fn_needle: &str, label: &str) -> &'a str {
    let after_sig = stripped_source.split(fn_needle).nth(1).unwrap_or_else(|| {
        panic!("{label}: 源码里没找到 {fn_needle:?}，测试的切片标记可能已经过期")
    });
    let open_rel = after_sig
        .find('{')
        .unwrap_or_else(|| panic!("{label}: {fn_needle:?} 后面没找到函数体开头的 `{{`"));
    let from_open = &after_sig[open_rel..];
    let mut depth: i32 = 0;
    for (i, c) in from_open.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &from_open[..=i];
                }
            }
            _ => {}
        }
    }
    panic!("{label}: {fn_needle:?} 的函数体没扫到匹配的收尾 `}}`，测试的切片标记可能已经过期");
}

fn extract_call<'a>(text: &'a str, call_needle: &str, label: &str) -> &'a str {
    let start = text.find(call_needle).unwrap_or_else(|| {
        panic!("{label}: 源码里没找到调用 {call_needle:?}，测试的切片标记可能已经过期")
    });
    let from_call = &text[start..];
    let open_rel = from_call
        .find('(')
        .unwrap_or_else(|| panic!("{label}: {call_needle:?} 后面没找到调用参数开头"));
    let mut depth: i32 = 0;
    for (i, c) in from_call[open_rel..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &from_call[..open_rel + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("{label}: {call_needle:?} 的调用没扫到匹配的收尾 `)`，测试的切片标记可能已经过期");
}

fn assert_lock_scope_closed_before_marker(
    fn_body: &str,
    lock_marker: &str,
    marker: &str,
    label: &str,
) {
    let marker_idx = fn_body.find(marker).unwrap_or_else(|| {
        panic!("{label}: 函数体里没找到 {marker:?}，测试的切片标记可能已经过期")
    });
    let mut lock_positions = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = fn_body[cursor..].find(lock_marker) {
        let idx = cursor + rel;
        if idx >= marker_idx {
            break;
        }
        lock_positions.push(idx);
        cursor = idx + lock_marker.len();
    }
    assert!(
        !lock_positions.is_empty(),
        "{label}: {marker:?} 之前没找到任何 {lock_marker:?}，测试的切片标记可能已经过期"
    );

    for &lock_idx in &lock_positions {
        let mut depth: i32 = 0;
        let mut release_idx: Option<usize> = None;
        for (i, c) in fn_body[lock_idx..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    if depth == 0 {
                        release_idx = Some(lock_idx + i);
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        let release_idx = release_idx.unwrap_or_else(|| {
            panic!("{label}: 字节 {lock_idx} 处的 {lock_marker:?} 往后没扫到把它包住的 block 收尾")
        });
        assert!(
                release_idx < marker_idx,
                "{label}: {lock_marker:?} 所在 block 到字节 {release_idx} 才收尾，晚于 {marker:?}@{marker_idx}"
            );
    }
}

fn spec() -> MemberSpec {
    MemberSpec {
        participant_id: "worker-1".into(),
        assignment_id: "run1-a1".into(),
        task_id: "run1-task-1".into(),
        agent_id: "agent-claude".into(),
        provider: "codex".into(),
        agent_name: "Claude".into(),
        subtask: "实现 X".into(),
        prompt: "## 总目标\n测试目标\n## 你的子任务\n实现 X\n".into(),
    }
}

fn auth_retry_member_command(marker: &std::path::Path, first_error: &str) -> Command {
    let script = format!(
        r#"count=0
if [ -f "$1" ]; then count=$(sed -n '1p' "$1"); fi
count=$((count + 1))
printf '%s\n' "$count" > "$1"
if [ "$count" -eq 1 ]; then
  printf '%s\n' '{}'
else
  printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"recovered"}}'
fi"#,
        serde_json::json!({"type": "result", "is_error": true, "result": first_error})
    );
    let mut command = Command::new("/bin/sh");
    command.args(["-c", &script, "auth-retry"]);
    command.arg(marker);
    command
}

fn auth_retry_member_command_with_trailing_empty_error(
    marker: &std::path::Path,
    first_error: &str,
) -> Command {
    let script = format!(
        r#"count=0
if [ -f "$1" ]; then count=$(sed -n '1p' "$1"); fi
count=$((count + 1))
printf '%s\n' "$count" > "$1"
if [ "$count" -eq 1 ]; then
  printf '%s\n' '{}'
  printf '%s\n' '{}'
else
  printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"recovered"}}'
fi"#,
        serde_json::json!({"type": "result", "is_error": true, "result": first_error}),
        serde_json::json!({"type": "result", "is_error": true, "result": ""})
    );
    let mut command = Command::new("/bin/sh");
    command.args(["-c", &script, "auth-retry-trailing-empty"]);
    command.arg(marker);
    command
}

fn member_input(agent_id: &str) -> MemberInput {
    MemberInput {
        participant_id: format!("participant-{agent_id}"),
        assignment_id: format!("assignment-{agent_id}"),
        task_id: format!("task-{agent_id}"),
        agent_id: agent_id.to_string(),
        subtask: "实现 X".to_string(),
        goal_title: None,
    }
}

fn agent_profile(id: &str, cap_lead: Option<&str>) -> crate::db::AgentProfile {
    crate::db::AgentProfile {
        id: id.to_string(),
        name: format!("Agent {id}"),
        access: "borrow".to_string(),
        provider: "claude".to_string(),
        primary_model: Some("claude-test".to_string()),
        endpoint: Some("https://api.example.test/v1".to_string()),
        auth_mode: Some("bearer".to_string()),
        model_opus: None,
        model_sonnet: None,
        model_haiku: None,
        model_subagent: None,
        reasoning_default: "auto".to_string(),
        max_output_tokens: None,
        api_timeout_ms: None,
        compat_disable_betas: false,
        compat_disable_nonessential: false,
        compat_disable_thinking: false,
        compat_proxy: None,
        custom_headers: None,
        extra_body: None,
        cap_reasoning: None,
        cap_computer_use: None,
        cap_lead: cap_lead.map(str::to_string),
        has_key: true,
        is_builtin: false,
        enabled: true,
        sort_order: 0,
        created_at: 100,
        updated_at: 100,
    }
}

fn member_pool_conn() -> rusqlite::Connection {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    crate::db::upsert_agent(&conn, &agent_profile("lead-a", Some("native_cli"))).unwrap();
    crate::db::upsert_agent(&conn, &agent_profile("worker-a", None)).unwrap();
    crate::db::upsert_agent(&conn, &agent_profile("worker-b", None)).unwrap();
    conn
}

/// H1/A2 测试专用：access="native" 不需要钥匙串，避免测试触达真实 macOS Keychain。
fn native_member_profile(id: &str) -> crate::db::AgentProfile {
    let mut p = agent_profile(id, None);
    p.access = "native".to_string();
    p.provider = "codex".to_string();
    p.primary_model = None;
    p.endpoint = None;
    p.auth_mode = None;
    p
}

fn in_place_team_conn(
    namespace_id: &str,
    repo_id: &str,
    session_id: &str,
) -> (rusqlite::Connection, tempfile::TempDir, std::path::PathBuf) {
    let conn = crate::test_support::mem_db();
    let project_dir = tempfile::tempdir().unwrap();
    let project = project_dir.path().to_path_buf();
    crate::namespaces_repo::add_namespace(&conn, namespace_id, "github_org", "org", 0).unwrap();
    crate::repos_repo::add_repo(
        &conn,
        repo_id,
        namespace_id,
        "github",
        None,
        "repo",
        project.to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(&conn, session_id, "t", repo_id, namespace_id).unwrap();
    (conn, project_dir, project)
}

fn team_db(conn: rusqlite::Connection) -> crate::db::Db {
    crate::db::Db(crate::perf_probe::TimedMutex::new(conn))
}

fn key() -> MemberKey {
    MemberKey::new("s1", "run1", "run1-a1")
}

fn ledger_result(status: &str, final_text: &str) -> MemberResult {
    MemberResult {
        schema_version: 1,
        assignment_id: "dispatch-worker-lead-0".into(),
        participant_id: "participant-worker".into(),
        status: status.into(),
        failure_reason: (status == "failed").then(|| "worker exited with code 1".into()),
        changed_files: vec![ChangedFile {
            path: "src/lib.rs".into(),
            insertions: 3,
            deletions: 1,
        }],
        anchor: ResultAnchor {
            base_sha: "base123".into(),
            head_sha: None,
            diff_ref: None,
            generated_from: "worktree_diff".into(),
        },
        command_evidence: vec![],
        risk_inputs: crate::agent_event::RiskInputs {
            files_changed: 1,
            cmd_danger: "none".into(),
            reversibility: "reversible".into(),
        },
        decisions: vec![],
        risks: vec![],
        final_text_ref: Some(final_text.into()),
        artifact_refs: vec![],
        result_source: "raw".into(),
        requires_long_task: None,
        exit_code: None,
        stderr_tail: None,
        failure_kind: None,
    }
}

fn running_dispatch_card_for_terminal_ordering() -> crate::db::Block {
    crate::db::Block::DispatchCard {
        run_id: "worker-run-1".into(),
        member: crate::db::MemberSnapshot {
            participant_id: "participant-worker".into(),
            assignment_id: "dispatch-worker-lead-0".into(),
            task_id: "task-1".into(),
            name: "Worker".into(),
            started_at: Some(1_785_500_450_123),
            status: "running".into(),
            sub: "实现终态收敛".into(),
            steps_total: 1,
            steps_done: 0,
            cost_usd: None,
            input_tokens: 0,
            output_tokens: 0,
            failed: false,
            blocks: vec![],
            result: None,
        },
    }
}

fn terminal_dispatch_card_member(conn: &Connection, session_id: &str) -> crate::db::MemberSnapshot {
    crate::db::get_messages(conn, session_id)
        .unwrap()
        .into_iter()
        .flat_map(|message| message.content)
        .find_map(|block| match block {
            crate::db::Block::DispatchCard { member, .. } => Some(member),
            _ => None,
        })
        .expect("expected persisted dispatch card")
}

fn assert_member_report_delivery_pending(conn: &Connection, session_id: &str, assignment_id: &str) {
    let pending: (String, Option<i64>) = conn
        .query_row(
            "SELECT assignment_id, delivered_at
                   FROM member_report_delivery
                  WHERE session_id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(pending, (assignment_id.to_string(), None));
}

// (3) 生命周期：真子进程实时流 + 退出后 registry 清理（codex P2-2·用 /bin/sh 假子进程·不起真 CLI）
fn member_reader_test_child(script: &str) -> Child {
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", script])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().unwrap()
}

fn empty_parser(_: &str) -> Vec<AgentEvent> {
    Vec::new()
}

mod dispatch_events;
mod locale_auth_retry;
mod preparation;
mod reader_errors;
mod reader_harness;
mod reader_streaming;
mod registry;
mod report_persistence;
mod result_evidence;
mod source_contracts;
mod task_goals;
mod worker_stage1;
