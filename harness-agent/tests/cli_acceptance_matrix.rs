use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

/// headless 严格断言：除「末尾换行产生的尾段」外，任何空行都视为人读泄漏 -> panic。
pub fn assert_pure_jsonl(stdout: &[u8]) -> Vec<Value> {
    let s = String::from_utf8_lossy(stdout);
    let parts: Vec<&str> = s.split('\n').collect();
    let mut out = Vec::new();
    for (i, line) in parts.iter().enumerate() {
        let is_trailing = i == parts.len() - 1;
        if line.is_empty() {
            assert!(
                is_trailing,
                "空行泄漏到 stdout（banner/提示符/EOF println 未静默）at index {i}"
            );
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("非 JSON 行泄漏到 stdout: {line:?} ({e})"));
        assert!(v.is_object(), "stdout 行不是 JSON object: {line:?}");
        for chrome in ["myagent>", "myagent interactive", "type /help"] {
            assert!(!line.contains(chrome), "人读外壳串泄漏: {chrome}");
        }
        out.push(v);
    }
    out
}

pub fn parse_lines(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn criteria_met_completes_exit0() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "just say hi",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--criteria",
            "cmd: true",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let events = parse_lines(&out.stdout);
    let ce = events
        .iter()
        .find(|e| e["type"] == "completion.evaluated")
        .unwrap();
    assert!(ce["payload"]["criteria"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["status"] == "passed"));
    assert!(events.iter().any(|e| e["type"] == "run.completed"));
}

#[test]
fn criteria_unmet_blocks_exit3() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "just say hi",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--max-eval-attempts",
            "1",
            "--criteria",
            "cmd: false",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(parse_lines(&out.stdout)
        .iter()
        .any(|e| e["type"] == "run.blocked"));
}

#[test]
fn tools_emit_started_stdout_completed() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "ship dispatch handoff",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let events = parse_lines(&out.stdout);
    assert!(events.iter().any(|e| e["type"] == "tool.started"));
    assert!(events.iter().any(|e| e["type"] == "tool.stdout.delta"));
    assert!(events.iter().any(|e| e["type"] == "tool.completed"));
}

#[test]
fn approval_gate_stdin_approve_executes_tool() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "ship dispatch handoff",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "ask",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .write_stdin(
            "{\"type\":\"approve\",\"run_id\":\"x\",\"approval_id\":\"approval_call_mock_shell_1\"}\n",
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = parse_lines(&out.stdout);
    assert!(events.iter().any(|e| e["type"] == "approval.requested"));
    assert!(events
        .iter()
        .any(|e| e["type"] == "approval.resolved" && e["payload"]["decision"] == "approved"));
    assert!(events.iter().any(|e| e["type"] == "tool.started"));
    assert!(events.iter().any(|e| e["type"] == "tool.completed"));
}

#[test]
fn shell_exec_escape_blocked_before_gate() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "escape shell please",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "ask",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = parse_lines(&out.stdout);
    assert!(
        !events.iter().any(|e| e["type"] == "approval.requested"),
        "escape command must be blocked before approval gate"
    );
    assert!(events.iter().any(|e| {
        e["type"] == "tool.failed"
            && e["payload"]["tool"] == "shell_exec"
            && e["payload"]["rule"] == "setsid"
            && e["payload"]["error"] == "blocked: escape attempt (setsid)"
    }));
    assert!(events.iter().any(|e| e["type"] == "run.completed"));
    assert!(!events.iter().any(|e| e["type"] == "run.failed"));
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore)]
fn network_off_blocks_curl_in_shell_tool() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "egress curl",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--network",
            "off",
            "--jsonl",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = parse_lines(&out.stdout);
    let curl_call_id = events
        .iter()
        .find(|e| {
            e["type"] == "tool.started"
                && e["payload"]["tool"] == "shell_exec"
                && e["payload"]["command"] == "curl -sS --max-time 5 https://example.com"
        })
        .and_then(|e| e["payload"]["tool_call_id"].as_str())
        .expect("curl shell_exec tool.started must exist");
    let completed = events
        .iter()
        .find(|e| {
            e["type"] == "tool.completed"
                && e["payload"]["tool"] == "shell_exec"
                && e["payload"]["tool_call_id"] == curl_call_id
        })
        .expect("curl shell_exec tool.completed must exist");
    assert_ne!(completed["payload"]["exit_code"].as_i64(), Some(0));
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore)]
fn network_on_allows_curl_not_blocked_by_sandbox() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "egress curl",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--network",
            "on",
            "--jsonl",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = parse_lines(&out.stdout);
    let curl_call_id = events
        .iter()
        .find(|e| {
            e["type"] == "tool.started"
                && e["payload"]["tool"] == "shell_exec"
                && e["payload"]["command"] == "curl -sS --max-time 5 https://example.com"
        })
        .and_then(|e| e["payload"]["tool_call_id"].as_str())
        .expect("curl shell_exec tool.started must exist");
    assert!(events.iter().any(|e| {
        e["type"] == "tool.completed"
            && e["payload"]["tool"] == "shell_exec"
            && e["payload"]["tool_call_id"] == curl_call_id
    }));
    assert!(!events.iter().any(|e| {
        e["type"] == "tool.failed"
            && e["payload"]["error"]
                .as_str()
                .is_some_and(|s| s.contains("unenforceable"))
    }));
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore)]
fn injected_exfil_attempt_blocked_under_network_off() {
    // 二段式：第一轮工具读本地内容（模拟读到注入指令），第二轮模型据此发起出网。
    // --network off 下断言第二轮出网被 seatbelt 确定性卡死（验 harness 关卡、不验模型乖不乖）。
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "two step egress",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--network",
            "off",
            "--jsonl",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = parse_lines(&out.stdout);
    let egress_call_id = events
        .iter()
        .find(|e| {
            e["type"] == "tool.started"
                && e["payload"]["tool"] == "shell_exec"
                && e["payload"]["command"] == "curl -sS --max-time 5 https://example.org/collect"
        })
        .and_then(|e| e["payload"]["tool_call_id"].as_str())
        .expect("second-round egress shell_exec tool.started must exist");
    let completed = events
        .iter()
        .find(|e| {
            e["type"] == "tool.completed"
                && e["payload"]["tool"] == "shell_exec"
                && e["payload"]["tool_call_id"] == egress_call_id
        })
        .expect("second-round egress tool.completed must exist");
    assert_ne!(completed["payload"]["exit_code"].as_i64(), Some(0));
}

#[test]
fn check_cmd_scrubs_secret_env() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .env("DEEPSEEK_API_KEY", "leak-me")
        .args([
            "run",
            "verify env scrub",
            "--criteria",
            "cmd: test -z \"$DEEPSEEK_API_KEY\"",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--jsonl",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = parse_lines(&out.stdout);
    assert!(events.iter().any(|e| e["type"] == "run.completed"));
    let evald = events
        .iter()
        .find(|e| e["type"] == "completion.evaluated")
        .unwrap();
    assert_eq!(evald["payload"]["criteria"][0]["status"], "passed");
}

#[test]
fn check_cmd_escape_blocked_fails_and_blocks() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "verify check escape",
            "--criteria",
            "cmd: setsid true",
            "--max-eval-attempts",
            "1",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--jsonl",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let events = parse_lines(&out.stdout);
    assert!(events.iter().any(|e| {
        e["type"] == "tool.failed"
            && e["payload"]["tool"] == "check_cmd"
            && e["payload"]["rule"] == "setsid"
    }));
    assert!(events.iter().any(|e| e["type"] == "run.blocked"));
}

#[test]
fn cli_propose_scope_change_jsonl_eof_deny_and_continue() {
    let ws = tempdir().unwrap();
    let jnl = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "propose scope change please",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--jsonl",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            jnl.path().to_str().unwrap(),
        ])
        .write_stdin("")
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(4));
    let events = parse_lines(&out.stdout);
    assert!(events.iter().any(|e| e["type"] == "goal.change.rejected"));
    assert!(events
        .iter()
        .any(|e| e.to_string().contains("approval_unavailable")));
    assert!(!events.iter().any(|e| e["type"] == "run.needs_decision"));
}

#[test]
fn cli_propose_criterion_approve_then_completes() {
    let ws = tempdir().unwrap();
    let jnl = tempdir().unwrap();
    let stdin = "{\"type\":\"approve\",\"run_id\":\"r_cli_crit\",\"approval_id\":\"approval_proposal_call_crit_1\"}\n";
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "propose criterion then finish",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--contract-policy",
            "ask",
            "--jsonl",
            "--run-id",
            "r_cli_crit",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            jnl.path().to_str().unwrap(),
        ])
        .write_stdin(stdin)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout);
    let lines: Vec<Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let p = |pred: &dyn Fn(&Value) -> bool, what: &str| {
        lines
            .iter()
            .position(pred)
            .unwrap_or_else(|| panic!("缺 {what}"))
    };
    let req = p(
        &|v| v["type"] == "approval.requested" && v["payload"]["request_kind"] == "criterion",
        "approval.requested",
    );
    let res = p(&|v| v["type"] == "approval.resolved", "approval.resolved");
    let app = p(
        &|v| v["type"] == "goal.change.approved",
        "goal.change.approved",
    );
    let upd = p(&|v| v["type"] == "goal.updated", "goal.updated");
    let check = p(
        &|v| v["type"] == "tool.started" && v["payload"]["tool"] == "check_cmd",
        "check_cmd",
    );
    assert!(
        req < res && res < app && app < upd && upd < check,
        "approve 前绝不跑 check_cmd"
    );
}

#[test]
fn cli_disallow_tools_rejects_propose_criterion_without_decision() {
    let ws = tempdir().unwrap();
    let jnl = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "propose criterion then finish",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--contract-policy",
            "ask",
            "--jsonl",
            "--disallow-tools",
            "propose_criterion",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            jnl.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    let events = assert_pure_jsonl(&out.stdout);
    assert!(!events.iter().any(|e| e["type"] == "goal.change.proposed"));
    assert!(!events.iter().any(|e| e["type"] == "run.needs_decision"));
    assert!(events.iter().any(|e| {
        e["type"] == "tool.failed"
            && e["payload"]["tool"] == "propose_criterion"
            && e["payload"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("disabled for this run"))
    }));
    assert!(events.iter().any(|e| e["type"] == "run.completed"));
}

#[test]
fn interrupt_via_stdin_stop_exits_130() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "ship dispatch handoff",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .write_stdin("{\"type\":\"stop\",\"run_id\":\"x\"}\n")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(130));
    assert!(parse_lines(&out.stdout)
        .iter()
        .any(|e| e["type"] == "run.interrupted"));
}

#[test]
fn reasoning_delta_is_emitted_for_reasoning_prompt() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "show reasoning please",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = parse_lines(&out.stdout);
    let rd = events
        .iter()
        .find(|e| e["type"] == "agent.reasoning.delta")
        .expect("须 emit agent.reasoning.delta");
    assert!(
        rd["payload"]["text"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "reasoning delta 须带非空 text"
    );
    assert!(events.iter().any(|e| e["type"] == "run.completed"));
}

#[test]
fn shell_headless_two_prompts_pure_jsonl_two_completed_resume_seq() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "shell",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--jsonl",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .write_stdin("first prompt\nsecond prompt\n/exit\n")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = assert_pure_jsonl(&out.stdout);
    assert!(events.iter().any(|e| e["type"] == "run.started"));
    assert!(events.iter().any(|e| e["type"] == "run.resumed"));
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "run.completed")
            .count(),
        2
    );
    let first_max = events
        .iter()
        .take_while(|e| e["type"] != "run.resumed")
        .filter_map(|e| e["seq"].as_u64())
        .max()
        .unwrap();
    let resumed_seq = events.iter().find(|e| e["type"] == "run.resumed").unwrap()["seq"]
        .as_u64()
        .unwrap();
    assert!(
        resumed_seq > first_max,
        "resume seq {resumed_seq} 须 > 第一段 max {first_max}"
    );
}

#[test]
fn shell_headless_slash_new_starts_fresh_run_not_resume() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "shell",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--jsonl",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .write_stdin("first prompt\n/new\nsecond prompt\n/exit\n")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = assert_pure_jsonl(&out.stdout);
    assert_eq!(
        events.iter().filter(|e| e["type"] == "run.started").count(),
        2,
        "/new 后应两个 fresh run"
    );
    assert_eq!(
        events.iter().filter(|e| e["type"] == "run.resumed").count(),
        0,
        "/new 必阻断 resume"
    );
}

#[test]
fn shell_headless_eof_without_exit_stays_pure_jsonl() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "shell",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--jsonl",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .write_stdin("only prompt\n")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = assert_pure_jsonl(&out.stdout);
    assert!(events.iter().any(|e| e["type"] == "run.completed"));
}

#[test]
fn acceptance_md_case_refs_all_exist_somewhere() {
    let root = env!("CARGO_MANIFEST_DIR");
    let acceptance_md = format!("{root}/ACCEPTANCE.md");
    if !std::path::Path::new(&acceptance_md).exists() {
        eprintln!(
            "ACCEPTANCE.md not present in this checkout (stripped from the public snapshot); skipping the doc-reference cross-check"
        );
        return;
    }
    let md = std::fs::read_to_string(acceptance_md).unwrap();
    let mut haystack = String::new();
    for f in [
        "tests/cli_acceptance_matrix.rs",
        "tests/cli_jsonl.rs",
        "src/orchestrator.rs",
        "tests/reject_loop.rs",
        "src/evaluator.rs",
        "tests/tool_call_id_unique.rs",
        "tests/golden_synthetic.rs",
    ] {
        if let Ok(s) = std::fs::read_to_string(format!("{root}/{f}")) {
            haystack.push_str(&s);
        }
    }
    for case in [
        "criteria_met_completes_exit0",
        "criteria_unmet_blocks_exit3",
        "tools_emit_started_stdout_completed",
        "reasoning_delta_is_emitted_for_reasoning_prompt",
        "cli_propose_scope_change_jsonl_eof_deny_and_continue",
        "cli_propose_criterion_approve_then_completes",
        "shell_headless_two_prompts_pure_jsonl_two_completed_resume_seq",
        "shell_headless_slash_new_starts_fresh_run_not_resume",
        "shell_headless_eof_without_exit_stays_pure_jsonl",
        "approval_gate_stdin_approve_executes_tool",
        "interrupt_via_stdin_stop_exits_130",
        "rejected_mutating_tool_is_reported_to_model_and_run_continues",
        "ask_permission_fails_closed_in_non_interactive_jsonl_mode",
        "interrupt_via_control_source_emits_run_interrupted",
        "resume_uses_saved_provider_and_appends_to_journal",
        "three_consecutive_user_rejections_self_stop_blocked",
        "check_cmd_scrubs_secret_env",
        "check_cmd_escape_blocked_fails_and_blocks",
        "shell_exec_escape_blocked_before_gate",
        "network_off_blocks_curl_in_shell_tool",
        "network_on_allows_curl_not_blocked_by_sandbox",
        "injected_exfil_attempt_blocked_under_network_off",
        "check_cmd_emits_deterministic_tool_call_id",
        "tool_call_ids_unique_within_run",
        "inspect_summary_shows_terminal_and_criteria",
        "inspect_jsonl_replays_journal_bytes",
        "inspect_jsonl_passthrough_preserves_truncated_tail",
        "inspect_unknown_run_exits_1",
        "inspect_list_jsonl_finds_runs_with_terminals",
        "inspect_list_empty_root_outputs_nothing_exit0",
        "inspect_usage_errors_exit_2",
        "info_mock_json_has_full_capability_fields",
        "info_json_key_set_matches_capabilities_declared_payload",
        "info_deepseek_json_works_offline_without_key",
        "needs_decision_long_task_shape_round_trips_single_handle",
        "needs_decision_long_task_multi_handles_description_optional",
    ] {
        assert!(md.contains(case), "ACCEPTANCE.md 缺 case 引用: {case}");
        assert!(
            haystack.contains(case),
            "测试源里找不到 case（归属错/被改名）: {case}"
        );
    }
}

// ===== C5: inspect（摘要 + 字节重放）=====

#[test]
fn inspect_summary_shows_terminal_and_criteria() {
    let ws = tempdir().unwrap();
    let run = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "just say hi",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--criteria",
            "cmd: true",
            "--run-id",
            "inspect_t1",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(0));
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "inspect",
            "inspect_t1",
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("run.completed"), "摘要须含终态: {text}");
    assert!(text.contains("passed"), "摘要须含 criteria 状态: {text}");
    // check_cmd 恰好跑一次（cmd: true 首轮即过）——锁计数，防 tool_started 统计逻辑回退
    assert!(
        text.contains("tools 1 started"),
        "摘要须含工具统计（check_cmd 计 1）: {text}"
    );
}

#[test]
fn inspect_jsonl_replays_journal_bytes() {
    let ws = tempdir().unwrap();
    let run = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "just say hi",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--run-id",
            "inspect_t2",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(0));
    let events_path = ws.path().join(".myagenthubs/runs/inspect_t2/events.jsonl");
    let file_bytes = std::fs::read(&events_path).unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "inspect",
            "inspect_t2",
            "--jsonl",
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, file_bytes, "重放必须与 journal 文件逐字节一致");
}

#[test]
fn inspect_jsonl_passthrough_preserves_truncated_tail() {
    // 手工 journal：末行写了一半（无尾随换行）——透传不得补/删字节。
    let ws = tempdir().unwrap();
    let dir = ws.path().join(".myagenthubs/runs/run_trunc");
    std::fs::create_dir_all(&dir).unwrap();
    let bytes: Vec<u8> = b"{\"type\":\"run.started\",\"seq\":1}\n{\"type\":\"tool.sta".to_vec();
    std::fs::write(dir.join("events.jsonl"), &bytes).unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "inspect",
            "run_trunc",
            "--jsonl",
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, bytes, "透传不得改字节（含截断末行）");
}

#[test]
fn inspect_unknown_run_exits_1() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "inspect",
            "no_such_run",
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!out.stderr.is_empty(), "须有 stderr 报错");
}

// ===== C5: inspect --list =====

#[test]
fn inspect_list_jsonl_finds_runs_with_terminals() {
    let ws = tempdir().unwrap();
    // run 1: completed（不固定 run_id——验「忘了 id 用 --list 找回」的真路径）
    let run1 = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "just say hi",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--criteria",
            "cmd: true",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(run1.status.code(), Some(0));
    // run 2: blocked（终态非 completed 也必须正确报出）
    let run2 = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "just say hi",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--max-eval-attempts",
            "1",
            "--criteria",
            "cmd: false",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(run2.status.code(), Some(3));
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "inspect",
            "--list",
            "--jsonl",
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let rows = parse_lines(&out.stdout);
    assert_eq!(rows.len(), 2, "应列出 2 个 run: {rows:?}");
    assert!(rows.iter().any(|r| r["terminal"] == "run.completed"));
    assert!(rows.iter().any(|r| r["terminal"] == "run.blocked"));
    for r in &rows {
        assert!(r["run_id"].is_string(), "行必须带 run_id: {r:?}");
        assert!(r["ts"].is_string(), "mock run 的行应有 ts: {r:?}");
    }
}

#[test]
fn inspect_list_empty_root_outputs_nothing_exit0() {
    let ws = tempdir().unwrap();
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "inspect",
            "--list",
            "--jsonl",
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "空 journal 根应空输出");
}

#[test]
fn inspect_usage_errors_exit_2() {
    // run_id 与 --list 都不给
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args(["inspect"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "都不给应 usage exit 2");
    // 都给
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args(["inspect", "some_run", "--list"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "都给应 usage exit 2");
}

// ===== C5: info capabilities 正式化 =====

#[test]
fn info_mock_json_has_full_capability_fields() {
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .args(["info", "--provider", "mock", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    for key in [
        "provider_id",
        "model_id",
        "supports_streaming",
        "supports_reasoning_deltas",
        "supports_tool_calling",
        "supports_images",
        "supports_computer_use",
        "supports_shell_tool",
        "max_context_tokens",
        "output_token_limit",
        "server_side_search",
    ] {
        assert!(v.get(key).is_some(), "info --json 缺字段 {key}: {v}");
    }
    assert_eq!(v["provider_id"], "mock");
}

#[test]
fn info_json_key_set_matches_capabilities_declared_payload() {
    use std::collections::BTreeSet;
    // 端到端防漂：跑一个 mock run 抓 capabilities.declared 事件 payload，
    // 与 info --json 输出比 key 集合（比 key 不比字符串——info 是 pretty、事件是紧凑）。
    let ws = tempdir().unwrap();
    let run = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "just say hi",
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "allow",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(0));
    let events = parse_lines(&run.stdout);
    let declared = events
        .iter()
        .find(|e| e["type"] == "capabilities.declared")
        .expect("run 应发 capabilities.declared");
    let event_keys: BTreeSet<String> = declared["payload"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let info = Command::cargo_bin("myagent")
        .unwrap()
        .args(["info", "--provider", "mock", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&info.stdout).unwrap();
    let info_keys: BTreeSet<String> = v.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        info_keys, event_keys,
        "info --json 与 capabilities.declared payload 同源 key 漂移"
    );
}

#[test]
fn info_deepseek_json_works_offline_without_key() {
    // 静态声明式查询承诺：无 key、无网络也必须成功（CONTRACT §8）。
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("MYAGENT_API_KEY")
        .args(["info", "--provider", "deepseek", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "info 是静态查询·无 key 无网络也必须成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["provider_id"], "deepseek");
}
