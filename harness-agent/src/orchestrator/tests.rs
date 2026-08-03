#[cfg(test)]
fn verify_reflex_record_debt(debt: &mut usize, invalidates: bool, execute_ok: bool) {
    if execute_ok && invalidates {
        *debt += 1;
    }
}

use super::*;
use crate::control::{ControlCommand, ControlSource, QueueControlSource};
use crate::journal::{load_conversation, save_conversation, SavedConversation};
use crate::provider::pairing::validate_tool_pairing;
use crate::provider::{FunctionCall, ProviderResponse, ToolCall};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

struct LiveChannel;

impl ControlSource for LiveChannel {
    fn poll(&mut self) -> Option<ControlCommand> {
        None
    }

    fn approval_channel_available(&self) -> bool {
        true
    }
}

#[test]
fn default_verify_and_watchdog_are_nonzero() {
    assert_eq!(DEFAULT_VERIFY_EVERY, 3);
    assert_eq!(DEFAULT_WATCHDOG_REPEAT, 3);
}

#[test]
fn executor_system_prompt_teaches_persistent_ripple_work() {
    let messages = initial_messages("do the work");
    let system = messages[0].content.as_deref().unwrap();

    assert!(system.contains("keep working until its acceptance"));
    assert!(system.contains("Use tools aggressively"));
    assert!(system.contains("ALL affected sites"));
    assert!(system
        .replace('\n', " ")
        .contains("the   harness runs them in order"));
    assert!(system.contains("compiler/tests"));
    assert!(system.contains("not progress"));
    assert!(system.contains("shell_exec"));
    assert!(!system.contains("Use tools only when needed"));

    // sharpened wide-ripple guidance (2026-06-21 slice)
    assert!(system.contains("the sites you miss are almost always in tests"));
    assert!(system.contains("grep -rn"));
    assert!(system.contains("whose path is under tests"));
    assert!(system.contains("grep → patch → rebuild"));
    assert!(system.contains("escalate with the exact paths"));
    assert!(system.contains("patch at least one listed site"));
}

#[test]
fn make_search_backend_picks_brave_when_key_present() {
    use super::search_backend_kind;
    use crate::config::SearchChoice;

    assert_eq!(
        search_backend_kind(&SearchChoice::Brave {
            api_key: "k".into()
        }),
        "fallback_brave_ddg"
    );
    assert_eq!(search_backend_kind(&SearchChoice::Ddg), "ddg");
}

#[test]
fn search_backend_kind_exa() {
    use crate::config::SearchChoice;

    assert_eq!(
        super::search_backend_kind(&SearchChoice::Exa {
            api_key: "k".into()
        }),
        "fallback_exa_ddg"
    );
}

#[tokio::test]
async fn run_solo_task_real_time_gate_allows_out_of_allowlist_with_advisory() {
    use crate::plan::write_audit::TaskScope;
    // C2：白名单外写入 → 软放行（文件真生成）+ journal 有 scope.advisory（不再硬挡）
    #[derive(Clone)]
    struct OneWriteProvider;
    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for OneWriteProvider {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut EventRecorder,
        ) -> Result<crate::provider::ProviderResponse> {
            if messages.iter().any(|m| m.role == "tool") {
                return Ok(crate::provider::ProviderResponse {
                    text: "done".into(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    finish_reason: None,
                });
            }
            Ok(crate::provider::ProviderResponse {
                text: "writing".into(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "w1".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "fs_write".into(),
                        arguments:
                            serde_json::json!({ "path": "out_of_scope.txt", "content": "x" })
                                .to_string(),
                    },
                }],
                finish_reason: None,
            })
        }
        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            task_test_caps()
        }
    }

    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "rt_gate", vec![]);
    let scope = Some(TaskScope {
        files_scope: vec!["allowed.txt".into()],
        forbidden_scope: vec![],
        crate_roots: vec![],
    });
    run_solo_task(
        OneWriteProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
        None,
        scope,
    )
    .await
    .unwrap();

    assert!(
        ws.path().join("out_of_scope.txt").exists(),
        "白名单外写入·软放行·文件应生成"
    );
    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/rt_gate/events.jsonl")).unwrap();
    assert!(
        events.contains("\"type\":\"scope.advisory\""),
        "应发 scope.advisory 软提示事件：{events}"
    );
    assert!(
        !events.contains("out of task scope"),
        "白名单外不再硬挡 PermissionDenied"
    );
}

#[tokio::test]
async fn run_solo_task_real_time_gate_hard_denies_forbidden() {
    use crate::plan::write_audit::TaskScope;
    // C2：forbidden 红线仍硬挡（文件不生成）
    #[derive(Clone)]
    struct ForbiddenWriteProvider;
    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for ForbiddenWriteProvider {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut EventRecorder,
        ) -> Result<crate::provider::ProviderResponse> {
            if messages.iter().any(|m| m.role == "tool") {
                return Ok(crate::provider::ProviderResponse {
                    text: "done".into(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    finish_reason: None,
                });
            }
            Ok(crate::provider::ProviderResponse {
                text: "writing".into(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "w1".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "fs_write".into(),
                        arguments: serde_json::json!({ "path": "secret.txt", "content": "x" })
                            .to_string(),
                    },
                }],
                finish_reason: None,
            })
        }
        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            task_test_caps()
        }
    }

    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "rt_forbidden", vec![]);
    let scope = Some(TaskScope {
        files_scope: vec!["secret.txt".into()],
        forbidden_scope: vec!["secret.txt".into()],
        crate_roots: vec![],
    });
    run_solo_task(
        ForbiddenWriteProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
        None,
        scope,
    )
    .await
    .unwrap();

    assert!(
        !ws.path().join("secret.txt").exists(),
        "红线 forbidden 写入·硬挡·文件不该生出来"
    );
    let events = std::fs::read_to_string(
        jr.path()
            .join(".myagenthubs/runs/rt_forbidden/events.jsonl"),
    )
    .unwrap();
    assert!(events.contains("out of task scope") || events.contains("permission denied"));
}

#[tokio::test]
async fn run_solo_task_injects_task_contract_scope_and_constraints() {
    // B1：任务契约的 scope/constraints 必须进 child（goal.created 看得见）
    #[derive(Clone)]
    struct DoneProvider;
    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for DoneProvider {
        async fn next_turn(
            &self,
            _m: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut EventRecorder,
        ) -> Result<crate::provider::ProviderResponse> {
            Ok(crate::provider::ProviderResponse {
                text: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }
        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            task_test_caps()
        }
    }
    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let contract = crate::goal::GoalContract {
        objective: "edit a".into(),
        constraints: vec!["forbidden_scope（绝不能碰）: src/secret.rs".into()],
        scope: Some("src/a.rs".into()),
        criteria: vec![],
        version: 1,
        update_log: vec![],
    };
    let opts = task_test_run_options(ws.path(), jr.path(), "inject", vec![]);
    run_solo_task(
        DoneProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
        Some(contract),
        None,
    )
    .await
    .unwrap();
    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/inject/events.jsonl")).unwrap();
    assert!(
        events.contains("\"scope\":\"src/a.rs\""),
        "child goal.created 须带 scope: {events}"
    );
    assert!(
        events.contains("src/secret.rs"),
        "child goal.created 须带 forbidden 约束"
    );
}

#[tokio::test]
async fn propose_scope_change_with_paths_extends_and_continues() {
    // C3：kind=scope 且给 paths → 并进白名单、run 继续（不 NeedsDecision）+ scope.extended 事件
    #[derive(Clone)]
    struct ExtendThenDoneProvider;

    #[async_trait::async_trait]
    impl crate::provider::ProviderClient for ExtendThenDoneProvider {
        async fn next_turn(
            &self,
            messages: &[crate::provider::ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut EventRecorder,
        ) -> Result<crate::provider::ProviderResponse> {
            if messages.iter().any(|m| m.role == "tool") {
                return Ok(crate::provider::ProviderResponse {
                    text: "done".into(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    finish_reason: None,
                });
            }
            Ok(crate::provider::ProviderResponse {
                text: "need more files".into(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "s1".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "propose_scope_change".into(),
                        arguments: serde_json::json!({
                            "kind": "scope",
                            "detail": "need to touch the cli too",
                            "paths": ["src/cli.rs"]
                        })
                        .to_string(),
                    },
                }],
                finish_reason: None,
            })
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            task_test_caps()
        }
    }

    let ws = tempfile::tempdir().unwrap();
    let jr = tempfile::tempdir().unwrap();
    let opts = task_test_run_options(ws.path(), jr.path(), "scope_ext", passing_criteria());
    let scope = Some(crate::plan::write_audit::TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec![],
        crate_roots: vec![],
    });
    let result = run_solo_task(
        ExtendThenDoneProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
        None,
        scope,
    )
    .await
    .unwrap();

    assert_ne!(
        result.outcome,
        RunOutcome::NeedsDecision,
        "带 paths 的 scope 申报不该硬停"
    );
    let events =
        std::fs::read_to_string(jr.path().join(".myagenthubs/runs/scope_ext/events.jsonl"))
            .unwrap();
    assert!(
        events.contains("\"type\":\"scope.extended\""),
        "应发 scope.extended：{events}"
    );
    assert!(events.contains("src/cli.rs"));
}

fn task_test_caps() -> crate::provider::ProviderCapabilities {
    crate::provider::ProviderCapabilities {
        provider_id: "mock".into(),
        model_id: "mock".into(),
        supports_streaming: false,
        supports_reasoning_deltas: false,
        supports_tool_calling: true,
        supports_images: false,
        supports_computer_use: false,
        supports_shell_tool: true,
        max_context_tokens: Some(128_000),
        output_token_limit: Some(8_192),
        server_side_search: false,
    }
}

fn task_test_run_options(
    ws: &std::path::Path,
    jr: &std::path::Path,
    run_id: &str,
    criteria: Vec<crate::goal::Criterion>,
) -> RunOptions {
    RunOptions {
        prompt: "do the task".into(),
        workspace: ws.to_path_buf(),
        provider_id: "mock".into(),
        model: "mock".into(),
        client_session_id: None,
        output_mode: crate::events::OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        evidence_gate: EvidenceGate::Off,
        permission: crate::shell::PermissionPolicy::Allow,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
        native_search_enabled: false,
        disallowed_tools: std::collections::BTreeSet::new(),
        memory_enabled: false,
        search: crate::config::SearchChoice::Ddg,
        max_turns: 4,
        run_id: Some(run_id.into()),
        context_files: vec![],
        criteria,
        contract_policy: crate::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 3,
        verify_reflex_debt: DEFAULT_VERIFY_EVERY,
        watchdog_repeat_threshold: DEFAULT_WATCHDOG_REPEAT,
        journal_root: jr.to_path_buf(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

fn evidence_tool_recorder(dir: &std::path::Path, run_id: &str) -> EventRecorder {
    EventRecorder::new(
        run_id,
        None,
        None,
        &dir.join("events.jsonl"),
        crate::events::OutputMode::Silent,
    )
    .unwrap()
}

fn init_git_index(workspace: &std::path::Path, paths: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(workspace)
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new("git")
        .arg("add")
        .arg("--")
        .args(paths)
        .current_dir(workspace)
        .status()
        .unwrap();
    assert!(status.success());
}

#[derive(Clone)]
struct EvidenceToolOffProvider {
    calls: Arc<AtomicUsize>,
    saw_dispatch_rejection: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceToolOffProvider {
    async fn next_turn(
        &self,
        messages: &[crate::provider::ChatMessage],
        tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        assert!(tools
            .iter()
            .all(|tool| { tool["function"]["name"].as_str() != Some("register_issue_probe") }));
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(crate::provider::ProviderResponse {
                text: String::new(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "probe-off-hard-call".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "register_issue_probe".into(),
                        arguments: json!({
                            "script": "printf BUG",
                            "command": "sh {probe}",
                            "red_marker": "BUG",
                            "rationale": "hard call while disabled"
                        })
                        .to_string(),
                    },
                }],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
            });
        }

        let rejected = messages.iter().any(|message| {
            message.role == "tool"
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("disabled for this run"))
        });
        self.saw_dispatch_rejection
            .store(usize::from(rejected), Ordering::SeqCst);
        Ok(crate::provider::ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: Some(crate::provider::FinishReason::Stop),
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn evidence_tool_off_is_not_offered_and_hard_call_is_rejected() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_dispatch_rejection = Arc::new(AtomicUsize::new(0));
    let options = task_test_run_options(workspace.path(), journal.path(), "evidence-off", vec![]);

    run_solo(
        EvidenceToolOffProvider {
            calls,
            saw_dispatch_rejection: saw_dispatch_rejection.clone(),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(saw_dispatch_rejection.load(Ordering::SeqCst), 1);
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-off").events_path).unwrap();
    assert!(!events.contains("evidence.probe."));
    assert!(!events.contains("evidence.gate."));
}

#[test]
fn evidence_tool_on_is_offered() {
    let registry = build_default_registry(&crate::config::SearchChoice::Ddg, false);
    let tools = build_offered_tools(
        &registry,
        &task_test_caps(),
        crate::goal::NetworkPolicy::On,
        false,
        &std::collections::BTreeSet::new(),
    );

    assert!(tools
        .iter()
        .any(|tool| { tool["function"]["name"].as_str() == Some("register_issue_probe") }));
}

#[tokio::test]
async fn evidence_liveness_empty_marker_counts_as_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "empty-marker");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    let feedback = register_issue_probe_call(
        &json!({
            "script": "printf BUG",
            "command": "sh {probe}",
            "red_marker": "",
            "rationale": "empty marker must be rejected before execution"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        1,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    assert!(feedback.contains("red_marker"));
    assert!(feedback.contains("must be non-empty"));
    assert_eq!(attempts, 1);
    assert_eq!(evidence.failed_registrations, 1);
    assert!(evidence.probe.is_none());
}

#[tokio::test]
async fn evidence_liveness_malformed_command_without_probe_placeholder_counts_as_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "missing-placeholder");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    for turn in 1..=MAX_FAILED_REGISTRATIONS {
        let feedback = register_issue_probe_call(
            &json!({
                "script": "printf BUG_PRESENT",
                "command": "python -m pytest tests/test_issue.py",
                "red_marker": "BUG_PRESENT",
                "rationale": "natural but malformed command"
            })
            .to_string(),
            &mut evidence,
            &mut attempts,
            workspace.path(),
            &journal.path().join("probes"),
            turn,
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &mut recorder,
        )
        .await
        .unwrap();
        assert!(feedback.contains("must contain the `{probe}` placeholder"));
        assert!(feedback.contains("python -I -B {probe}"));
    }

    assert_eq!(evidence.failed_registrations, MAX_FAILED_REGISTRATIONS);
    assert!(evidence.bypassed);
    assert_eq!(evidence.may_edit(), EditVerdict::Allow);
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert_eq!(events.matches("evidence.probe.rejected").count(), 3);
    assert!(events.contains("\"reason\":\"registration_failures\""));
}

#[tokio::test]
async fn evidence_probe_registered_event_carries_script_and_output() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "code-red");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    let feedback = register_issue_probe_call(
        &json!({
            "script": "printf 'BUG_PRESENT\\n'",
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stdout",
            "rationale": "the buggy behavior prints BUG_PRESENT"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        4,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    assert!(feedback.contains("Probe confirmed RED by the harness (ran twice)"));
    assert!(feedback.contains("detects a workspace content change"));
    assert!(feedback.contains("three consecutive completion denials"));
    assert!(!feedback.contains("automatically after every edit"));
    assert!(!feedback.contains("completes only when it turns green"));
    assert!(feedback.contains("run 1 stdout"));
    assert!(feedback.contains("BUG_PRESENT"));
    assert_eq!(attempts, 1);
    assert_eq!(evidence.failed_registrations, 0);
    assert_eq!(
        evidence.probe.as_ref().map(|probe| probe.probe_id.as_str()),
        Some("issue_probe_4")
    );
    let events: Vec<Value> = std::fs::read_to_string(journal.path().join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let registered = events
        .iter()
        .find(|event| event["type"] == "evidence.probe.registered")
        .expect("registered probe event");
    let payload = &registered["payload"];
    assert_eq!(payload["verdict"], "code_red");
    assert_eq!(payload["attempt"], 1);
    assert_eq!(payload["turn"], 4);
    assert_eq!(payload["script"], "printf 'BUG_PRESENT\\n'");
    assert_eq!(payload["red_marker"], "BUG_PRESENT");
    assert!(payload["command"]
        .as_str()
        .unwrap()
        .starts_with("sh \"${TMPDIR:-/tmp}/agentloom-probes/"));
    assert_eq!(payload["script_sha256"].as_str().unwrap().len(), 64);
    assert!(payload["output_tail"]
        .as_str()
        .unwrap()
        .contains("BUG_PRESENT"));
}

#[tokio::test]
async fn evidence_probe_rejected_event_carries_script_and_output() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "observable-rejection");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    register_issue_probe_call(
        &json!({
            "script": "printf 'actual probe output\\n'",
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stdout",
            "rationale": "does not reproduce the marker"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        2,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    let events: Vec<Value> = std::fs::read_to_string(journal.path().join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejected = events
        .iter()
        .find(|event| event["type"] == "evidence.probe.rejected")
        .expect("rejected probe event");
    let payload = &rejected["payload"];
    assert_eq!(payload["verdict"], "pre_green");
    assert_eq!(payload["script"], "printf 'actual probe output\\n'");
    assert_eq!(payload["red_marker"], "BUG_PRESENT");
    assert!(payload["command"]
        .as_str()
        .unwrap()
        .starts_with("sh \"${TMPDIR:-/tmp}/agentloom-probes/"));
    assert_eq!(payload["script_sha256"].as_str().unwrap().len(), 64);
    assert!(payload["output_tail"]
        .as_str()
        .unwrap()
        .contains("actual probe output"));
}

#[tokio::test]
async fn evidence_missing_probe_script_feedback_identifies_harness_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "missing-probe-script");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    let feedback = register_issue_probe_call(
        &json!({
            "script": "printf 'this script must never run\\n'",
            "command": "rm -f {probe} && python3 {probe}",
            "red_marker": "BUG_PRESENT",
            "rationale": "a missing harness script must not look pre-green"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        2,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    assert!(feedback.contains("harness-side infrastructure failure"));
    assert!(feedback.contains("not a problem with your reproduction"));
    assert!(feedback.contains("probe_script_not_materialized"));
    assert!(!feedback.contains("does not reproduce the bug"));
    assert!(evidence.probe.is_none());

    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"verdict\":\"infra_red\""));
    assert!(events.contains("\"infra_signature\":\"probe_script_not_materialized\""));
    assert!(!events.contains("\"verdict\":\"pre_green\""));
}

#[cfg_attr(
    target_os = "linux",
    ignore = "linux: probe rejection event not emitted on ubuntu runners; macos/linux divergence undiagnosed — must investigate before linux support"
)]
#[tokio::test]
async fn evidence_probe_script_in_event_is_hard_capped() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "capped-probe-script");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;
    let script = format!("printf green\\n# {}", "x".repeat(100_000));

    register_issue_probe_call(
        &json!({
            "script": script,
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "rationale": "large rejected reproduction"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        3,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    let events: Vec<Value> = std::fs::read_to_string(journal.path().join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let event_script = events
        .iter()
        .find(|event| event["type"] == "evidence.probe.rejected")
        .expect("rejected probe event")["payload"]["script"]
        .as_str()
        .unwrap();
    assert!(event_script.starts_with("printf green\\n# "));
    assert!(event_script.contains("truncated"));
    assert!(event_script.chars().count() <= PROBE_SCRIPT_EVENT_LIMIT);
}

#[tokio::test]
async fn evidence_tool_rejected_verdicts_increment_and_third_bypasses() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "rejected-probes");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    let mut attempts = 0;

    let pre_green = register_issue_probe_call(
        &json!({
            "script": "printf 'already green\\n'",
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "rationale": "does not actually reproduce"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        1,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();
    assert!(pre_green.contains("did NOT fail on the current code"));
    assert!(pre_green.contains("already green"));
    assert_eq!(evidence.failed_registrations, 1);

    let infra_red = register_issue_probe_call(
        &json!({
            "script": "printf 'ModuleNotFoundError: no module named dependency\\n' >&2; exit 1",
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stderr",
            "rationale": "environment failure must not count as code red"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        2,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();
    assert!(infra_red.contains("environment reason (`ModuleNotFoundError`)"));
    assert!(infra_red.contains("Do not grind on package installation"));
    assert!(infra_red.contains("ModuleNotFoundError"));
    assert_eq!(evidence.failed_registrations, 2);

    let toggle = journal.path().join("flaky-toggle");
    let flaky_script = format!(
        "if [ -e '{}' ]; then rm -f '{}'; printf 'green\\n'; else touch '{}'; printf 'BUG_PRESENT\\n'; fi",
        toggle.display(),
        toggle.display(),
        toggle.display()
    );
    let flaky = register_issue_probe_call(
        &json!({
            "script": flaky_script,
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stdout",
            "rationale": "deliberately nondeterministic for classification coverage"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        3,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();
    assert!(flaky.contains("red once and green once"));
    assert!(flaky.contains("Make it deterministic"));
    assert!(flaky.contains("BUG_PRESENT"));
    assert!(flaky.contains("evidence gate is now advisory"));
    assert_eq!(attempts, 3);
    assert_eq!(evidence.failed_registrations, 3);
    assert!(evidence.bypassed);

    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert_eq!(events.matches("evidence.probe.rejected").count(), 3);
    assert!(events.contains("\"verdict\":\"pre_green\""));
    assert!(events.contains("\"verdict\":\"infra_red\""));
    assert!(events.contains("\"infra_signature\":\"ModuleNotFoundError\""));
    assert!(events.contains("\"verdict\":\"flaky\""));
    assert!(events.contains("\"type\":\"evidence.gate.bypassed\""));
    assert!(events.contains("\"attempt\":3"));
}

#[tokio::test]
async fn evidence_junk_registrations_do_not_bypass_an_accepted_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "junk-after-red");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "printf 'BUG_PRESENT\n'",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;
    let mut attempts = 0;

    for turn in 2..=4 {
        register_issue_probe_call(
            "not-json",
            &mut evidence,
            &mut attempts,
            workspace.path(),
            &journal.path().join("probes"),
            turn,
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &mut recorder,
        )
        .await
        .unwrap();
    }

    assert_eq!(evidence.failed_registrations, 3);
    assert!(!evidence.bypassed);
    assert!(evidence.probe.is_some());
    assert_eq!(evidence.ready(), Err(EvidenceDenial::NoEditYet));
}

#[test]
fn evidence_edit_shell_exec_is_never_blocked_without_a_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let evidence = EvidenceState::new(EvidenceGate::On);
    let targets = vec![workspace.path().join("shell-created.txt")];

    assert!(!evidence_edit_should_block(
        "shell_exec",
        &targets,
        workspace.path(),
        &evidence,
    ));
}

#[derive(Clone)]
struct EvidenceEditShellProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceEditShellProvider {
    async fn next_turn(
        &self,
        _messages: &[crate::provider::ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(crate::provider::ProviderResponse {
                text: "run the reproduction from the shell".into(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "evidence-shell".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'shell allowed\\n' > shell-created.txt"
                        })
                        .to_string(),
                    },
                }],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
            });
        }
        Ok(crate::provider::ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: Some(crate::provider::FinishReason::Stop),
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn evidence_edit_shell_exec_runs_when_gate_on_without_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut options =
        task_test_run_options(workspace.path(), journal.path(), "evidence-shell", vec![]);
    options.evidence_gate = EvidenceGate::On;

    run_solo(
        EvidenceEditShellProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("shell-created.txt")).unwrap(),
        "shell allowed\n"
    );
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-shell").events_path)
            .unwrap();
    assert!(!events.contains("evidence.edit.blocked"));
}

#[derive(Clone)]
struct EvidenceShellEditAfterProbeProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceShellEditAfterProbeProvider {
    async fn next_turn(
        &self,
        _messages: &[crate::provider::ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(evidence_completion_response(vec![
                evidence_completion_register_call(
                    "register-shell-edit-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                ToolCall {
                    id: "shell-edit-target".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'fixed\\n' > target.txt"
                        })
                        .to_string(),
                    },
                },
            ]));
        }
        Ok(evidence_completion_response(Vec::new()))
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn evidence_liveness_shell_exec_edit_advances_epoch() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let mut options = task_test_run_options(
        workspace.path(),
        journal.path(),
        "evidence-shell-edit",
        vec![],
    );
    options.evidence_gate = EvidenceGate::On;

    let result = run_solo(
        EvidenceShellEditAfterProbeProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target.txt")).unwrap(),
        "fixed\n"
    );
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-shell-edit").events_path)
            .unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"edit_epoch\":1"));
    assert!(events.contains("\"green_epoch\":1"));
}

#[derive(Clone)]
struct EvidenceDirtyRewriteProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for EvidenceDirtyRewriteProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let tool_calls = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => vec![
                evidence_completion_register_call(
                    "register-dirty-rewrite-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                ToolCall {
                    id: "make-dirty-green".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'fixed\\n' > target.txt"
                        })
                        .to_string(),
                    },
                },
            ],
            1 => vec![ToolCall {
                id: "rewrite-same-dirty-file-red".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "shell_exec".into(),
                    arguments: json!({
                        "command": "printf 'buggy again\\n' > target.txt"
                    })
                    .to_string(),
                },
            }],
            _ => Vec::new(),
        };
        Ok(evidence_completion_response(tool_calls))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("evidence-dirty-rewrite")
    }
}

#[tokio::test]
async fn evidence_liveness_shell_exec_rewrite_of_dirty_file_invalidates_green() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=AgentLoom Test",
            "-c",
            "user.email=agentloom@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ])
        .current_dir(workspace.path())
        .status()
        .unwrap();
    assert!(commit.success());
    let mut options = task_test_run_options(
        workspace.path(),
        journal.path(),
        "evidence-dirty-rewrite",
        Vec::new(),
    );
    options.evidence_gate = EvidenceGate::On;
    options.max_turns = 3;

    let result = run_solo(
        EvidenceDirtyRewriteProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target.txt")).unwrap(),
        "buggy again\n"
    );
    let events = std::fs::read_to_string(
        RunPaths::new(journal.path(), "evidence-dirty-rewrite").events_path,
    )
    .unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"type\":\"evidence.probe.still_red\""));
    assert!(events.contains("\"edit_epoch\":2"));
    assert!(events.contains("\"green_epoch\":null"));
    assert!(events.contains("\"reason\":\"evidence_probe_still_red\""));
}

#[derive(Clone)]
struct EvidenceUnverifiableWorkspaceProvider {
    calls: Arc<AtomicUsize>,
    saw_feedback: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for EvidenceUnverifiableWorkspaceProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let tool_calls = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => vec![
                evidence_completion_register_call(
                    "register-unverifiable-workspace-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                ToolCall {
                    id: "make-unverifiable-workspace-green".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'fixed\\n' > target.txt"
                        })
                        .to_string(),
                    },
                },
            ],
            1 => vec![ToolCall {
                id: "break-git-fingerprint".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "shell_exec".into(),
                    arguments: json!({
                        "command": "chmod 000 target.txt"
                    })
                    .to_string(),
                },
            }],
            _ => {
                let saw_feedback = messages.iter().any(|message| {
                    message.content.as_deref().is_some_and(|content| {
                        content.contains("harness cannot verify the workspace state")
                            && content.contains("cannot confirm the fix")
                    })
                });
                self.saw_feedback
                    .store(usize::from(saw_feedback), Ordering::SeqCst);
                Vec::new()
            }
        };
        Ok(evidence_completion_response(tool_calls))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("evidence-unverifiable-workspace")
    }
}

#[tokio::test]
async fn evidence_unverifiable_workspace_invalidates_green_not_keeps_it() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=AgentLoom Test",
            "-c",
            "user.email=agentloom@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ])
        .current_dir(workspace.path())
        .status()
        .unwrap();
    assert!(commit.success());
    let saw_feedback = Arc::new(AtomicUsize::new(0));
    let mut options = task_test_run_options(
        workspace.path(),
        journal.path(),
        "evidence-unverifiable-workspace",
        Vec::new(),
    );
    options.evidence_gate = EvidenceGate::On;
    options.max_turns = 3;

    let result = run_solo(
        EvidenceUnverifiableWorkspaceProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_feedback: saw_feedback.clone(),
        },
        options,
    )
    .await
    .unwrap();

    let events = std::fs::read_to_string(
        RunPaths::new(journal.path(), "evidence-unverifiable-workspace").events_path,
    )
    .unwrap();
    assert_ne!(result.outcome, RunOutcome::Completed);
    assert_eq!(saw_feedback.load(Ordering::SeqCst), 1);
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"type\":\"evidence.workspace.unverifiable\""));
    assert!(events.contains("\"edit_epoch\":2"));
    assert!(events.contains("\"green_epoch\":1"));
    assert!(events.contains("\"reason\":\"evidence_stale_green\""));
}

#[derive(Clone)]
struct EvidenceEditBlockedProvider {
    calls: Arc<AtomicUsize>,
    saw_guidance: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceEditBlockedProvider {
    async fn next_turn(
        &self,
        messages: &[crate::provider::ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(crate::provider::ProviderResponse {
                text: "edit before registering".into(),
                reasoning: String::new(),
                tool_calls: vec![crate::provider::ToolCall {
                    id: "evidence-blocked-edit".into(),
                    call_type: "function".into(),
                    function: crate::provider::FunctionCall {
                        name: "fs_edit".into(),
                        arguments: json!({
                            "path": "target.txt",
                            "old_string": "buggy",
                            "new_string": "fixed"
                        })
                        .to_string(),
                    },
                }],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
            });
        }
        let saw_guidance = messages.iter().any(|message| {
            message.role == "tool"
                && message.content.as_deref().is_some_and(|content| {
                    content.contains("Blocked: you have no confirmed-red reproduction yet")
                        && content.contains("Call register_issue_probe first")
                })
        });
        self.saw_guidance
            .store(usize::from(saw_guidance), Ordering::SeqCst);
        Ok(crate::provider::ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: Some(crate::provider::FinishReason::Stop),
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn evidence_edit_fs_edit_is_blocked_without_probe_and_emits_guidance() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let saw_guidance = Arc::new(AtomicUsize::new(0));
    let mut options =
        task_test_run_options(workspace.path(), journal.path(), "evidence-block", vec![]);
    options.evidence_gate = EvidenceGate::On;

    run_solo(
        EvidenceEditBlockedProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_guidance: saw_guidance.clone(),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(saw_guidance.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target.txt")).unwrap(),
        "buggy\n"
    );
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-block").events_path)
            .unwrap();
    assert!(events.contains("\"type\":\"evidence.edit.blocked\""));
    assert!(events.contains("\"tool\":\"fs_edit\""));
    assert!(events.contains("\"outcome\":\"require_probe\""));
    assert!(events.contains("target.txt"));
}

#[derive(Clone)]
struct EvidenceEditAcceptedProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::provider::ProviderClient for EvidenceEditAcceptedProvider {
    async fn next_turn(
        &self,
        _messages: &[crate::provider::ChatMessage],
        _tools: &[serde_json::Value],
        _events: &mut EventRecorder,
    ) -> Result<crate::provider::ProviderResponse> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(crate::provider::ProviderResponse {
                text: "register, inspect, and edit".into(),
                reasoning: String::new(),
                tool_calls: vec![
                    crate::provider::ToolCall {
                        id: "evidence-register".into(),
                        call_type: "function".into(),
                        function: crate::provider::FunctionCall {
                            name: "register_issue_probe".into(),
                            arguments: json!({
                                "script": "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; else printf 'fixed\\n'; fi",
                                "command": "sh {probe}",
                                "red_marker": "BUG_PRESENT",
                                "marker_stream": "stdout",
                                "rationale": "target remains buggy"
                            })
                            .to_string(),
                        },
                    },
                    crate::provider::ToolCall {
                        id: "evidence-read".into(),
                        call_type: "function".into(),
                        function: crate::provider::FunctionCall {
                            name: "fs_read".into(),
                            arguments: json!({"path": "target.txt"}).to_string(),
                        },
                    },
                    crate::provider::ToolCall {
                        id: "evidence-allowed-edit".into(),
                        call_type: "function".into(),
                        function: crate::provider::FunctionCall {
                            name: "fs_edit".into(),
                            arguments: json!({
                                "path": "target.txt",
                                "old_string": "buggy",
                                "new_string": "fixed"
                            })
                            .to_string(),
                        },
                    },
                ],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
            });
        }
        Ok(crate::provider::ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: Some(crate::provider::FinishReason::Stop),
        })
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        task_test_caps()
    }
}

#[tokio::test]
async fn evidence_edit_registered_probe_allows_fs_edit_and_auto_reruns_green() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut options =
        task_test_run_options(workspace.path(), journal.path(), "evidence-allowed", vec![]);
    options.evidence_gate = EvidenceGate::On;

    run_solo(
        EvidenceEditAcceptedProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target.txt")).unwrap(),
        "fixed\n"
    );
    let events =
        std::fs::read_to_string(RunPaths::new(journal.path(), "evidence-allowed").events_path)
            .unwrap();
    assert!(!events.contains("evidence.edit.blocked"));
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"edit_epoch\":1"));
    assert!(events.contains("\"green_epoch\":1"));
}

#[test]
fn evidence_edit_gate_allows_probe_off_and_bypassed_states() {
    let workspace = tempfile::tempdir().unwrap();
    let targets = vec![workspace.path().join("target.txt")];

    let mut with_probe = EvidenceState::new(EvidenceGate::On);
    with_probe.accept_probe(ProbeManifest {
        probe_id: "probe".into(),
        script_sha256: "hash".into(),
        script: "printf BUG".into(),
        script_path: PathBuf::from("${TMPDIR:-/tmp}/agentloom-probes/test/probe.sh"),
        command: "sh ${TMPDIR:-/tmp}/agentloom-probes/test/probe.sh".into(),
        red_oracle: RedOracle {
            marker: "BUG".into(),
            stream: MarkerStream::Any,
        },
        rationale: "test".into(),
        registered_turn: 1,
    });
    assert!(!evidence_edit_should_block(
        "fs_edit",
        &targets,
        workspace.path(),
        &with_probe,
    ));

    let off = EvidenceState::new(EvidenceGate::Off);
    assert!(!evidence_edit_should_block(
        "fs_edit",
        &targets,
        workspace.path(),
        &off,
    ));

    let mut bypassed = EvidenceState::new(EvidenceGate::On);
    for _ in 0..MAX_FAILED_REGISTRATIONS {
        bypassed.note_registration_failure();
    }
    assert!(!evidence_edit_should_block(
        "fs_edit",
        &targets,
        workspace.path(),
        &bypassed,
    ));
}

async fn evidence_edit_register_probe(
    script: &str,
    workspace: &std::path::Path,
    journal: &std::path::Path,
    evidence: &mut EvidenceState,
    recorder: &mut EventRecorder,
) {
    let mut attempts = 0;
    let feedback = register_issue_probe_call(
        &json!({
            "script": script,
            "command": "sh {probe}",
            "red_marker": "BUG_PRESENT",
            "marker_stream": "stdout",
            "rationale": "exercise the evidence edit lifecycle"
        })
        .to_string(),
        evidence,
        &mut attempts,
        workspace,
        &journal.join("probes"),
        1,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        recorder,
    )
    .await
    .unwrap();
    assert!(feedback.contains("Probe confirmed RED"), "{feedback}");
    assert!(evidence.probe.is_some());
}

#[tokio::test]
async fn evidence_edit_success_increments_epoch_and_green_probe_counts() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-green");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; else printf 'fixed\\n'; fi",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "fixed\n").unwrap();
    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(evidence.edit_epoch, 1);
    assert_eq!(evidence.green_epoch, Some(1));
    assert!(feedback.contains("now PASSES"));
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.green\""));
    assert!(events.contains("\"outcome\":\"green\""));
    assert!(events.contains("\"edit_epoch\":1"));
    assert!(events.contains("\"green_epoch\":1"));
}

#[tokio::test]
async fn evidence_edit_still_red_clears_green_and_emits_output() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-red");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "printf 'BUG_PRESENT still broken\\n'",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(evidence.edit_epoch, 1);
    assert_eq!(evidence.green_epoch, None);
    assert!(feedback.contains("still FAILS"));
    assert!(feedback.contains("still broken"));
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.still_red\""));
}

#[tokio::test]
async fn evidence_edit_second_edit_invalidates_old_green_on_infra() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-stale");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "case \"$(cat target.txt)\" in buggy*) printf 'BUG_PRESENT\\n';; fixed*) printf 'fixed\\n';; *) printf 'ModuleNotFoundError: missing dependency\\n' >&2; exit 1;; esac",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "fixed\n").unwrap();
    rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap();
    assert_eq!(evidence.green_epoch, Some(1));

    std::fs::write(workspace.path().join("target.txt"), "infra\n").unwrap();
    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        3,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(evidence.edit_epoch, 2);
    assert_eq!(evidence.green_epoch, Some(1));
    assert_ne!(evidence.green_epoch, Some(evidence.edit_epoch));
    assert!(feedback.contains("environment problem"));
    assert!(feedback.contains("Do not grind on package installation"));
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.infra\""));
    assert!(events.contains("\"signature\":\"ModuleNotFoundError\""));
    assert!(events.contains("\"edit_epoch\":2"));
    assert!(events.contains("\"green_epoch\":1"));
}

#[tokio::test]
async fn evidence_liveness_repeated_infra_rerun_discards_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "infra-discard");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'ModuleNotFoundError: missing dependency\n' >&2; exit 1; fi",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "infra-one\n").unwrap();
    let first = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(first.contains("environment problem"));
    assert!(evidence.probe.is_some());

    std::fs::write(workspace.path().join("target.txt"), "infra-two\n").unwrap();
    let second = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        3,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert!(second.contains("no longer evidence and has been discarded"));
    assert!(second.contains("Register a new one"));
    assert!(evidence.probe.is_none());
    assert_eq!(evidence.failed_registrations, 1);
    assert_eq!(evidence.green_epoch, None);
}

#[tokio::test]
async fn evidence_baseline_refreshes_after_legit_edit() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let mut recorder = evidence_tool_recorder(journal.path(), "baseline-refresh");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "fixed\n").unwrap();
    rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap();

    let mut attempts = 0;
    let feedback = register_issue_probe_call(
        &json!({
            "script": "if grep -q fixed target.txt; then printf 'NEW_BUG\n'; fi",
            "command": "sh {probe}",
            "red_marker": "NEW_BUG",
            "marker_stream": "stdout",
            "rationale": "register again after a legitimate edit"
        })
        .to_string(),
        &mut evidence,
        &mut attempts,
        workspace.path(),
        &journal.path().join("probes"),
        3,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut recorder,
    )
    .await
    .unwrap();

    assert!(feedback.contains("Probe confirmed RED"), "{feedback}");
    assert!(!feedback.contains("modified the workspace"));
    assert!(evidence.probe.is_some());
}

#[tokio::test]
async fn evidence_edit_workspace_mutation_is_not_green_and_keeps_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(workspace.path())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-mutated");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; else printf 'probe write\\n' >> probe-mutated.txt; fi",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;

    std::fs::write(workspace.path().join("target.txt"), "fixed\n").unwrap();
    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(evidence.edit_epoch, 1);
    assert_eq!(evidence.green_epoch, None);
    assert!(evidence.probe.is_some());
    assert!(feedback.contains("wrote to the workspace"));
    assert!(feedback.contains("does not count as green"));
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\":\"evidence.probe.workspace_mutated\""));
    assert!(events.contains("\"outcome\":\"workspace_mutated\""));
}

#[tokio::test]
async fn evidence_edit_off_skips_epoch_probe_and_events_entirely() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "edit-off");
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence_edit_register_probe(
        "printf 'BUG_PRESENT\\n'",
        workspace.path(),
        journal.path(),
        &mut evidence,
        &mut recorder,
    )
    .await;
    evidence.mode = EvidenceGate::Off;

    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        workspace.path(),
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        2,
        &mut recorder,
    )
    .await
    .unwrap();

    assert_eq!(feedback, None);
    assert_eq!(evidence.edit_epoch, 0);
    assert_eq!(evidence.green_epoch, None);
    let events = std::fs::read_to_string(journal.path().join("events.jsonl")).unwrap();
    assert!(!events.contains("evidence.probe.green"));
    assert!(!events.contains("evidence.probe.still_red"));
    assert!(!events.contains("evidence.probe.infra"));
    assert!(!events.contains("evidence.probe.workspace_mutated"));
}

#[tokio::test]
async fn evidence_gate_off_skips_workspace_edit_detection() {
    let journal = tempfile::tempdir().unwrap();
    let mut recorder = evidence_tool_recorder(journal.path(), "off-no-status");
    let mut evidence = EvidenceState::new(EvidenceGate::Off);
    evidence.probe = Some(evidence_completion_probe());
    let missing_workspace = journal.path().join("does-not-exist");

    let feedback = rerun_evidence_after_edit(
        &mut evidence,
        &missing_workspace,
        ISSUE_PROBE_TIMEOUT_S,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        1,
        &mut recorder,
    )
    .await
    .unwrap();

    assert_eq!(feedback, None);
    assert_eq!(evidence.edit_epoch, 0);
    assert_eq!(evidence.green_epoch, None);
}

fn passing_criteria() -> Vec<crate::goal::Criterion> {
    crate::goal::parse_criteria(&["cmd: true".into()]).unwrap()
}

#[test]
fn make_search_backend_exa_builds() {
    use crate::config::SearchChoice;

    let _ = super::make_search_backend(&SearchChoice::Exa {
        api_key: "k".into(),
    });
}

#[test]
fn guardrail_summary_mcp_gate_shows_server_tool_args() {
    let workspace = std::path::Path::new("/tmp/ws");
    let shell_args = serde_json::json!({ "command": "ls -la" }).to_string();
    assert_eq!(
        guardrail_summary("shell_exec", &shell_args, workspace),
        "ls -la"
    );

    let args = format!(
        "{{\"title\":\"trusted gate\",\"body\":\"{}\"}}",
        "x".repeat(100)
    );
    let summary = guardrail_summary("mcp__github__create_issue", &args, workspace);

    assert!(summary.contains("github"));
    assert!(summary.contains("create_issue"));
    assert!(summary.contains("trusted gate"));
    assert!(summary.ends_with('…'));
    assert!(summary.len() < args.len());
}

#[test]
fn memory_lookup_registry_respects_memory_enabled() {
    use crate::config::SearchChoice;

    fn names(registry: ToolRegistry) -> Vec<String> {
        registry
            .definitions()
            .into_iter()
            .filter_map(|d| d["function"]["name"].as_str().map(ToString::to_string))
            .collect()
    }

    let disabled = names(build_default_registry(&SearchChoice::Ddg, false));
    assert!(!disabled.iter().any(|name| name == "memory_lookup"));

    let enabled = names(build_default_registry(&SearchChoice::Ddg, true));
    assert!(enabled.iter().any(|name| name == "memory_lookup"));
}

fn assert_each_assistant_tool_call_has_exactly_one_tool_result(messages: &[ChatMessage]) {
    let mut tool_call_ids = Vec::new();
    for message in messages
        .iter()
        .filter(|message| message.role == "assistant")
    {
        if let Some(tool_calls) = &message.tool_calls {
            for tool_call in tool_calls {
                tool_call_ids.push(tool_call.id.as_str());
            }
        }
    }
    assert!(
        !tool_call_ids.is_empty(),
        "test must include assistant tool calls"
    );

    for tool_call_id in tool_call_ids {
        let matching_tool_results = messages
            .iter()
            .filter(|message| {
                message.role == "tool" && message.tool_call_id.as_deref() == Some(tool_call_id)
            })
            .count();
        assert_eq!(
            matching_tool_results, 1,
            "tool_call_id {tool_call_id} must have exactly one tool result"
        );
    }
}

#[test]
fn network_tool_gate_decisions() {
    use super::{network_tool_gate, NetworkGate};
    use crate::goal::NetworkPolicy::{Off, On};
    // 不要联网的工具：永远执行。
    assert!(matches!(
        network_tool_gate(false, Off, 99, 5),
        NetworkGate::Execute
    ));
    // 要联网 + 联网关：拒。
    assert!(matches!(
        network_tool_gate(true, Off, 0, 5),
        NetworkGate::RefuseNetworkOff
    ));
    // 要联网 + 联网开 + 没超上限：执行（prior=4 -> 这是第5次）。
    assert!(matches!(
        network_tool_gate(true, On, 4, 5),
        NetworkGate::Execute
    ));
    // 要联网 + 联网开 + 超上限：拒（prior=5 -> 这是第6次）。
    assert!(matches!(
        network_tool_gate(true, On, 5, 5),
        NetworkGate::RefuseCap
    ));
}

#[test]
fn verify_reflex_debt_counts_only_successful_invalidating_tools() {
    let mut debt = 0usize;
    verify_reflex_record_debt(&mut debt, true, true);
    assert_eq!(debt, 1);

    verify_reflex_record_debt(&mut debt, true, false);
    assert_eq!(debt, 1);

    verify_reflex_record_debt(&mut debt, false, true);
    assert_eq!(debt, 1);
}

#[test]
fn verify_reflex_threshold_boundaries_and_k_zero() {
    let criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    let goal = GoalState::new("obj", criteria);
    let progress = crate::run_progress::RunProgress::default();

    assert!(!verify_reflex_should_run(0, 99, &goal, &progress));
    assert!(!verify_reflex_should_run(2, 1, &goal, &progress));
    assert!(verify_reflex_should_run(2, 2, &goal, &progress));
    assert!(verify_reflex_should_run(2, 3, &goal, &progress));
}

#[test]
fn verify_reflex_requires_approved_verifiable_criterion() {
    let goal = GoalState::new("obj", Vec::new());
    let progress = crate::run_progress::RunProgress::default();
    assert!(!verify_reflex_should_run(1, 1, &goal, &progress));

    let mut criteria = crate::goal::parse_criteria(&["judge: check manually".into()]).unwrap();
    criteria[0].approval = crate::goal::Approval::Approved;
    let goal = GoalState::new("obj", criteria);
    assert!(!verify_reflex_should_run(1, 1, &goal, &progress));
}

#[test]
fn verify_reflex_runs_after_one_mutating_edit_when_ripple_candidates_open() {
    let mut criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    criteria[0].approval = crate::goal::Approval::Approved;
    let goal = GoalState::new("obj", criteria);

    let mut progress = crate::run_progress::RunProgress::default();
    assert!(!verify_reflex_should_run(3, 1, &goal, &progress));

    progress.set_ripple_candidates(vec![crate::run_progress::RippleCandidate {
        symbol: "RunOptions".into(),
        missing_field: Some("journal_root".into()),
        compiler_reported_sites: vec!["src/lib.rs:10".into()],
        extra_candidate_sites: vec!["tests/integration.rs:20".into()],
        truncated: false,
    }]);

    assert!(verify_reflex_should_run(3, 1, &goal, &progress));
    assert!(!verify_reflex_should_run(0, 1, &goal, &progress));
    assert!(!verify_reflex_should_run(3, 0, &goal, &progress));
}

#[test]
fn verify_reflex_debt_clears_after_validation_and_accumulates_across_turns() {
    let criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    let goal = GoalState::new("obj", criteria);
    let progress = crate::run_progress::RunProgress::default();
    let mut debt = 0usize;

    verify_reflex_record_debt(&mut debt, true, true);
    assert!(!verify_reflex_should_run(2, debt, &goal, &progress));

    verify_reflex_record_debt(&mut debt, true, true);
    assert!(verify_reflex_should_run(2, debt, &goal, &progress));

    verify_reflex_clear_debt(&mut debt);
    assert_eq!(debt, 0);
    assert!(!verify_reflex_should_run(2, debt, &goal, &progress));
}

#[tokio::test]
async fn edit_turn_injects_new_compile_error_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    write_compile_feedback_crate(
        dir.path(),
        "include!(\"generated.rs\");\n",
        "pub fn generated() -> i32 {\n    1\n}\n",
    );
    let run_id = "run_compile_feedback_new_error";
    let opts = compile_feedback_options(dir.path().to_path_buf(), run_id);
    let saw_feedback = Arc::new(AtomicUsize::new(0));

    let result = run_solo(
        IntroduceCompileErrorProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_feedback: saw_feedback.clone(),
        },
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    assert_eq!(saw_feedback.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn read_only_turn_does_not_probe() {
    let dir = tempfile::tempdir().unwrap();
    write_compile_feedback_crate(
        dir.path(),
        "include!(\"generated.rs\");\n",
        "pub fn generated() -> i32 {\n    1\n}\n",
    );
    let run_id = "run_compile_feedback_read_only";
    let opts = compile_feedback_options(dir.path().to_path_buf(), run_id);
    let saw_clean_second_turn = Arc::new(AtomicUsize::new(0));

    let result = run_solo(
        ReadOnlyCompileProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_clean_second_turn: saw_clean_second_turn.clone(),
        },
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(saw_clean_second_turn.load(Ordering::SeqCst), 1);
    let paths = RunPaths::new(dir.path(), run_id);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(
        !events.contains("immediate_"),
        "read-only turn should not run an immediate diagnostic probe: {events}"
    );
}

#[tokio::test]
async fn pre_existing_error_not_repeated() {
    let dir = tempfile::tempdir().unwrap();
    write_compile_feedback_crate(
        dir.path(),
        "pub fn baseline_type_error() -> i32 {\n    \"baseline\"\n}\ninclude!(\"generated.rs\");\n",
        "pub fn generated() -> i32 {\n    1\n}\n",
    );
    let run_id = "run_compile_feedback_pre_existing";
    let opts = compile_feedback_options(dir.path().to_path_buf(), run_id);
    let saw_feedback = Arc::new(AtomicUsize::new(0));

    let result = run_solo(
        PreExistingCompileErrorProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_feedback: saw_feedback.clone(),
        },
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    assert_eq!(saw_feedback.load(Ordering::SeqCst), 1);
}

struct RejectScriptProvider;

#[async_trait::async_trait]
impl ProviderClient for RejectScriptProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        if messages.iter().any(|message| {
            message.role == "tool"
                && message.tool_call_id.as_deref() == Some("call_reject_write")
                && message.content.as_deref() == Some("permission denied by user")
        }) {
            return Ok(ProviderResponse {
                text: "Continuing without the denied write.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "I will write the requested file.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_reject_write".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "fs_write".to_string(),
                    arguments: json!({ "path": "demo.txt", "content": "VALUE=1\n" }).to_string(),
                },
            }],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "test".to_string(),
            model_id: "reject-script".to_string(),
            supports_streaming: false,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: false,
            max_context_tokens: None,
            output_token_limit: None,
            server_side_search: false,
        }
    }
}

struct ToolOutcomeRecoverProvider;

#[async_trait::async_trait]
impl ProviderClient for ToolOutcomeRecoverProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let seed_read_done = messages.iter().any(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("seed_read")
        });
        if !seed_read_done {
            return Ok(ProviderResponse {
                text: "Reading the file before editing.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "seed_read".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "fs_read".to_string(),
                        arguments: json!({"path": "target.txt"}).to_string(),
                    },
                }],
                finish_reason: None,
            });
        }

        if messages.iter().any(|message| {
            message.role == "tool"
                && message.tool_call_id.as_deref() == Some("bad_edit")
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("no match"))
        }) {
            if messages.iter().any(|message| {
                message.role == "tool" && message.tool_call_id.as_deref() == Some("good_edit")
            }) {
                return Ok(ProviderResponse {
                    text: "The file has been corrected.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                });
            }

            return Ok(ProviderResponse {
                text: "Retrying with the exact string.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "good_edit".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "fs_edit".to_string(),
                        arguments: json!({
                            "path": "target.txt",
                            "old_string": "alpha beta",
                            "new_string": "alpha gamma"
                        })
                        .to_string(),
                    },
                }],
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Trying an edit with the wrong old string.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "bad_edit".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "fs_edit".to_string(),
                    arguments: json!({
                        "path": "target.txt",
                        "old_string": "not present",
                        "new_string": "alpha gamma"
                    })
                    .to_string(),
                },
            }],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("tool-outcome-recover")
    }
}

struct RuntimeErrProvider;

#[async_trait::async_trait]
impl ProviderClient for RuntimeErrProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        assert!(
            !messages.iter().any(|message| message.role == "tool"),
            "runtime Err from execute must not be converted into a tool message"
        );
        Ok(ProviderResponse {
            text: "Calling the runtime-failing tool.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "runtime_err".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "runtime_err_tool".to_string(),
                    arguments: "{}".to_string(),
                },
            }],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("runtime-err")
    }
}

struct CheckpointFatalWriteThenFollowUpProvider {
    calls: Arc<AtomicUsize>,
    saw_tool_feedback: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for CheckpointFatalWriteThenFollowUpProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => {
                assert!(
                    !messages.iter().any(|message| message.role == "tool"),
                    "fatal checkpoint failures must abort before any tool result reaches the model"
                );
                Ok(ProviderResponse {
                    text: "Attempting the write first.".to_string(),
                    reasoning: String::new(),
                    tool_calls: vec![test_tool_call(
                        "call_checkpoint_write",
                        "fs_write",
                        json!({
                            "path": "nested/out.txt",
                            "content": "hello from checkpoint fatal path\n"
                        }),
                    )],
                    finish_reason: None,
                })
            }
            1 => {
                if messages.iter().any(|message| {
                    message.role == "tool"
                        && message.tool_call_id.as_deref() == Some("call_checkpoint_write")
                }) {
                    self.saw_tool_feedback.store(1, Ordering::SeqCst);
                }
                Ok(ProviderResponse {
                    text: "Following up after the write.".to_string(),
                    reasoning: String::new(),
                    tool_calls: vec![test_tool_call(
                        "call_after_checkpoint_failure",
                        "fs_read",
                        json!({ "path": "nested/out.txt" }),
                    )],
                    finish_reason: None,
                })
            }
            _ => panic!("checkpoint fatal provider should stop after the follow-up turn"),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("checkpoint-fatal-fs-write")
    }
}

struct TwoReadCallsProvider;

#[async_trait::async_trait]
impl ProviderClient for TwoReadCallsProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "Reading two files.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![
                ToolCall {
                    id: "call_read_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "fs_read".to_string(),
                        arguments: json!({ "path": "first.txt" }).to_string(),
                    },
                },
                ToolCall {
                    id: "call_read_2".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "fs_read".to_string(),
                        arguments: json!({ "path": "second.txt" }).to_string(),
                    },
                },
            ],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("two-read-calls")
    }
}

struct ScopeChangeWithTrailingToolProvider;

#[async_trait::async_trait]
impl ProviderClient for ScopeChangeWithTrailingToolProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "Proposing a scope change and another tool.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![
                test_tool_call(
                    "call_scope_trailing",
                    "propose_scope_change",
                    json!({
                        "kind": "scope",
                        "detail": "Include the trailing tool pairing case"
                    }),
                ),
                test_tool_call(
                    "call_read_after_scope",
                    "fs_read",
                    json!({ "path": "after_scope.txt" }),
                ),
            ],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("scope-change-with-trailing-tool")
    }
}

struct BlockWithQuestionsProvider;

#[async_trait::async_trait]
impl ProviderClient for BlockWithQuestionsProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "I am blocked and need user input.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                "call_block_questions",
                "block_with_questions",
                json!({
                    "blocked_reason": "criterion c1 looks wrong",
                    "questions": ["Should c1 still be required?", "Can I use a fixture instead?"],
                    "agent_diagnosis": "criteria",
                    "failed_criteria": ["c1"],
                    "evidence_refs": ["events:12"]
                }),
            )],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("block-with-questions")
    }
}

struct BlockWithQuestionsTrailingToolProvider;

#[async_trait::async_trait]
impl ProviderClient for BlockWithQuestionsTrailingToolProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "I am blocked and also asked for a read.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![
                test_tool_call(
                    "call_block_trailing",
                    "block_with_questions",
                    json!({
                        "blocked_reason": "missing user decision",
                        "questions": ["Which path should I take?"]
                    }),
                ),
                test_tool_call(
                    "call_read_after_block",
                    "fs_read",
                    json!({ "path": "after_block.txt" }),
                ),
            ],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("block-with-questions-trailing-tool")
    }
}

struct StopOnPoll {
    run_id: String,
    stop_on: usize,
    polls: usize,
}

impl ControlSource for StopOnPoll {
    fn poll(&mut self) -> Option<ControlCommand> {
        self.polls += 1;
        if self.polls == self.stop_on {
            Some(ControlCommand::Stop {
                run_id: self.run_id.clone(),
            })
        } else {
            None
        }
    }

    fn recv_approval(&mut self, _timeout: Duration) -> crate::control::ControlRecv {
        crate::control::ControlRecv::Closed
    }
}

struct PairingAssertResumeProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for PairingAssertResumeProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        validate_tool_pairing(messages).expect("resume must repair conversation before provider");
        assert!(
            !messages.iter().any(|message| {
                message.role == "assistant"
                    && message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| calls.iter().any(|call| call.id == "legacy_unpaired"))
            }),
            "unpaired legacy assistant must be dropped before resume prompt"
        );
        Ok(ProviderResponse {
            text: "resumed cleanly".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("resume-pairing")
    }
}

struct ResumeContractReflexProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ResumeContractReflexProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Restoring progress with a write.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_resume_write",
                    "fs_write",
                    json!({ "path": "touched.txt", "content": "ok\n" }),
                )],
                finish_reason: None,
            });
        }

        assert!(
            messages.iter().any(|message| {
                message.role == "tool"
                    && message.tool_call_id.as_deref() == Some("call_resume_write")
            }),
            "resume provider must see the write result before finalizing"
        );
        Ok(ProviderResponse {
            text: "Finished after the resumed write.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("resume-contract-reflex")
    }
}

struct ResumeRealignProvider {
    calls: Arc<AtomicUsize>,
    seen_messages: Arc<Mutex<Vec<ChatMessage>>>,
}

#[async_trait::async_trait]
impl ProviderClient for ResumeRealignProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            *self.seen_messages.lock().unwrap() = messages.to_vec();
            return Ok(ProviderResponse {
                text: "Applying the realigned contract.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_realign_write",
                    "fs_write",
                    json!({ "path": "realigned.txt", "content": "ok\n" }),
                )],
                finish_reason: None,
            });
        }

        assert!(
            messages.iter().any(|message| {
                message.role == "tool"
                    && message.tool_call_id.as_deref() == Some("call_realign_write")
            }),
            "realign provider must see the write result before finalizing"
        );
        Ok(ProviderResponse {
            text: "Finished after the realigned write.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("resume-realign")
    }
}

struct CompleteImmediatelyProvider;

#[async_trait::async_trait]
impl ProviderClient for CompleteImmediatelyProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: "Finished.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("complete-immediately")
    }
}

struct StateFrameCaptorProvider {
    seen_messages: Arc<Mutex<Vec<ChatMessage>>>,
}

#[async_trait::async_trait]
impl ProviderClient for StateFrameCaptorProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        *self.seen_messages.lock().unwrap() = messages.to_vec();
        Ok(ProviderResponse {
            text: "Finished.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("state-frame-captor")
    }
}

struct UpdateWorkingStateProvider {
    calls: Arc<AtomicUsize>,
    seen_systems: Arc<Mutex<Vec<String>>>,
    offered_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl ProviderClient for UpdateWorkingStateProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.offered_tools.lock().unwrap().push(tool_names(tools));
        self.seen_systems.lock().unwrap().push(
            messages
                .first()
                .and_then(|message| message.content.clone())
                .unwrap_or_default(),
        );
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Updating my working notes, then editing.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![
                    test_tool_call(
                        "call_update_ledger",
                        "update_working_state",
                        json!({
                            "plan": "do X",
                            "known": ["target.txt exists"],
                            "unknown": ["whether final text is enough"],
                            "next_intent": "edit target.txt"
                        }),
                    ),
                    test_tool_call(
                        "call_write_after_ledger",
                        "fs_write",
                        json!({ "path": "target.txt", "content": "updated\n" }),
                    ),
                ],
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Finished after using the working notes.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("update-working-state")
    }
}

struct ProposeCriterionThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ProposeCriterionThenFinalProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Proposing a criterion.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_new_criterion",
                    "propose_criterion",
                    json!({ "claim": "new criterion", "check_cmd": "true" }),
                )],
                finish_reason: None,
            });
        }

        assert!(
            messages.iter().any(|message| {
                message.role == "tool"
                    && message.tool_call_id.as_deref() == Some("call_new_criterion")
                    && message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains("criterion approved"))
            }),
            "provider must see the approved criterion tool result"
        );
        Ok(ProviderResponse {
            text: "Criterion accepted; finishing.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("propose-criterion-then-final")
    }
}

struct ProposeCriterionObjectSuccessThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ProposeCriterionObjectSuccessThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Proposing a criterion with object success.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_object_success_criterion",
                    "propose_criterion",
                    json!({
                        "claim": "new criterion",
                        "check_cmd": "true",
                        "success": { "exit_zero": true }
                    }),
                )],
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after object success criterion.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("propose-criterion-object-success-then-final")
    }
}

struct ProposeCriterionMalformedArgsThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ProposeCriterionMalformedArgsThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Proposing a criterion with malformed JSON args.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_malformed_args".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "propose_criterion".to_string(),
                        arguments: r#"{"claim": "bad criterion", "check_cmd": "tru"#.to_string(),
                    },
                }],
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after malformed args rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("propose-criterion-malformed-args-then-final")
    }
}

struct UpdateWorkingStateMalformedArgsThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for UpdateWorkingStateMalformedArgsThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Updating working state with malformed JSON args.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_malformed_args".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "update_working_state".to_string(),
                        arguments: r#"{"summary": "work in progress"#.to_string(),
                    },
                }],
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after malformed args rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("update-working-state-malformed-args-then-final")
    }
}

struct BlockWithQuestionsMalformedArgsThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for BlockWithQuestionsMalformedArgsThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Blocking with malformed JSON args.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_malformed_args".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "block_with_questions".to_string(),
                        arguments: r#"{"blocked_reason": "stuck", "questions": ["#.to_string(),
                    },
                }],
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after malformed args rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("block-with-questions-malformed-args-then-final")
    }
}

struct ProposeScopeChangeMalformedArgsThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ProposeScopeChangeMalformedArgsThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Proposing a scope change with malformed JSON args.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_malformed_args".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "propose_scope_change".to_string(),
                        arguments: r#"{"kind": "scope", "detail": "expand"#.to_string(),
                    },
                }],
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after malformed args rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("propose-scope-change-malformed-args-then-final")
    }
}

struct DisallowedProposeCriterionThenFinalProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for DisallowedProposeCriterionThenFinalProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(ProviderResponse {
                text: "Hard-calling a disabled criterion tool.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_disallowed_criterion",
                    "propose_criterion",
                    json!({ "claim": "new criterion", "check_cmd": "true" }),
                )],
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Continuing after disabled tool rejection.".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("disallowed-propose-criterion-then-final")
    }
}

struct PureReaderProvider {
    calls: Arc<AtomicUsize>,
    offered_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl ProviderClient for PureReaderProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.offered_tools.lock().unwrap().push(tool_names(tools));
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: "Reading another file.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_read_{call}"),
                "fs_read",
                json!({ "path": format!("read_{call}.txt") }),
            )],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("pure-reader")
    }
}

struct RepeatReaderProvider {
    calls: Arc<AtomicUsize>,
    offered_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl ProviderClient for RepeatReaderProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.offered_tools.lock().unwrap().push(tool_names(tools));
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        // 每轮读同一个文件 → 第二轮起 RepeatRead(无新信息)
        Ok(ProviderResponse {
            text: "Re-reading the same file.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_repeat_{call}"),
                "fs_read",
                json!({ "path": "same.txt" }),
            )],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("repeat-reader")
    }
}

struct EditingProvider {
    calls: Arc<AtomicUsize>,
    offered_tools: Arc<Mutex<Vec<Vec<String>>>>,
    edits_before_final: usize,
}

#[async_trait::async_trait]
impl ProviderClient for EditingProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.offered_tools.lock().unwrap().push(tool_names(tools));
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call >= self.edits_before_final {
            return Ok(ProviderResponse {
                text: "Finished after concrete edits.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Editing the target file.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![
                test_tool_call(
                    &format!("call_read_{call}"),
                    "fs_read",
                    json!({"path": "target.txt"}),
                ),
                test_tool_call(
                    &format!("call_edit_{call}"),
                    "fs_edit",
                    json!({
                        "path": "target.txt",
                        "old_string": format!("v{call}"),
                        "new_string": format!("v{}", call + 1),
                    }),
                ),
            ],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("editing-run")
    }
}

struct ShellEditingProvider {
    calls: Arc<AtomicUsize>,
    shell_turns_before_final: usize,
}

#[async_trait::async_trait]
impl ProviderClient for ShellEditingProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call >= self.shell_turns_before_final {
            return Ok(ProviderResponse {
                text: "Finished after shell work.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Changing the workspace with shell.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_shell_{call}"),
                "shell_exec",
                json!({
                    "command": format!("printf 'shell {call}\\n' >> shell.log"),
                }),
            )],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("shell-editing-run")
    }
}

struct NovelShellProvider {
    calls: Arc<AtomicUsize>,
    shell_turns_before_final: usize,
}

#[async_trait::async_trait]
impl ProviderClient for NovelShellProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call >= self.shell_turns_before_final {
            return Ok(ProviderResponse {
                text: "Finished after novel shell work.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Running distinct shell-only work.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_novel_shell_{call}"),
                "shell_exec",
                json!({
                    "command": format!("true # novel shell {call}"),
                }),
            )],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("novel-shell-run")
    }
}

struct RepeatShellProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for RepeatShellProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: "repeat same red command".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("repeat_shell_{call}"),
                "shell_exec",
                json!({ "command": "printf 'red\\n'; exit 7" }),
            )],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("repeat-shell")
    }
}

struct ReadThenEditRecoveryProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ReadThenEditRecoveryProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let names = tool_names(tools);
        match call {
            0 => Ok(ProviderResponse {
                text: "Reading the target before editing.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_recovery_read_target",
                    "fs_read",
                    json!({"path": "target.txt"}),
                )],
                finish_reason: None,
            }),
            1..=6 => Ok(ProviderResponse {
                text: "Re-reading before editing.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    &format!("call_recovery_read_{call}"),
                    "fs_read",
                    json!({"path": "target.txt"}),
                )],
                finish_reason: None,
            }),
            7 => {
                for tool in ["grep", "ls", "glob"] {
                    assert!(
                        names.iter().all(|name| name != tool),
                        "narrow threshold should hide {tool}"
                    );
                }
                assert!(
                    names.iter().any(|name| name == "fs_read"),
                    "narrow threshold should keep fs_read visible"
                );
                Ok(ProviderResponse {
                    text: "Editing once exploration is narrowed.".to_string(),
                    reasoning: String::new(),
                    tool_calls: vec![test_tool_call(
                        "call_recovery_edit",
                        "fs_edit",
                        json!({
                            "path": "target.txt",
                            "old_string": "v0",
                            "new_string": "v1",
                        }),
                    )],
                    finish_reason: None,
                })
            }
            8 => {
                assert!(
                    names.iter().any(|name| name == "fs_read"),
                    "successful edit should clear the streak and re-offer fs_read"
                );
                assert!(
                    names.iter().any(|name| name == "grep"),
                    "successful edit should clear the streak and re-offer exploration tools"
                );
                Ok(ProviderResponse {
                    text: "Finished after tools recovered.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                })
            }
            _ => panic!("provider should have completed on turn 9"),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("read-then-edit-recovery")
    }
}

struct IntroduceCompileErrorProvider {
    calls: Arc<AtomicUsize>,
    saw_feedback: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for IntroduceCompileErrorProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ProviderResponse {
                text: "Introducing a compile error after reading the file.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![
                    test_tool_call(
                        "call_read_generated",
                        "fs_read",
                        json!({ "path": "src/generated.rs" }),
                    ),
                    test_tool_call(
                        "call_write_generated_bad",
                        "fs_write",
                        json!({
                            "path": "src/generated.rs",
                            "content": "pub fn generated() -> i32 {\n    missing_added()\n}\n"
                        }),
                    ),
                ],
                finish_reason: None,
            }),
            1 => {
                assert!(
                    messages_contain(messages, "新增编译错")
                        && messages_contain(messages, "src/generated.rs")
                        && messages_contain(messages, "missing_added"),
                    "provider should see immediate compile feedback, got: {messages:#?}"
                );
                self.saw_feedback.store(1, Ordering::SeqCst);
                Ok(ProviderResponse {
                    text: "Saw the immediate compile feedback.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                })
            }
            // K3：criteria 仍不满足 → turn 2 用完预算后，run_loop 在发 NeedsDecision 前多给
            // 一轮收尾发言（不影响本测试真正要钉的断言：saw_feedback 早在 call 1 就已记下）。
            2 => Ok(ProviderResponse {
                text: "Wrapping up.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            }),
            _ => panic!("compile feedback provider should finish on turn 2 (+ one K3 wrapup call)"),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("introduce-compile-error")
    }
}

struct ReadOnlyCompileProvider {
    calls: Arc<AtomicUsize>,
    saw_clean_second_turn: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ReadOnlyCompileProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ProviderResponse {
                text: "Only reading the generated file.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_read_generated_only",
                    "fs_read",
                    json!({ "path": "src/generated.rs" }),
                )],
                finish_reason: None,
            }),
            1 => {
                assert!(
                    !messages_contain(messages, "新增编译错"),
                    "read-only turn must not inject immediate compile feedback: {messages:#?}"
                );
                self.saw_clean_second_turn.store(1, Ordering::SeqCst);
                Ok(ProviderResponse {
                    text: "Finished after read-only turn.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                })
            }
            _ => panic!("read-only provider should finish on turn 2"),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("read-only-compile")
    }
}

struct PreExistingCompileErrorProvider {
    calls: Arc<AtomicUsize>,
    saw_feedback: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for PreExistingCompileErrorProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ProviderResponse {
                text: "Adding a second compile error after reading the file.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![
                    test_tool_call(
                        "call_read_generated_with_baseline",
                        "fs_read",
                        json!({ "path": "src/generated.rs" }),
                    ),
                    test_tool_call(
                        "call_write_generated_with_new_error",
                        "fs_write",
                        json!({
                            "path": "src/generated.rs",
                            "content": "pub fn generated() -> i32 {\n    missing_added()\n}\n"
                        }),
                    ),
                ],
                finish_reason: None,
            }),
            1 => {
                assert!(
                    messages_contain(messages, "新增编译错")
                        && messages_contain(messages, "src/generated.rs")
                        && messages_contain(messages, "missing_added"),
                    "provider should see the newly introduced compile error: {messages:#?}"
                );
                assert!(
                    !compile_feedback_messages_contain(messages, "src/lib.rs")
                        && !compile_feedback_messages_contain(messages, "baseline_type_error"),
                    "pre-existing baseline diagnostic must not be repeated: {messages:#?}"
                );
                self.saw_feedback.store(1, Ordering::SeqCst);
                Ok(ProviderResponse {
                    text: "Saw only the new compile feedback.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                })
            }
            // K3：criteria 仍不满足 → turn 2 用完预算后，run_loop 在发 NeedsDecision 前多给
            // 一轮收尾发言（不影响本测试真正要钉的断言：saw_feedback 早在 call 1 就已记下）。
            2 => Ok(ProviderResponse {
                text: "Wrapping up.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            }),
            _ => {
                panic!("pre-existing-error provider should finish on turn 2 (+ one K3 wrapup call)")
            }
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("pre-existing-compile-error")
    }
}

struct RuntimeErrTool;

#[async_trait::async_trait]
impl crate::tools::Tool for RuntimeErrTool {
    fn name(&self) -> &str {
        "runtime_err_tool"
    }

    fn definition(&self) -> Value {
        json!({ "type": "function", "function": { "name": "runtime_err_tool" } })
    }

    fn mutates(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        _ctx: &mut crate::tools::ToolContext<'_>,
        _call: &ToolCall,
    ) -> Result<crate::tools::ToolOutcome> {
        Err(HarnessError::Runtime("runtime err from tool".to_string()))
    }
}

fn test_tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

fn tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str().map(ToString::to_string))
        .collect()
}

fn messages_contain(messages: &[ChatMessage], needle: &str) -> bool {
    messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains(needle))
    })
}

fn compile_feedback_messages_contain(messages: &[ChatMessage], needle: &str) -> bool {
    messages.iter().any(|message| {
        message.role == "user"
            && message.content.as_deref().is_some_and(|content| {
                content.starts_with("新增编译错") && content.contains(needle)
            })
    })
}

fn write_compile_feedback_crate(dir: &Path, lib_rs: &str, generated_rs: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"compile_feedback_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), lib_rs).unwrap();
    std::fs::write(dir.join("src/generated.rs"), generated_rs).unwrap();
}

fn compile_feedback_options(workspace: PathBuf, run_id: &str) -> RunOptions {
    let mut opts = options(workspace, "compile feedback");
    opts.permission = PermissionPolicy::Allow;
    opts.native_search_enabled = false;
    opts.memory_enabled = false;
    opts.max_turns = 2;
    opts.run_id = Some(run_id.to_string());
    opts.criteria =
        crate::goal::parse_criteria(&["cmd: cargo check --manifest-path Cargo.toml".into()])
            .unwrap();
    opts
}

struct PreflightRejectProvider {
    model_id: &'static str,
    tool_call_id: &'static str,
    tool_name: &'static str,
    arguments: String,
    expected_feedback: &'static [&'static str],
}

#[async_trait::async_trait]
impl ProviderClient for PreflightRejectProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        if messages.iter().any(|message| {
            message.role == "tool"
                && message.tool_call_id.as_deref() == Some(self.tool_call_id)
                && message.content.as_deref().is_some_and(|content| {
                    self.expected_feedback
                        .iter()
                        .all(|expected| content.contains(expected))
                })
        }) {
            return Ok(ProviderResponse {
                text: "Continuing after preflight rejection.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            });
        }

        Ok(ProviderResponse {
            text: "Calling a tool that should be rejected during preflight.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: self.tool_call_id.to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: self.tool_name.to_string(),
                    arguments: self.arguments.clone(),
                },
            }],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities(self.model_id)
    }
}

fn test_capabilities(model_id: &str) -> ProviderCapabilities {
    ProviderCapabilities {
        provider_id: "test-local".to_string(),
        model_id: model_id.to_string(),
        supports_streaming: false,
        supports_reasoning_deltas: false,
        supports_tool_calling: true,
        supports_images: false,
        supports_computer_use: false,
        supports_shell_tool: false,
        max_context_tokens: None,
        output_token_limit: None,
        server_side_search: false,
    }
}

fn options(workspace: PathBuf, prompt: &str) -> RunOptions {
    RunOptions {
        prompt: prompt.to_string(),
        workspace: workspace.clone(),
        provider_id: "mock".into(),
        model: "mock-model".into(),
        client_session_id: None,
        output_mode: OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        evidence_gate: EvidenceGate::Off,
        permission: PermissionPolicy::Ask,
        network: crate::goal::NetworkPolicy::On,
        fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: true,
        search: crate::config::SearchChoice::Ddg,
        max_turns: 3,
        run_id: Some("run_test".into()),
        context_files: Vec::new(),
        criteria: Vec::new(),
        contract_policy: crate::guardrails::ContractPolicy::TrustAll,
        max_eval_attempts: 3,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        journal_root: workspace.clone(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

#[tokio::test]
async fn mcp_wire_bad_server_failure_downgrades_and_run_completes() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_mcp_wire_bad_server");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "finish despite bad mcp");
    opts.run_id = Some("run_mcp_wire_bad_server".into());
    opts.network = crate::goal::NetworkPolicy::On;
    opts.criteria = passing_criteria();
    opts.mcp_servers = vec![McpServerConfig {
        name: "badsrv".into(),
        command: "/nonexistent/mcp-xyz".into(),
        url: None,
        args: Vec::new(),
        env: Default::default(),
        trusted: false,
    }];
    let mut recorder = EventRecorder::new(
        "run_mcp_wire_bad_server",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), opts.criteria.clone());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        CompleteImmediatelyProvider,
        opts,
        paths.clone(),
        "run_mcp_wire_bad_server",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| {
        event["type"] == "mcp.server.failed"
            && event["payload"]["server"] == "badsrv"
            && event["payload"]["phase"] == "connect"
    }));
}

#[tokio::test]
async fn run_injects_terrain_with_crate_root() {
    let dir = tempfile::tempdir().unwrap();
    let crate_dir = dir.path().join("harness-agent");
    std::fs::create_dir_all(&crate_dir).unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]\nname = \"myagent\"\n",
    )
    .unwrap();

    let mut opts = options(dir.path().to_path_buf(), "capture terrain");
    opts.max_turns = 1;
    opts.run_id = Some("run_terrain_wire".into());
    opts.memory_enabled = false;
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = StateFrameCaptorProvider {
        seen_messages: seen_messages.clone(),
    };

    run_solo(provider, opts).await.unwrap();

    let seen = seen_messages.lock().unwrap().clone();
    assert!(seen.iter().any(|message| {
        message.content.as_deref().is_some_and(|content| {
            content.contains("Working directory") && content.contains("harness-agent")
        })
    }));
}

#[tokio::test]
async fn provider_sees_state_frame_without_mutating_canonical_messages() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_state_frame_wire");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "provider sees frame");
    opts.max_turns = 1;
    opts.run_id = Some("run_state_frame_wire".into());
    let mut recorder = EventRecorder::new(
        "run_state_frame_wire",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let base_system = messages[0].content.clone().unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = StateFrameCaptorProvider {
        seen_messages: seen_messages.clone(),
    };
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    run_loop(
        provider,
        opts,
        paths,
        "run_state_frame_wire",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    let seen = seen_messages.lock().unwrap().clone();
    let provider_system = seen[0].content.as_deref().unwrap();
    assert!(provider_system.contains(&base_system));
    assert!(provider_system.contains("Current state"));
    assert!(provider_system.contains("Objective: provider sees frame"));
    assert_eq!(messages[0].content.as_deref(), Some(base_system.as_str()));
}

#[test]
fn unmet_summary_includes_failed_and_uncertain() {
    use crate::goal::{Approval, AuthoredBy, Criterion, CriterionStatus, GoalState, Verifier};

    let mk = |id: &str, status: CriterionStatus| Criterion {
        id: id.into(),
        claim: "c".into(),
        scope: None,
        authored_by: AuthoredBy::User,
        approval: Approval::Approved,
        verifier: Verifier::Judgmental { rubric: "r".into() },
        status,
        evidence_ref: Some("ev".into()),
    };
    let goal = GoalState::new(
        "obj",
        vec![
            mk("c1", CriterionStatus::Failed),
            mk("c2", CriterionStatus::Uncertain),
        ],
    );

    let summary = unmet_summary(&goal);

    assert!(
        summary.contains("c1") && summary.contains("FAILED"),
        "应含 Failed"
    );
    assert!(
        summary.contains("c2") && summary.contains("UNCERTAIN"),
        "应含 Uncertain"
    );
}

#[test]
fn unmet_summary_renders_no_internal_markers() {
    use crate::goal::{parse_criteria, CriterionStatus, GoalState};

    let mut goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());
    goal.contract.criteria[0].status = CriterionStatus::Failed;
    goal.contract.criteria[0].evidence_ref =
        Some("check_cmd[user] exit=Some(101) passed=false cmd=cargo test stderr=...".into());
    let s = unmet_summary(&goal);
    assert!(s.contains("c1") && s.contains("FAILED"));
    assert!(s.contains("验收检查") && s.contains("cargo test"));
    assert!(!s.contains("check_cmd[") && !s.contains("cmd="));
}

#[test]
fn make_control_source_uses_stdin_jsonl_for_jsonl_mode() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_test");
    paths.create_dirs().unwrap();
    std::fs::write(&paths.interrupt_path, b"interrupt\n").unwrap();

    let mut control = make_control_source(ControlInputKind::StdinJsonl, &paths, "run_test");

    assert!(control.poll().is_none());
}

#[test]
fn make_control_source_uses_sentinel_for_human_and_silent_modes() {
    for _output_mode in [OutputMode::Human, OutputMode::Silent] {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "run_test");
        paths.create_dirs().unwrap();
        std::fs::write(&paths.interrupt_path, b"interrupt\n").unwrap();

        let mut control = make_control_source(ControlInputKind::Sentinel, &paths, "run_test");

        assert!(matches!(
            control.poll(),
            Some(ControlCommand::Stop { run_id }) if run_id == "run_test"
        ));
    }
}

#[tokio::test]
async fn rejected_mutating_tool_is_reported_to_model_and_run_continues() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_test");
    paths.create_dirs().unwrap();
    let opts = options(dir.path().to_path_buf(), "agentic loop");
    let mut recorder = EventRecorder::new(
        "run_test",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(vec![ControlCommand::Reject {
        run_id: "run_test".into(),
        approval_id: "approval_call_reject_write".into(),
    }]);
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let result = run_loop(
        RejectScriptProvider,
        opts.clone(),
        paths.clone(),
        "run_test",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await;

    assert!(result.is_ok());
    assert!(!dir.path().join("demo.txt").exists());
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_reject_write")
            && message.content.as_deref() == Some("permission denied by user")
    }));
    assert!(
        messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count()
            >= 2
    );

    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"type\":\"tool.failed\""));
    assert!(events.contains("\"tool_call_id\":\"call_reject_write\""));
    assert!(!events.contains("\"type\":\"artifact.created\""));
}

#[tokio::test]
async fn disallowed_inline_propose_criterion_is_rejected_without_decision_and_run_continues() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try to negotiate criteria");
    opts.disallowed_tools
        .insert("propose_criterion".to_string());
    opts.max_turns = 2;
    opts.criteria = passing_criteria();
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        DisallowedProposeCriterionThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(!events.contains("\"type\":\"goal.change.proposed\""));
    assert!(!events.contains("\"type\":\"run.needs_decision\""));
    assert!(events.contains("\"type\":\"tool.failed\""));
    assert!(events.contains("propose_criterion"));
    assert!(events.contains("disabled for this run"));

    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_disallowed_criterion")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("disabled for this run"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}

#[tokio::test]
async fn propose_criterion_object_success_form_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try object success criterion");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        ProposeCriterionObjectSuccessThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"type\":\"goal.change.proposed\""));

    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}

#[tokio::test]
async fn propose_criterion_malformed_args_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try malformed args criterion");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        ProposeCriterionMalformedArgsThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    // 两轮都没能满足空标准的「Stop 收尾」条件 → 预算耗尽；K3 多给一轮收尾发言，
    // provider 因此多被调用 1 次（3 = 2 个正常轮 + 1 个收尾轮）。
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    // The malformed call should have a tool rejection message
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_malformed_args")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("malformed arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}

#[tokio::test]
async fn update_working_state_malformed_args_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try malformed working state");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        UpdateWorkingStateMalformedArgsThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    // 两轮都没能满足空标准的「Stop 收尾」条件 → 预算耗尽；K3 多给一轮收尾发言，
    // provider 因此多被调用 1 次（3 = 2 个正常轮 + 1 个收尾轮）。
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_malformed_args")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("malformed arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}

#[tokio::test]
async fn block_with_questions_malformed_args_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try malformed block");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        BlockWithQuestionsMalformedArgsThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    // 两轮都没能满足空标准的「Stop 收尾」条件 → 预算耗尽；K3 多给一轮收尾发言，
    // provider 因此多被调用 1 次（3 = 2 个正常轮 + 1 个收尾轮）。
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_malformed_args")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("malformed arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}

#[tokio::test]
async fn propose_scope_change_malformed_args_does_not_crash_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "try malformed scope change");
    opts.max_turns = 2;
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        ProposeScopeChangeMalformedArgsThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Failed);
    // 两轮都没能满足空标准的「Stop 收尾」条件 → 预算耗尽；K3 多给一轮收尾发言，
    // provider 因此多被调用 1 次（3 = 2 个正常轮 + 1 个收尾轮）。
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_malformed_args")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("malformed arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
}

#[tokio::test]
async fn block_with_questions_escalates_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_block_with_questions";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "need user input");
    opts.max_turns = 5;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(
        opts.prompt.clone(),
        crate::goal::parse_criteria(&["judge: c1 must hold".to_string()]).unwrap(),
    );
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        BlockWithQuestionsProvider,
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| {
            event["type"] == "run.needs_decision"
                && event["payload"]["reason"] == "blocked_questions"
        })
        .expect("run should emit blocked_questions needs_decision");
    assert_eq!(
        needs_decision["payload"]["blocked_reason"],
        "criterion c1 looks wrong"
    );
    assert_eq!(
        needs_decision["payload"]["questions"],
        json!([
            "Should c1 still be required?",
            "Can I use a fixture instead?"
        ])
    );
    assert_eq!(needs_decision["payload"]["contract_version"], 1);
    assert_eq!(needs_decision["payload"]["trigger"], "agent");
    assert_eq!(needs_decision["payload"]["agent_diagnosis"], "criteria");
    assert_eq!(needs_decision["payload"]["failed_criteria"], json!(["c1"]));
    assert_eq!(
        needs_decision["payload"]["evidence_refs"],
        json!(["events:12"])
    );
    assert_eq!(needs_decision["payload"]["attempts_summary"]["turns"], 1);
    assert!(!events.iter().any(|event| {
        event["type"] == "run.failed" && event["payload"]["error"] == "max_turns_exceeded"
    }));
}

#[tokio::test]
async fn block_with_questions_skips_trailing_tool_calls() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("after_block.txt"), "unused").unwrap();
    let run_id = "run_block_with_questions_trailing";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "blocked before read");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        BlockWithQuestionsTrailingToolProvider,
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);

    let block_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_block_trailing")
    });
    let block_content: Value =
        serde_json::from_str(block_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(block_content["status"], "blocked_questions");

    let trailing_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_read_after_block")
    });
    let trailing_content: Value =
        serde_json::from_str(trailing_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(trailing_content["status"], "skipped");
    assert_eq!(
        trailing_content["reason"],
        "superseded by blocked_questions"
    );
}

#[tokio::test]
async fn update_working_state_persists_and_feeds_frame() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_update_working_state";
    let paths = RunPaths::new(dir.path(), run_id);
    let mut opts = options(dir.path().to_path_buf(), "track working notes");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 2;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen_systems = Arc::new(Mutex::new(Vec::new()));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let result = run_solo_with_judge(
        UpdateWorkingStateProvider {
            calls: calls.clone(),
            seen_systems: seen_systems.clone(),
            offered_tools: offered_tools.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let offered = offered_tools.lock().unwrap();
    assert!(offered[0].iter().any(|name| name == "update_working_state"));

    let ledger = crate::journal::load_working_ledger(&paths.working_ledger_path);
    assert_eq!(ledger.plan.as_deref(), Some("do X"));
    assert_eq!(ledger.next_intent.as_deref(), Some("edit target.txt"));
    assert_eq!(ledger.known, vec!["target.txt exists"]);
    assert_eq!(ledger.unknown, vec!["whether final text is enough"]);
    assert!(ledger.applied.contains("call_update_ledger"));

    let seen = seen_systems.lock().unwrap();
    assert!(seen.len() >= 2);
    assert!(seen[1].contains("Your working notes"));
    assert!(seen[1].contains("plan: do X"));
    assert!(seen[1].contains("next: edit target.txt"));

    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);
    assert!(!saved.messages[0]
        .content
        .as_deref()
        .unwrap_or_default()
        .contains("Your working notes"));
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_write_after_ledger")
    }));
    let update_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_update_ledger")
    });
    let update_content: Value =
        serde_json::from_str(update_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(update_content["status"], "updated");
}

#[tokio::test]
async fn pure_reader_hits_no_edit_backstop_before_budget() {
    // 每轮读到新文件会持续清空 stale，但 K 仍按「距上次真编辑」兜底，避免无限读。
    let dir = tempfile::tempdir().unwrap();
    let max_turns = crate::adaptive_safety_net::Thresholds::DEFAULT.no_edit_backstop + 5;
    for index in 0..max_turns {
        std::fs::write(
            dir.path().join(format!("read_{index}.txt")),
            format!("content {index}"),
        )
        .unwrap();
    }
    let run_id = "run_pure_reader_no_edit_backstop";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "pure reader");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = max_turns;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        PureReaderProvider {
            calls: calls.clone(),
            offered_tools: offered_tools.clone(),
        },
        opts.clone(),
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        // K3：halt 前多给一轮收尾发言，这里的 provider 会再被调用一次（只取其 text，
        // 不执行它试图发起的工具调用）——calls 因此比 no_edit_backstop 阈值多 1。
        crate::adaptive_safety_net::Thresholds::DEFAULT.no_edit_backstop + 1,
        "每轮读新信息时应由 K 兜底在 backstop 附近停 + 1 轮收尾发言"
    );
    assert!(
        calls.load(Ordering::SeqCst) < opts.max_turns,
        "K 兜底必须早于 max_turns，避免跑满预算"
    );
    let snapshots = offered_tools.lock().unwrap();
    // K3 的收尾发言轮故意不给任何工具（逼模型只出文本），是最后一次 provider 调用；
    // 除它以外的每一轮都该仍然带着 fs_read（探索工具不该被砍）。
    let (wrapup_snapshot, normal_snapshots) = snapshots.split_last().expect("at least one call");
    assert!(
        wrapup_snapshot.is_empty(),
        "K3 收尾轮应以空工具集调用 provider：{wrapup_snapshot:?}"
    );
    assert!(
        normal_snapshots
            .iter()
            .all(|names| names.iter().any(|name| name == "fs_read")),
        "一直读到新信息 → 探索工具不该被砍"
    );
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| {
        event["type"] == "run.needs_decision" && event["payload"]["blocked_reason"] == "no_progress"
    }));
}

/// R1 对抗审补口：镜像 `pure_reader_hits_no_edit_backstop_before_budget`，唯一差别是
/// `disallowed_tools` 拿掉 fs_write/fs_edit（模拟被禁写工具的 lead）——K2 的接线（
/// run_loop.rs 里 `write_tools_offered` 那行表达式）此前完全没有测试盯着：把它变异成
/// 恒 `true` 之后跑全量单测依然 1099 绿，说明没人验过它真的被算对、真的被传进
/// `decide()`。这条测试直接钉死「写工具被禁时，no_edit_backstop 不该在 40 轮触发」——
/// `write_tools_offered` 若被错误算成 true（或没被接上 `decide()`），这条测试必红
/// （已手工验证：见下方"变异验证"记录，改动只在开发过程中临时做，未进最终 diff）。
///
/// 变异验证记录（2026-07-25 补测时手工做过，非自动化）：把 run_loop.rs 里
/// `let write_tools_offered = [...]` 那一行临时改成 `let write_tools_offered = true;`，
/// 跑本测试 → 在 no_edit_backstop(40) 附近被 halt，`calls` 远小于 max_turns，断言全部
/// 失败（红）；改回真实表达式后重跑 → 绿。证明这条测试确实在盯 K2 的接线，不是摆设。
#[tokio::test]
async fn pure_reader_with_write_tools_disallowed_never_hits_no_edit_backstop() {
    let dir = tempfile::tempdir().unwrap();
    // R1 要求 max_turns > 40（no_edit_backstop 阈值）：用同样 +10 的余量确认「一路跑到
    // 预算耗尽」而不是撞见其他边界。
    let max_turns = crate::adaptive_safety_net::Thresholds::DEFAULT.no_edit_backstop + 10;
    assert!(max_turns > 40, "R1 要求 max_turns 严格 > 40");
    for index in 0..max_turns {
        std::fs::write(
            dir.path().join(format!("read_{index}.txt")),
            format!("content {index}"),
        )
        .unwrap();
    }
    let run_id = "run_pure_reader_no_write_tools";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "pure reader, write tools disallowed",
    );
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = max_turns;
    opts.run_id = Some(run_id.to_string());
    // K2 的核心场景：结构上不可能写文件的 run（例如被 --disallow-tools 拿掉 fs_write/
    // fs_edit 的 lead）。
    opts.disallowed_tools.insert("fs_write".to_string());
    opts.disallowed_tools.insert("fs_edit".to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        PureReaderProvider {
            calls: calls.clone(),
            offered_tools: offered_tools.clone(),
        },
        opts.clone(),
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    // 关键断言（K2 若失效 = 这里必红）：不该在 no_edit_backstop(40) 附近停——必须一路跑满
    // max_turns，靠预算耗尽收场（+1 是 K3 的收尾发言轮）。
    assert_eq!(
        calls.load(Ordering::SeqCst),
        max_turns + 1,
        "写工具被禁时 no_edit_backstop 不该触发，run 必须撑到预算耗尽 + 1 轮收尾发言"
    );
    // offered_tools 快照里确认 fs_write/fs_edit 真的从没出现过（证明 disallowed_tools 生效，
    // 不是巧合撑到了预算耗尽）。
    let snapshots = offered_tools.lock().unwrap();
    assert!(
        snapshots.iter().all(|names| !names
            .iter()
            .any(|name| name == "fs_write" || name == "fs_edit")),
        "fs_write/fs_edit 不该出现在任何一轮的 offered tools 里"
    );
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    // P2（2026-07-26 更新此前 R1 断言）：这条 run 结构上不可能编辑（fs_write/fs_edit 被
    // disallow），从没编辑过、从没重置过 turns_since_last_real_edit——但也正因为「结构上不可能
    // 编辑」，不能拿「没编辑过」倒推 no_progress（P1 修完 MCP 型 lead 的安全网之后，这类
    // 无写工具的 run 打满预算是常态，不是卡住）。`budget_exhausted_blocked_reason` 现在收
    // `write_tools_offered` 入参，无写工具时恒落 `budget_exhausted_still_progressing`
    // （不是 halt 触发的——是 loop 跑满后 emit_budget_exhausted_needs_decision 报的）。
    assert_eq!(
        needs_decision["payload"]["blocked_reason"],
        "budget_exhausted_still_progressing"
    );
    assert_eq!(
        needs_decision["payload"]["turns_since_last_real_edit"],
        max_turns
    );
}

#[tokio::test]
async fn repeat_reader_with_no_new_info_gets_explore_truncated() {
    // 一直读同一个文件：urge 档只催不收工具，narrow 档收 grep/ls/glob 但保留 fs_read。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("same.txt"), "constant").unwrap();
    let run_id = "run_repeat_reader";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "repeat reader");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        RepeatReaderProvider {
            calls: calls.clone(),
            offered_tools: offered_tools.clone(),
        },
        opts.clone(),
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let snapshots = offered_tools.lock().unwrap();
    assert!(
        snapshots[crate::adaptive_safety_net::Thresholds::DEFAULT.urge]
            .iter()
            .any(|name| name == "grep"),
        "urge 档只出文案，不应收工具"
    );
    // K3 的收尾发言轮是最后一次 provider 调用，故意以空工具集发起（逼模型只出文本）——
    // 把它从「fs_read 该一直可见」这条检查里摘出去，其余每一轮仍必须能看到 fs_read。
    let (wrapup_snapshot, normal_snapshots) = snapshots.split_last().expect("at least one call");
    assert!(
        wrapup_snapshot.is_empty(),
        "K3 收尾轮应以空工具集调用 provider：{wrapup_snapshot:?}"
    );
    assert!(
        normal_snapshots
            .iter()
            .position(|names| names.iter().all(|name| name != "fs_read"))
            .is_none(),
        "fs_read 应在收窄态保持可见"
    );
    let first_without_grep = snapshots
        .iter()
        .position(|names| names.iter().all(|name| name != "grep"))
        .expect("无新信息又不动手 → narrow 阈值应收 grep");
    assert_eq!(
        first_without_grep,
        crate::adaptive_safety_net::Thresholds::DEFAULT.narrow + 1
    );
    for tool in ["grep", "ls", "glob"] {
        assert!(
            snapshots[first_without_grep]
                .iter()
                .all(|name| name != tool),
            "narrow 档应收 {tool}"
        );
    }
    assert!(
        snapshots[first_without_grep]
            .iter()
            .any(|name| name == "fs_read"),
        "narrow 档必须保留 fs_read"
    );
}

/// F1（opus 对抗审 Finding 1·T2）：镜像
/// `repeat_reader_with_no_new_info_gets_explore_truncated`，唯一差别是 `disallowed_tools`
/// 拿掉 fs_write/fs_edit（模拟被禁写工具的 lead——K2 的同一个场景）。对这类 run，
/// narrow_explore 不该摘 grep/ls/glob：fs_read 之外，「新颖读」正是它清零 stale 计数、
/// 避免 8 轮 halt 的仅剩自救手段之一，收窄探索工具是在它离 halt 只剩 2 轮时反而拿走
/// 一半自救工具（误掐的放大器，不是刹车）。这条测试钉死"grep/ls/glob 在无写工具的 run
/// 里、任何轮次都不该消失"——即便 stale 已经越过 narrow(6)/urge(4) 阈值。
#[tokio::test]
async fn repeat_reader_with_write_tools_disallowed_keeps_explore_tools_at_narrow() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("same.txt"), "constant").unwrap();
    let run_id = "run_repeat_reader_no_write_tools";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "repeat reader, write tools disallowed",
    );
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    // T2 的核心场景：结构上不可能写文件的 run（例如被 --disallow-tools 拿掉 fs_write/
    // fs_edit 的 lead）——与 K2/`pure_reader_with_write_tools_disallowed_...` 同一个禁写
    // 接线。
    opts.disallowed_tools.insert("fs_write".to_string());
    opts.disallowed_tools.insert("fs_edit".to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        RepeatReaderProvider {
            calls: calls.clone(),
            offered_tools: offered_tools.clone(),
        },
        opts.clone(),
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let snapshots = offered_tools.lock().unwrap();
    // K3 收尾轮以空工具集调用 provider（逼模型只出文本）——排除在「grep 该一直可见」之外，
    // 其余每一轮（含 stale 越过 narrow(6) 阈值之后）都必须仍能看到 grep/ls/glob。
    let (wrapup_snapshot, normal_snapshots) = snapshots.split_last().expect("at least one call");
    assert!(
        wrapup_snapshot.is_empty(),
        "K3 收尾轮应以空工具集调用 provider：{wrapup_snapshot:?}"
    );
    assert!(
        normal_snapshots.len() > crate::adaptive_safety_net::Thresholds::DEFAULT.narrow + 1,
        "run 应该跑过 narrow 阈值之后才收尾（否则这条测试没测到 narrow 档）: {} 轮",
        normal_snapshots.len()
    );
    for (turn_index, names) in normal_snapshots.iter().enumerate() {
        for tool in ["grep", "ls", "glob"] {
            assert!(
                names.iter().any(|name| name == tool),
                "无写工具的 run 在第 {turn_index} 轮不该摘 {tool}（narrow_explore 必须对齐 \
                 write_tools_offered=false）：{names:?}"
            );
        }
        assert!(
            names.iter().any(|name| name == "fs_read"),
            "fs_read 应在每一轮都可见"
        );
    }
}

#[test]
fn narrow_set_keeps_fs_read() {
    let narrowed: Vec<&str> = ["grep", "ls", "glob"].to_vec();
    assert!(!narrowed.contains(&"fs_read"));
}

#[test]
fn no_progress_threshold_constants_are_absolute() {
    assert_eq!(MIN_TASK_TURN_BUDGET, 40);
    assert_eq!(NO_PROGRESS_SOFT_TURNS, 4);
}

#[derive(Clone)]
struct FinishReasonProvider {
    calls: Arc<AtomicUsize>,
    first_finish_reason: crate::provider::FinishReason,
    saw_truncation_feedback: Arc<Mutex<bool>>,
    first_tool_call: bool,
}

#[async_trait::async_trait]
impl ProviderClient for FinishReasonProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if messages.iter().any(|message| {
            message.role == "user"
                && message.content.as_deref().is_some_and(|content| {
                    content.contains("输出长度上限截断") && content.contains("直接给出工具调用")
                })
        }) {
            *self.saw_truncation_feedback.lock().unwrap() = true;
        }
        if call == 0 && self.first_tool_call {
            return Ok(ProviderResponse {
                text: String::new(),
                reasoning: "ready to edit".into(),
                tool_calls: vec![ToolCall {
                    id: "write_1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "fs_write".into(),
                        arguments: json!({"path": "finish_reason_tool.txt", "content": "done"})
                            .to_string(),
                    },
                }],
                finish_reason: Some(crate::provider::FinishReason::ToolCalls),
            });
        }
        Ok(ProviderResponse {
            text: if call == 0 { "done" } else { "still done" }.into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: if call == 0 {
                Some(self.first_finish_reason.clone())
            } else {
                Some(crate::provider::FinishReason::Stop)
            },
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("finish-reason-test")
    }
}

fn finish_reason_provider(
    finish_reason: crate::provider::FinishReason,
    first_tool_call: bool,
) -> (FinishReasonProvider, Arc<AtomicUsize>, Arc<Mutex<bool>>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_truncation_feedback = Arc::new(Mutex::new(false));
    (
        FinishReasonProvider {
            calls: calls.clone(),
            first_finish_reason: finish_reason,
            saw_truncation_feedback: saw_truncation_feedback.clone(),
            first_tool_call,
        },
        calls,
        saw_truncation_feedback,
    )
}

#[tokio::test]
async fn empty_criteria_stop_with_text_completes_without_spinning() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) = finish_reason_provider(crate::provider::FinishReason::Stop, false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(dir.path().to_path_buf(), "empty criteria final text"),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"criteria_verified\":false"));
}

#[tokio::test]
async fn approved_passing_criterion_final_text_still_completes() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) = finish_reason_provider(crate::provider::FinishReason::Stop, false);
    let mut opts = options(dir.path().to_path_buf(), "passing criterion final text");
    opts.criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"criteria_verified\":true"));
}

#[tokio::test]
async fn empty_criteria_tool_call_executes_normally() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, _, _) = finish_reason_provider(crate::provider::FinishReason::ToolCalls, true);
    let mut opts = options(dir.path().to_path_buf(), "empty criteria tool call");
    opts.permission = PermissionPolicy::Allow;
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("finish_reason_tool.txt")).unwrap(),
        "done"
    );
}

#[tokio::test]
async fn length_finish_reason_never_completes_and_injects_direct_tool_feedback() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, saw_feedback) =
        finish_reason_provider(crate::provider::FinishReason::Length, false);
    let mut opts = options(dir.path().to_path_buf(), "truncated response");
    opts.criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert!(calls.load(Ordering::SeqCst) >= 2);
    assert!(*saw_feedback.lock().unwrap());
}

#[tokio::test]
async fn empty_criteria_length_does_not_complete_before_later_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, saw_feedback) =
        finish_reason_provider(crate::provider::FinishReason::Length, false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(
            dir.path().to_path_buf(),
            "truncated empty-criteria response",
        ),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(*saw_feedback.lock().unwrap());
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"turns\":2"));
    assert!(events.contains("\"criteria_verified\":false"));
}

#[tokio::test]
async fn rejected_completion_emits_observable_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) =
        finish_reason_provider(crate::provider::FinishReason::ToolCalls, false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(dir.path().to_path_buf(), "observable completion rejection"),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let mut rejected = events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("completion rejection should be observable")
        .clone();
    assert_eq!(rejected["payload"]["via"], "model_final_text");
    rejected["payload"]
        .as_object_mut()
        .expect("completion rejection payload should be an object")
        .remove("via");
    assert_eq!(
        rejected["payload"],
        json!({
            "reason": "empty_criteria_not_stopped",
            "finish_reason": "tool_calls",
            "text_len": 4,
            "tool_calls": 0,
            "criteria_count": 0,
            "turn": 1,
        })
    );
}

#[tokio::test]
async fn try_finalize_rejection_emits_null_response_metadata_and_preserves_via() {
    let dir = tempfile::tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let mut recorder = EventRecorder::new(
        "try_finalize_rejected",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new("engine finalize rejection", Vec::new());
    let mut eval_round = 0;
    let mut evidence = EvidenceState::new(EvidenceGate::Off);

    let outcome = try_finalize(
        &mut goal,
        &mut evidence,
        crate::guardrails::ContractPolicy::TrustAll,
        dir.path(),
        &crate::judge::NoopJudge,
        &mut recorder,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut eval_round,
        12,
        "engine_finalize",
    )
    .await
    .unwrap();

    assert_eq!(outcome, FinalizeOutcome::NotComplete);
    let events: Vec<Value> = std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rejected = events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("try_finalize rejection should be observable");
    assert_eq!(
        rejected["payload"],
        json!({
            "reason": "no_criteria",
            "finish_reason": null,
            "text_len": null,
            "tool_calls": null,
            "criteria_count": 0,
            "turn": 12,
            "via": "engine_finalize",
        })
    );
}

#[tokio::test]
async fn completion_rejection_via_distinguishes_model_text_and_engine_finalize_paths() {
    let model_dir = tempfile::tempdir().unwrap();
    let (provider, _, _) = finish_reason_provider(crate::provider::FinishReason::ToolCalls, false);
    run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(
            model_dir.path().to_path_buf(),
            "model completion rejection via",
        ),
    )
    .await
    .unwrap();
    let model_events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(model_dir.path(), "run_test").events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let model_via = model_events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("model final text rejection should be observable")["payload"]["via"]
        .clone();

    let engine_dir = tempfile::tempdir().unwrap();
    let engine_events_path = engine_dir.path().join("events.jsonl");
    let mut recorder = EventRecorder::new(
        "engine_finalize_rejected",
        None,
        Some(engine_dir.path().to_string_lossy().into_owned()),
        &engine_events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new("engine completion rejection via", Vec::new());
    let mut eval_round = 0;
    let mut evidence = EvidenceState::new(EvidenceGate::Off);
    let outcome = try_finalize(
        &mut goal,
        &mut evidence,
        crate::guardrails::ContractPolicy::TrustAll,
        engine_dir.path(),
        &crate::judge::NoopJudge,
        &mut recorder,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut eval_round,
        12,
        "engine_finalize",
    )
    .await
    .unwrap();
    assert_eq!(outcome, FinalizeOutcome::NotComplete);
    let engine_events: Vec<Value> = std::fs::read_to_string(engine_events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let engine_via = engine_events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("engine finalize rejection should be observable")["payload"]["via"]
        .clone();

    assert_eq!(model_via, "model_final_text");
    assert_eq!(engine_via, "engine_finalize");
    assert_ne!(model_via, engine_via);
}

#[tokio::test]
async fn other_finish_reason_payload_is_preserved_in_observability_events() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) =
        finish_reason_provider(crate::provider::FinishReason::Other("foo".into()), false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(
            dir.path().to_path_buf(),
            "observable provider finish reason",
        ),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let provider_turn = events
        .iter()
        .find(|event| event["type"] == "provider.turn.finished" && event["payload"]["turn"] == 1)
        .expect("provider turn should preserve the finish reason");
    let rejected = events
        .iter()
        .find(|event| event["type"] == "completion.rejected")
        .expect("completion rejection should preserve the finish reason");

    assert_eq!(provider_turn["payload"]["finish_reason"], "other:foo");
    assert_eq!(rejected["payload"]["finish_reason"], "other:foo");
}

fn evidence_completion_probe() -> ProbeManifest {
    ProbeManifest {
        probe_id: "completion-probe".into(),
        script_sha256: "completion-hash".into(),
        script: "printf BUG_PRESENT".into(),
        script_path: PathBuf::from("/tmp/agentloom-completion-probe.sh"),
        command: "sh /tmp/agentloom-completion-probe.sh".into(),
        red_oracle: RedOracle {
            marker: "BUG_PRESENT".into(),
            stream: MarkerStream::Any,
        },
        rationale: "completion gate test".into(),
        registered_turn: 1,
    }
}

async fn evidence_completion_try_finalize(
    evidence: &mut EvidenceState,
    run_id: &str,
) -> (FinalizeOutcome, Vec<Value>) {
    let dir = tempfile::tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(
        "engine evidence completion",
        crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
    );
    let mut eval_round = 0;

    let outcome = try_finalize(
        &mut goal,
        evidence,
        crate::guardrails::ContractPolicy::TrustAll,
        dir.path(),
        &crate::judge::NoopJudge,
        &mut recorder,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut eval_round,
        7,
        "engine_finalize",
    )
    .await
    .unwrap();
    let events = std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (outcome, events)
}

#[tokio::test]
async fn evidence_completion_off_preserves_engine_finalize_for_all_state_shapes() {
    let mut states = vec![EvidenceState::new(EvidenceGate::Off)];

    let mut no_edit = EvidenceState::new(EvidenceGate::Off);
    no_edit.accept_probe(evidence_completion_probe());
    states.push(no_edit);

    let mut still_red = EvidenceState::new(EvidenceGate::Off);
    still_red.accept_probe(evidence_completion_probe());
    still_red.note_edit();
    states.push(still_red);

    let mut green = EvidenceState::new(EvidenceGate::Off);
    green.accept_probe(evidence_completion_probe());
    green.note_edit();
    green.note_probe_green();
    states.push(green);

    let mut stale = EvidenceState::new(EvidenceGate::Off);
    stale.accept_probe(evidence_completion_probe());
    stale.note_edit();
    stale.note_probe_green();
    stale.note_edit();
    states.push(stale);

    let mut bypassed = EvidenceState::new(EvidenceGate::Off);
    bypassed.bypassed = true;
    states.push(bypassed);

    for (index, evidence) in states.iter_mut().enumerate() {
        let (outcome, events) =
            evidence_completion_try_finalize(evidence, &format!("evidence-off-{index}")).await;
        assert_eq!(outcome, FinalizeOutcome::Completed, "state={evidence:?}");
        assert!(events.iter().any(|event| event["type"] == "run.completed"));
        assert!(!events
            .iter()
            .any(|event| event["type"] == "completion.rejected"));
    }
}

#[tokio::test]
async fn evidence_completion_engine_finalize_denies_every_unready_state_with_epochs() {
    let mut cases = [
        (
            EvidenceState::new(EvidenceGate::On),
            "evidence_no_probe_registered",
            0,
            Value::Null,
        ),
        {
            let mut evidence = EvidenceState::new(EvidenceGate::On);
            evidence.accept_probe(evidence_completion_probe());
            (evidence, "evidence_no_edit_yet", 0, Value::Null)
        },
        {
            let mut evidence = EvidenceState::new(EvidenceGate::On);
            evidence.accept_probe(evidence_completion_probe());
            evidence.note_edit();
            (evidence, "evidence_probe_still_red", 1, Value::Null)
        },
        {
            let mut evidence = EvidenceState::new(EvidenceGate::On);
            evidence.accept_probe(evidence_completion_probe());
            evidence.note_edit();
            evidence.note_probe_green();
            evidence.note_edit();
            (evidence, "evidence_stale_green", 2, json!(1))
        },
    ];

    for (index, (evidence, reason, edit_epoch, green_epoch)) in cases.iter_mut().enumerate() {
        let (outcome, events) =
            evidence_completion_try_finalize(evidence, &format!("evidence-denied-{index}")).await;
        assert_eq!(outcome, FinalizeOutcome::NotComplete);
        let rejected = events
            .iter()
            .find(|event| event["type"] == "completion.rejected")
            .expect("evidence rejection should be observable");
        assert_eq!(rejected["payload"]["reason"], *reason);
        assert_eq!(rejected["payload"]["via"], "engine_finalize");
        assert_eq!(rejected["payload"]["edit_epoch"], *edit_epoch);
        assert_eq!(rejected["payload"]["green_epoch"], *green_epoch);
        assert!(!events.iter().any(|event| event["type"] == "run.completed"));
    }
}

#[tokio::test]
async fn evidence_liveness_completion_denials_eventually_release_gate() {
    let dir = tempfile::tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let mut recorder = EventRecorder::new(
        "completion-liveness",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(
        "completion liveness",
        crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
    );
    let mut evidence = EvidenceState::new(EvidenceGate::On);
    evidence.accept_probe(evidence_completion_probe());
    evidence.note_edit();
    evidence.note_probe_red();
    let mut eval_round = 0;

    for turn in 1..=MAX_COMPLETION_DENIALS {
        let outcome = try_finalize(
            &mut goal,
            &mut evidence,
            crate::guardrails::ContractPolicy::TrustAll,
            dir.path(),
            &crate::judge::NoopJudge,
            &mut recorder,
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &mut eval_round,
            turn,
            "engine_finalize",
        )
        .await
        .unwrap();
        assert_eq!(outcome, FinalizeOutcome::NotComplete);
    }

    assert!(evidence.bypassed);
    let completed = try_finalize(
        &mut goal,
        &mut evidence,
        crate::guardrails::ContractPolicy::TrustAll,
        dir.path(),
        &crate::judge::NoopJudge,
        &mut recorder,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &mut eval_round,
        MAX_COMPLETION_DENIALS + 1,
        "engine_finalize",
    )
    .await
    .unwrap();
    assert_eq!(completed, FinalizeOutcome::Completed);

    let events = std::fs::read_to_string(events_path).unwrap();
    assert_eq!(events.matches("completion.rejected").count(), 3);
    assert!(events.contains("\"type\":\"evidence.gate.bypassed\""));
    assert!(events.contains("\"reason\":\"completion_no_progress\""));
    assert!(events.contains("\"type\":\"run.completed\""));
}

#[derive(Clone, Copy)]
enum EvidenceMainLoopLivenessPath {
    ModelFinalText,
    EngineFinalize,
}

#[derive(Clone)]
struct EvidenceMainLoopLivenessProvider {
    calls: Arc<AtomicUsize>,
    path: EvidenceMainLoopLivenessPath,
}

#[async_trait::async_trait]
impl ProviderClient for EvidenceMainLoopLivenessProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(evidence_completion_response(vec![
                evidence_completion_register_call(
                    "register-main-loop-liveness-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                evidence_completion_edit_call(
                    "edit-with-probe-still-red",
                    "other.txt",
                    "old",
                    "changed",
                ),
            ]));
        }

        let should_finish =
            matches!(self.path, EvidenceMainLoopLivenessPath::ModelFinalText) && call >= 6;
        if should_finish {
            return Ok(evidence_completion_response(Vec::new()));
        }
        let command = match self.path {
            EvidenceMainLoopLivenessPath::ModelFinalText => {
                format!("printf 'liveness warmup {call}\\n'")
            }
            EvidenceMainLoopLivenessPath::EngineFinalize => "printf 'liveness warmup\\n'".into(),
        };
        Ok(evidence_completion_response(vec![ToolCall {
            id: format!("liveness-warmup-{call}"),
            call_type: "function".into(),
            function: FunctionCall {
                name: "shell_exec".into(),
                arguments: json!({ "command": command }).to_string(),
            },
        }]))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("evidence-main-loop-liveness")
    }
}

#[tokio::test]
async fn evidence_liveness_gate_releases_after_repeated_denials_through_main_loop() {
    for (path, run_id, max_turns, expected_calls, expected_via) in [
        (
            EvidenceMainLoopLivenessPath::ModelFinalText,
            "evidence-main-loop-liveness-a",
            10,
            10,
            "model_final_text",
        ),
        (
            EvidenceMainLoopLivenessPath::EngineFinalize,
            "evidence-main-loop-liveness-b",
            13,
            13,
            "engine_finalize",
        ),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let journal = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
        std::fs::write(workspace.path().join("other.txt"), "old\n").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut options = task_test_run_options(
            workspace.path(),
            journal.path(),
            run_id,
            crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
        );
        options.evidence_gate = EvidenceGate::On;
        options.max_turns = max_turns;
        options.max_eval_attempts = 3;

        let result = run_solo(
            EvidenceMainLoopLivenessProvider {
                calls: calls.clone(),
                path,
            },
            options,
        )
        .await
        .unwrap();

        assert_eq!(result.outcome, RunOutcome::Completed, "via={expected_via}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            expected_calls,
            "via={expected_via}"
        );
        let events =
            std::fs::read_to_string(RunPaths::new(journal.path(), run_id).events_path).unwrap();
        assert_eq!(
            events
                .lines()
                .filter(|line| {
                    line.contains("\"type\":\"completion.rejected\"")
                        && line.contains(&format!("\"via\":\"{expected_via}\""))
                })
                .count(),
            MAX_COMPLETION_DENIALS,
            "via={expected_via}\n{events}"
        );
        assert!(events.contains("\"reason\":\"completion_no_progress\""));
        assert!(events.contains("\"type\":\"evidence.gate.bypassed\""));
        assert!(events.contains("\"type\":\"run.completed\""));
        assert!(!events.contains("\"type\":\"run.needs_decision\""));
    }
}

#[tokio::test]
async fn evidence_completion_engine_finalize_accepts_current_green_and_bypassed() {
    let mut green = EvidenceState::new(EvidenceGate::On);
    green.accept_probe(evidence_completion_probe());
    green.note_edit();
    green.note_probe_green();

    let mut bypassed = EvidenceState::new(EvidenceGate::On);
    bypassed.bypassed = true;

    for (index, evidence) in [green, bypassed].iter_mut().enumerate() {
        let (outcome, events) =
            evidence_completion_try_finalize(evidence, &format!("evidence-ready-{index}")).await;
        assert_eq!(outcome, FinalizeOutcome::Completed);
        assert!(events.iter().any(|event| event["type"] == "run.completed"));
    }
}

#[derive(Clone)]
enum EvidenceCompletionPathAScenario {
    NoProbe,
    NoEdit,
    StillRed,
    Green,
    StaleGreen { marker: PathBuf },
    Bypassed,
}

#[derive(Clone)]
struct EvidenceCompletionPathAProvider {
    calls: Arc<AtomicUsize>,
    saw_feedback: Arc<AtomicUsize>,
    scenario: EvidenceCompletionPathAScenario,
}

fn evidence_completion_register_call(id: &str, script: String) -> ToolCall {
    ToolCall {
        id: id.into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "register_issue_probe".into(),
            arguments: json!({
                "script": script,
                "command": "sh {probe}",
                "red_marker": "BUG_PRESENT",
                "marker_stream": "stdout",
                "rationale": "completion gate reproduction"
            })
            .to_string(),
        },
    }
}

fn evidence_completion_edit_call(id: &str, path: &str, old: &str, new: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "fs_edit".into(),
            arguments: json!({
                "path": path,
                "old_string": old,
                "new_string": new,
            })
            .to_string(),
        },
    }
}

fn evidence_completion_read_call(id: &str, path: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "fs_read".into(),
            arguments: json!({ "path": path }).to_string(),
        },
    }
}

fn evidence_completion_response(tool_calls: Vec<ToolCall>) -> ProviderResponse {
    if tool_calls.is_empty() {
        ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls,
            finish_reason: Some(crate::provider::FinishReason::Stop),
        }
    } else {
        ProviderResponse {
            text: "working".into(),
            reasoning: String::new(),
            tool_calls,
            finish_reason: Some(crate::provider::FinishReason::ToolCalls),
        }
    }
}

#[async_trait::async_trait]
impl ProviderClient for EvidenceCompletionPathAProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let expected_feedback = match self.scenario {
            EvidenceCompletionPathAScenario::NoProbe => Some(
                "You cannot finish: no confirmed-red reproduction was ever registered. Call register_issue_probe with a reproduction that fails on the current code.",
            ),
            EvidenceCompletionPathAScenario::NoEdit => {
                Some("You cannot finish: you have not changed any source code. Implement the required source-code fix before trying to finish.")
            }
            EvidenceCompletionPathAScenario::StillRed => Some(
                "You cannot finish: your frozen reproduction still fails. The bug is not fixed.",
            ),
            EvidenceCompletionPathAScenario::StaleGreen { .. } => Some(
                "You cannot finish: source changes after the last passing run invalidated that result, and the latest automatic re-run did not confirm a pass. Correct the implementation so the frozen reproduction passes.",
            ),
            EvidenceCompletionPathAScenario::Green
            | EvidenceCompletionPathAScenario::Bypassed => None,
        };
        if expected_feedback.is_some_and(|expected| {
            messages.iter().any(|message| {
                message.role == "user"
                    && message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains(expected))
            })
        }) {
            self.saw_feedback.store(1, Ordering::SeqCst);
        }

        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let ordinary_probe = || {
            evidence_completion_register_call(
                "register-completion-probe",
                "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; else printf 'fixed\\n'; fi"
                    .into(),
            )
        };
        let tool_calls = match (&self.scenario, call) {
            (EvidenceCompletionPathAScenario::NoProbe, _) => Vec::new(),
            (EvidenceCompletionPathAScenario::NoEdit, 0) => vec![ordinary_probe()],
            (EvidenceCompletionPathAScenario::StillRed, 0) => vec![ordinary_probe()],
            (EvidenceCompletionPathAScenario::StillRed, 1) => vec![
                evidence_completion_read_call("read-unrelated", "other.txt"),
                evidence_completion_edit_call("edit-unrelated", "other.txt", "old", "new"),
            ],
            (EvidenceCompletionPathAScenario::Green, 0) => vec![
                ordinary_probe(),
                evidence_completion_read_call("read-target", "target.txt"),
                evidence_completion_edit_call("fix-target", "target.txt", "buggy", "fixed"),
            ],
            (EvidenceCompletionPathAScenario::StaleGreen { marker }, 0) => vec![
                evidence_completion_register_call(
                    "register-stale-probe",
                    format!(
                        "if grep -q buggy target.txt; then printf 'BUG_PRESENT\\n'; elif [ ! -e '{}' ]; then touch '{}'; printf 'fixed\\n'; else printf 'ModuleNotFoundError: stale rerun\\n' >&2; exit 1; fi",
                        marker.display(),
                        marker.display(),
                    ),
                ),
                evidence_completion_read_call("read-before-stale", "target.txt"),
                evidence_completion_edit_call("fix-before-stale", "target.txt", "buggy", "fixed"),
            ],
            (EvidenceCompletionPathAScenario::StaleGreen { .. }, 1) => vec![
                evidence_completion_read_call("read-after-green", "other.txt"),
                evidence_completion_edit_call("edit-after-green", "other.txt", "old", "new"),
            ],
            (EvidenceCompletionPathAScenario::Bypassed, 0) => (0..MAX_FAILED_REGISTRATIONS)
                .map(|index| {
                    evidence_completion_register_call(
                        &format!("reject-probe-{index}"),
                        "printf 'already green\\n'".into(),
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(evidence_completion_response(tool_calls))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("evidence-completion-path-a")
    }
}

async fn evidence_completion_run_path_a(
    scenario: EvidenceCompletionPathAScenario,
    run_id: &str,
) -> (RunOutcome, Vec<Value>, usize) {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    std::fs::write(workspace.path().join("other.txt"), "old\n").unwrap();
    let saw_feedback = Arc::new(AtomicUsize::new(0));
    let mut opts = task_test_run_options(workspace.path(), journal.path(), run_id, Vec::new());
    opts.evidence_gate = EvidenceGate::On;
    opts.max_turns = match &scenario {
        EvidenceCompletionPathAScenario::StillRed
        | EvidenceCompletionPathAScenario::StaleGreen { .. } => 4,
        _ => 3,
    };
    let result = run_solo(
        EvidenceCompletionPathAProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_feedback: saw_feedback.clone(),
            scenario,
        },
        opts,
    )
    .await
    .unwrap();
    let events = std::fs::read_to_string(RunPaths::new(journal.path(), run_id).events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (result.outcome, events, saw_feedback.load(Ordering::SeqCst))
}

#[tokio::test]
async fn evidence_completion_model_final_text_denies_unready_states_and_feeds_back() {
    let stale_dir = tempfile::tempdir().unwrap();
    let cases = [
        (
            EvidenceCompletionPathAScenario::NoProbe,
            "evidence_no_probe_registered",
            0,
            Value::Null,
        ),
        (
            EvidenceCompletionPathAScenario::NoEdit,
            "evidence_no_edit_yet",
            0,
            Value::Null,
        ),
        (
            EvidenceCompletionPathAScenario::StillRed,
            "evidence_probe_still_red",
            1,
            Value::Null,
        ),
        (
            EvidenceCompletionPathAScenario::StaleGreen {
                marker: stale_dir.path().join("green-once"),
            },
            "evidence_stale_green",
            2,
            json!(1),
        ),
    ];

    for (index, (scenario, reason, edit_epoch, green_epoch)) in cases.into_iter().enumerate() {
        let (outcome, events, saw_feedback) =
            evidence_completion_run_path_a(scenario, &format!("evidence-path-a-{index}")).await;
        assert_ne!(outcome, RunOutcome::Completed);
        assert_eq!(saw_feedback, 1, "reason={reason}");
        let rejected = events
            .iter()
            .find(|event| {
                event["type"] == "completion.rejected" && event["payload"]["reason"] == reason
            })
            .expect("Path A evidence rejection should be observable");
        assert_eq!(rejected["payload"]["via"], "model_final_text");
        assert_eq!(rejected["payload"]["edit_epoch"], edit_epoch);
        assert_eq!(rejected["payload"]["green_epoch"], green_epoch);
        assert!(!events.iter().any(|event| event["type"] == "run.completed"));
    }
}

#[tokio::test]
async fn evidence_completion_model_final_text_accepts_current_green_and_bypassed() {
    for (index, scenario) in [
        EvidenceCompletionPathAScenario::Green,
        EvidenceCompletionPathAScenario::Bypassed,
    ]
    .into_iter()
    .enumerate()
    {
        let (outcome, events, _) =
            evidence_completion_run_path_a(scenario, &format!("evidence-path-a-ready-{index}"))
                .await;
        assert_eq!(outcome, RunOutcome::Completed);
        assert!(events.iter().any(|event| event["type"] == "run.completed"));
        assert!(!events
            .iter()
            .any(|event| event["type"] == "completion.rejected"));
    }
}

#[tokio::test]
async fn provider_turn_finished_is_emitted_for_every_turn_including_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls, _) = finish_reason_provider(crate::provider::FinishReason::Length, false);
    let result = run_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        options(dir.path().to_path_buf(), "observable provider turns"),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let turns: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "provider.turn.finished")
        .collect();
    assert_eq!(turns.len(), 2);
    assert_eq!(
        turns[0]["payload"],
        json!({
            "turn": 1,
            "finish_reason": "length",
            "text_len": 4,
            "reasoning_len": 0,
            "tool_calls": 0,
        })
    );
    assert_eq!(
        turns[1]["payload"],
        json!({
            "turn": 2,
            "finish_reason": "stop",
            "text_len": 10,
            "reasoning_len": 0,
            "tool_calls": 0,
        })
    );
}

#[derive(Clone)]
struct FinishReasonSequenceProvider {
    calls: Arc<AtomicUsize>,
    finish_reasons: Arc<Vec<crate::provider::FinishReason>>,
}

#[async_trait::async_trait]
impl ProviderClient for FinishReasonSequenceProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: format!("response {call}"),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: Some(
                self.finish_reasons
                    .get(call)
                    .cloned()
                    .unwrap_or(crate::provider::FinishReason::Stop),
            ),
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("finish-reason-sequence-test")
    }
}

fn finish_reason_sequence_provider(
    finish_reasons: Vec<crate::provider::FinishReason>,
) -> (FinishReasonSequenceProvider, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        FinishReasonSequenceProvider {
            calls: calls.clone(),
            finish_reasons: Arc::new(finish_reasons),
        },
        calls,
    )
}

#[tokio::test]
async fn three_consecutive_length_responses_stop_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls) = finish_reason_sequence_provider(vec![
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Length,
    ]);
    let mut opts = options(dir.path().to_path_buf(), "repeated truncation");
    opts.max_turns = 40;
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(events.contains("\"reason\":\"consecutive_output_truncation\""));
    assert!(events.contains("\"consecutive_truncated_turns\":3"));
}

#[tokio::test]
async fn normal_response_resets_consecutive_length_counter() {
    let dir = tempfile::tempdir().unwrap();
    let (provider, calls) = finish_reason_sequence_provider(vec![
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Stop,
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Length,
        crate::provider::FinishReason::Stop,
    ]);
    let mut opts = options(dir.path().to_path_buf(), "non-consecutive truncation");
    opts.max_turns = 5;
    opts.max_eval_attempts = 99;
    opts.criteria = crate::goal::parse_criteria(&["cmd: false".into()]).unwrap();
    let result = run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), opts)
        .await
        .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    // K3：criteria 恒假 → 预算（5 轮）耗尽前 run_loop 多给一轮收尾发言，provider 因此多被调用 1 次。
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let events =
        std::fs::read_to_string(RunPaths::new(dir.path(), "run_test").events_path).unwrap();
    assert!(!events.contains("consecutive_output_truncation"));
}

#[tokio::test]
async fn repeated_same_shell_result_trips_no_progress_before_budget() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_repeated_shell_no_progress";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();

    let mut opts = options(dir.path().to_path_buf(), "repeat shell");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 16;
    opts.max_eval_attempts = 99;
    opts.run_id = Some(run_id.to_string());

    let calls = Arc::new(AtomicUsize::new(0));
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        RepeatShellProvider {
            calls: calls.clone(),
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    assert!(calls.load(Ordering::SeqCst) < 16);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"blocked_reason\":\"no_progress\""));
    assert!(!events.contains("max_turns_exceeded"));
}

#[tokio::test]
async fn final_text_failed_eval_counts_no_progress_and_trips_hard_stop() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_final_text_no_progress";
    let mut opts = options(dir.path().to_path_buf(), "plain failing final text");
    opts.max_turns = 16;
    opts.max_eval_attempts = 99;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = crate::goal::parse_criteria(&["cmd: false".to_string()]).unwrap();

    let result = run_solo_with_judge(
        crate::provider::mock::MockProvider::default(),
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    let paths = RunPaths::new(dir.path(), run_id);
    let events = std::fs::read_to_string(paths.events_path).unwrap();
    assert!(events.contains("\"blocked_reason\":\"no_progress\""));
    assert!(!events.contains("max_turns_exceeded"));
}

#[tokio::test]
async fn budget_exhausted_after_real_edits_reports_still_progressing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.txt"), "v0").unwrap();
    let run_id = "run_budget_exhausted_still_progressing";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();

    let mut opts = options(dir.path().to_path_buf(), "keep editing");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 5;
    opts.max_eval_attempts = 99;
    opts.run_id = Some(run_id.to_string());

    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        EditingProvider {
            calls: calls.clone(),
            offered_tools,
            edits_before_final: 99,
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    // K3：预算耗尽前多给一轮收尾发言——provider 因此比 max_turns(5) 多被调用 1 次。
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    assert_eq!(
        needs_decision["payload"]["blocked_reason"],
        "budget_exhausted_still_progressing"
    );
    assert_eq!(needs_decision["payload"]["attempts_summary"]["turns"], 5);
    assert!(!events.iter().any(|event| event["type"] == "run.failed"));
}

#[tokio::test]
async fn budget_exhausted_read_only_run_reports_no_progress_before_absolute_hard() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..5 {
        std::fs::write(
            dir.path().join(format!("read_{index}.txt")),
            format!("content {index}"),
        )
        .unwrap();
    }
    let run_id = "run_no_progress_hard_boundary";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "pure reader boundary");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 5;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        PureReaderProvider {
            calls,
            offered_tools,
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| {
            event["type"] == "run.needs_decision"
                && event["payload"]["reason"] == "blocked_questions"
        })
        .expect("run should emit no_progress run.needs_decision");
    assert_eq!(needs_decision["payload"]["blocked_reason"], "no_progress");
    assert_eq!(needs_decision["payload"]["contract_version"], 1);
    assert_eq!(needs_decision["payload"]["questions"], json!([]));
    assert_eq!(needs_decision["payload"]["evidence_refs"], json!([]));
    assert_eq!(needs_decision["payload"]["agent_diagnosis"], Value::Null);
    assert_eq!(needs_decision["payload"]["trigger"], "harness");
    assert_eq!(needs_decision["payload"]["attempts_summary"]["turns"], 5);
    assert_eq!(needs_decision["payload"]["consecutive_stale_turns"], 0);
    assert_eq!(needs_decision["payload"]["turns_since_last_real_edit"], 5);
    assert!(
        needs_decision["payload"]["consecutive_read_only_turns"].is_null(),
        "旧计数器字段不该再出现在事件 payload"
    );
    assert!(
        !events.iter().any(|event| {
            event["type"] == "run.failed" && event["payload"]["error"] == "max_turns_exceeded"
        }),
        "hard no-progress boundary must not fall through to max_turns_exceeded"
    );
}

/// K3 探针：不预先按阈值算出「收尾轮是第几次调用」（脆），改用 K3 自身的行为特征识别
/// 收尾轮——它必以空工具集调用 provider（逼模型只能出文本）。命中特征时：① 仍尝试发一个
/// 工具调用（验证会被无视，不执行）；② 给出可辨识的收尾文案（验证落进最终对话）。
/// 特征之外的每一轮都照常重复读同一个文件，制造真停滞把 stale halt 顶上去。
struct StaleHaltWrapupProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for StaleHaltWrapupProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if tools.is_empty() {
            return Ok(ProviderResponse {
                text: "Wrap-up: kept re-reading the same file with no new information, made no edits. Recommend narrowing scope next time.".to_string(),
                reasoning: String::new(),
                // K3 承诺"收尾轮的工具调用不会被执行"——这里故意仍然尝试一次，交给测试断言校验。
                tool_calls: vec![test_tool_call("call_ignored", "fs_read", json!({ "path": "const.txt" }))],
                finish_reason: None,
            });
        }
        Ok(ProviderResponse {
            text: "Reading the same file again.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_{call}"),
                "fs_read",
                json!({ "path": "const.txt" }),
            )],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("stale-halt-wrapup-probe")
    }
}

#[tokio::test]
async fn halt_wrapup_turn_offered_once_lands_final_text_and_ignores_tool_calls() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("const.txt"), "constant").unwrap();
    let run_id = "run_halt_wrapup";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "halt wrapup probe");
    opts.permission = PermissionPolicy::Allow;
    // 留足预算：确保是 stale halt（8 轮真停滞）先触发，不是预算耗尽。
    opts.max_turns = 30;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop(
        StaleHaltWrapupProvider {
            calls: calls.clone(),
        },
        opts.clone(),
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    // 恰好一轮收尾：一旦命中过一次空工具集调用，run 立刻终止——不会再有第二次。
    assert!(
        calls.load(Ordering::SeqCst) < opts.max_turns,
        "halt 必须早于 max_turns 触发，否则这条测试没测到 halt 路径本身"
    );

    // final_text 落地：收尾轮的文本作为最后一条 assistant 消息出现在保存的对话里。
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    let last_assistant = saved
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .expect("K3 收尾轮应留下一条 assistant 消息");
    assert!(
        last_assistant
            .content
            .as_deref()
            .unwrap_or_default()
            .contains("Wrap-up"),
        "最后一条 assistant 消息应是收尾文案：{last_assistant:?}"
    );
    // R2：收尾文本真落地时，nudge 必须和 assistant 回复成对相邻落进 canonical messages
    // （nudge 紧挨在收尾回复前一条，不是孤零零飘在中间某处）。
    assert_eq!(
        saved.messages.last().map(|m| m.role.as_str()),
        Some("assistant"),
        "对话最后一条必须是收尾回复本身"
    );
    let second_to_last = &saved.messages[saved.messages.len() - 2];
    assert_eq!(second_to_last.role, "user");
    assert_eq!(
        second_to_last.content.as_deref(),
        Some(crate::orchestrator::HALT_WRAPUP_NUDGE),
        "收尾回复前一条必须正是 nudge 本身"
    );
    // 收尾轮就算尝试发工具调用也不会被执行——不会出现对应它的 tool 结果消息。
    assert!(
        !saved.messages.iter().any(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("call_ignored")
        }),
        "K3 收尾轮的工具调用必须被忽略、不执行"
    );

    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"blocked_reason\":\"no_progress\""));
    assert!(events.contains("\"step_id\":\"solo.wrapup\""));
    assert!(events.contains("\"outcome\":\"wrapup_given\""));
}

/// K3 反面探针：收尾轮模型完全不配合（只回空白），run 也必须照常无条件终止——不重试、
/// 不卡死、不多给第二轮。
struct UncooperativeWrapupProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for UncooperativeWrapupProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if tools.is_empty() {
            // 收尾轮：只回空白，什么有用的都不说。
            return Ok(ProviderResponse {
                text: "   ".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            });
        }
        Ok(ProviderResponse {
            text: format!("attempt {call}"),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("uncooperative-wrapup-probe")
    }
}

#[tokio::test]
async fn budget_exhausted_wrapup_terminates_even_when_model_gives_empty_response() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "uncooperative wrapup");
    opts.max_turns = 3;
    opts.max_eval_attempts = 99;
    // 永远不满足的标准，逼这条 run 只能靠预算耗尽收场（而非某轮碰巧判完成）。
    opts.criteria = crate::goal::parse_criteria(&["cmd: false".into()]).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let result = run_solo_with_judge(
        UncooperativeWrapupProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    // 3 个正常轮 + 恰好 1 个收尾轮：模型不配合也不会换来第二次机会。
    assert_eq!(calls.load(Ordering::SeqCst), 4);

    let paths = RunPaths::new(dir.path(), &result.run_id);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events
        .iter()
        .any(|event| event["type"] == "orchestration.step.started"
            && event["payload"]["step_id"] == "solo.wrapup"));
    // 模型只回了空白 → 收尾轮不该往对话里塞一条空文本 assistant 消息。
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    assert!(
        !saved
            .messages
            .last()
            .is_some_and(|message| message.role == "assistant"
                && message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()),
        "空白收尾文本不该落进对话：{:?}",
        saved.messages.last()
    );
    // R2：模型不配合（空白回复）时 nudge 本身也绝不该单独落地——不能只有 nudge 没有配对的
    // 回复孤零零飘在对话里的任何位置（不止是最后一条），否则 resume 续跑会被这条悬空的
    // 「不要再调用任何工具」带偏。
    assert!(
        !saved
            .messages
            .iter()
            .any(|message| message.content.as_deref()
                == Some(crate::orchestrator::HALT_WRAPUP_NUDGE)),
        "收尾不配合时不该留下任何一条悬空 nudge：{:?}",
        saved.messages
    );
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    // 从没编辑过（criteria 恒假、provider 从不发工具调用）→ blocked_reason 该是 no_progress，
    // 不是 still_progressing（K3 的收尾轮本身也不算编辑，不该把这个判断带偏）。
    assert_eq!(needs_decision["payload"]["blocked_reason"], "no_progress");
}

struct TextToolCallWrapupProvider;

#[async_trait::async_trait]
impl ProviderClient for TextToolCallWrapupProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let text = if tools.is_empty() {
            "<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name=\"fs_write\">ignored"
        } else {
            "Still working."
        };
        Ok(ProviderResponse {
            text: text.to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("text-tool-call-wrapup-probe")
    }
}

#[tokio::test]
async fn budget_exhausted_wrapup_hides_text_tool_call_and_records_detection() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "text tool call wrapup");
    opts.max_turns = 1;
    opts.max_eval_attempts = 99;
    opts.criteria = crate::goal::parse_criteria(&["cmd: false".into()]).unwrap();

    let result = run_solo_with_judge(
        TextToolCallWrapupProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    let paths = RunPaths::new(dir.path(), &result.run_id);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    let wrapup = saved
        .messages
        .last()
        .and_then(|message| message.content.as_deref())
        .expect("收尾轮应落下一条可读说明");
    assert!(!wrapup.contains("DSML"));
    assert_eq!(wrapup, crate::orchestrator::TEXT_TOOL_CALL_HIDDEN_NOTICE);

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let completed = events
        .iter()
        .find(|event| {
            event["type"] == "orchestration.step.completed"
                && event["payload"]["step_id"] == "solo.wrapup"
        })
        .expect("收尾轮应发 completed 事件");
    assert_eq!(completed["payload"]["text_tool_call_detected"], true);
}

#[tokio::test]
async fn normal_editing_run_not_tripped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.txt"), "v0").unwrap();
    let run_id = "run_no_progress_normal_editing";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "normal editing");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), opts.criteria.clone());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        EditingProvider {
            calls,
            offered_tools: offered_tools.clone(),
            edits_before_final: 6,
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("target.txt")).unwrap(),
        "v6"
    );
    let snapshots = offered_tools.lock().unwrap();
    assert!(snapshots
        .iter()
        .all(|names| names.iter().any(|name| name == "fs_read")));
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(!events.contains("\"reason\":\"no_progress\""));
    assert!(!events.contains("\"type\":\"run.blocked\""));
}

#[tokio::test]
async fn shell_exec_workspace_work_counts_as_progress() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_no_progress_shell_work";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "shell edits workspace");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), opts.criteria.clone());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop(
        ShellEditingProvider {
            calls: calls.clone(),
            shell_turns_before_final: 6,
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 7);
    let shell_log = std::fs::read_to_string(dir.path().join("shell.log")).unwrap();
    assert!(shell_log.contains("shell 5"));
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(!events.contains("\"reason\":\"no_progress\""));
    assert!(!events.contains("\"type\":\"run.blocked\""));
}

#[tokio::test]
async fn novel_shell_commands_cross_no_progress_threshold_through_run_solo() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_novel_shell_crosses_no_progress_threshold";
    let calls = Arc::new(AtomicUsize::new(0));
    let mut opts = options(dir.path().to_path_buf(), "distinct shell-only work");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();

    let result = run_solo(
        NovelShellProvider {
            calls: calls.clone(),
            // Ten shell-only turns cross the eight-turn no-progress halt while
            // remaining below the twelve-reset novel-shell quota.
            shell_turns_before_final: 10,
        },
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 11);
    let events = std::fs::read_to_string(RunPaths::new(dir.path(), run_id).events_path).unwrap();
    assert!(!events.contains("\"reason\":\"no_progress\""));
    assert!(!events.contains("\"type\":\"run.needs_decision\""));
}

#[tokio::test]
async fn progress_after_narrowing_restores_exploration_tools_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.txt"), "v0").unwrap();
    let run_id = "run_no_progress_recovery";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "recover after edit");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = passing_criteria();
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), opts.criteria.clone());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop(
        ReadThenEditRecoveryProvider {
            calls: calls.clone(),
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 9);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("target.txt")).unwrap(),
        "v1"
    );
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(!events.contains("\"reason\":\"no_progress\""));
    assert!(!events.contains("\"type\":\"run.blocked\""));
}

#[tokio::test]
async fn tool_outcome_recoverable_fs_edit_feedback_continues_and_preserves_pairing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.txt"), "alpha beta").unwrap();
    let paths = RunPaths::new(dir.path(), "run_tool_outcome_recover");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "edit target");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 4;
    opts.run_id = Some("run_tool_outcome_recover".into());
    let mut recorder = EventRecorder::new(
        "run_tool_outcome_recover",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        ToolOutcomeRecoverProvider,
        opts,
        paths.clone(),
        "run_tool_outcome_recover",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_ne!(outcome, RunOutcome::Failed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("target.txt")).unwrap(),
        "alpha gamma"
    );
    let failed_tool_results: Vec<&ChatMessage> = messages
        .iter()
        .filter(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("bad_edit")
        })
        .collect();
    assert_eq!(failed_tool_results.len(), 1);
    assert!(failed_tool_results[0]
        .content
        .as_deref()
        .unwrap()
        .contains("no match"));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&messages);
}

#[tokio::test]
async fn tool_outcome_execute_runtime_err_remains_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_tool_outcome_runtime_err");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "call bad tool");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some("run_tool_outcome_runtime_err".into());
    let mut recorder = EventRecorder::new(
        "run_tool_outcome_runtime_err",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(RuntimeErrTool));

    let result = run_loop_with_registry(
        registry,
        RuntimeErrProvider,
        opts,
        paths.clone(),
        "run_tool_outcome_runtime_err",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await;

    assert!(
        matches!(result, Err(HarnessError::Runtime(message)) if message.contains("runtime err from tool"))
    );
    assert!(!messages.iter().any(|message| message.role == "tool"));
}

#[tokio::test]
async fn checkpoint_failure_from_fs_write_fails_run_before_follow_up_turn() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_checkpoint_failure_from_fs_write";
    let mut opts = options(dir.path().to_path_buf(), "try the guarded write");
    opts.permission = PermissionPolicy::Allow;
    opts.memory_enabled = false;
    opts.max_turns = 3;
    opts.run_id = Some(run_id.into());
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_tool_feedback = Arc::new(AtomicUsize::new(0));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "http://127.0.0.1:{}/checkpoint",
        listener.local_addr().unwrap().port()
    );
    drop(listener);

    let result = crate::tools::with_checkpoint_env_override_for_test(
        Some(endpoint),
        Some("secret-token".into()),
        async {
            run_solo_with_judge(
                CheckpointFatalWriteThenFollowUpProvider {
                    calls: calls.clone(),
                    saw_tool_feedback: saw_tool_feedback.clone(),
                },
                Box::new(crate::judge::NoopJudge),
                opts,
            )
            .await
        },
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(saw_tool_feedback.load(Ordering::SeqCst), 0);

    let target = dir.path().join("nested/out.txt");
    let parent = target.parent().unwrap();
    assert!(
        !parent.exists(),
        "checkpoint failure must not create the parent directory"
    );
    assert!(
        !target.exists(),
        "checkpoint failure must not create the target file"
    );

    let paths = RunPaths::new(dir.path(), run_id);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "provider.turn.finished")
            .count(),
        1,
        "fatal checkpoint failure must stop the loop before a second provider turn"
    );
    assert!(events.iter().any(|event| {
        event["type"] == "tool.failed"
            && event["payload"]["tool_call_id"] == "call_checkpoint_write"
            && event["payload"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("checkpoint"))
    }));
    assert!(events.iter().any(|event| {
        event["type"] == "run.failed"
            && event["payload"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("checkpoint"))
    }));
    assert!(!events
        .iter()
        .any(|event| { event["payload"]["tool_call_id"] == "call_after_checkpoint_failure" }));
}

#[tokio::test]
async fn conversation_pairing_runtime_fatal_keeps_saved_conversation_provider_legal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_conversation_pairing_runtime_err");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "call bad tool");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some("run_conversation_pairing_runtime_err".into());
    let mut recorder = EventRecorder::new(
        "run_conversation_pairing_runtime_err",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(RuntimeErrTool));

    let result = run_loop_with_registry(
        registry,
        RuntimeErrProvider,
        opts,
        paths.clone(),
        "run_conversation_pairing_runtime_err",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await;

    assert!(result.is_err());
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert!(
        !saved.messages.iter().any(|message| {
            message.role == "assistant"
                && message
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|call| call.id == "runtime_err"))
        }),
        "fatal tool turn must not be persisted without a paired tool result"
    );
}

#[tokio::test]
async fn conversation_pairing_multi_tool_interrupt_saves_completed_and_placeholder_results() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("first.txt"), "first content").unwrap();
    std::fs::write(dir.path().join("second.txt"), "second content").unwrap();
    let paths = RunPaths::new(dir.path(), "run_conversation_pairing_interrupt");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "read files");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some("run_conversation_pairing_interrupt".into());
    let mut recorder = EventRecorder::new(
        "run_conversation_pairing_interrupt",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = StopOnPoll {
        run_id: "run_conversation_pairing_interrupt".into(),
        stop_on: 3,
        polls: 0,
    };
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        TwoReadCallsProvider,
        opts,
        paths.clone(),
        "run_conversation_pairing_interrupt",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::Interrupted);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_read_1")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("first content"))
    }));
    assert!(saved.messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_read_2")
            && message.content.as_deref() == Some("interrupted before execution")
    }));
}

#[tokio::test]
async fn conversation_pairing_scope_change_with_trailing_tool_call_saves_placeholder_result() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("after_scope.txt"), "unused").unwrap();
    let paths = RunPaths::new(dir.path(), "run_conversation_pairing_scope_trailing");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "propose scope change");
    opts.permission = PermissionPolicy::Allow;
    opts.run_id = Some("run_conversation_pairing_scope_trailing".into());
    let mut recorder = EventRecorder::new(
        "run_conversation_pairing_scope_trailing",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = LiveChannel;
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        ScopeChangeWithTrailingToolProvider,
        opts,
        paths.clone(),
        "run_conversation_pairing_scope_trailing",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&saved.messages);

    let scope_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_scope_trailing")
    });
    let scope_content: Value =
        serde_json::from_str(scope_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(scope_content["status"], "needs_decision");

    let trailing_result = saved.messages.iter().find(|message| {
        message.role == "tool" && message.tool_call_id.as_deref() == Some("call_read_after_scope")
    });
    let trailing_content: Value =
        serde_json::from_str(trailing_result.unwrap().content.as_deref().unwrap()).unwrap();
    assert_eq!(trailing_content["status"], "skipped");
    assert_eq!(trailing_content["reason"], "superseded by needs_decision");
}

#[tokio::test]
async fn conversation_pairing_resume_repairs_legacy_unpaired_tail_before_provider() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_conversation_pairing_resume";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let bad_messages = vec![
        ChatMessage::system("system"),
        ChatMessage::user("start"),
        ChatMessage::assistant(
            "will call",
            None,
            vec![test_tool_call(
                "legacy_unpaired",
                "fs_read",
                json!({"path":"missing.txt"}),
            )],
        ),
    ];
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-pairing".to_string(),
            messages: bad_messages,
        },
    )
    .unwrap();
    let contract = GoalState::new("start", passing_criteria()).contract;
    crate::journal::save_contract(&paths.contract_path, &contract).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let result = resume_solo_with_judge(
        PairingAssertResumeProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        dir.path(),
        dir.path().to_path_buf(),
        run_id.to_string(),
        Some("resume now".to_string()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        crate::goal::NetworkPolicy::On,
        2,
        ControlInputKind::Sentinel,
        true,
        true,
        crate::config::SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let saved: SavedConversation<ChatMessage> =
        load_conversation(&paths.conversation_path).unwrap();
    validate_tool_pairing(&saved.messages).unwrap();
    assert!(
        !saved.messages.iter().any(|message| {
            message.role == "assistant"
                && message
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|call| call.id == "legacy_unpaired"))
        }),
        "repaired journal must not keep the legacy unpaired assistant"
    );
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"type\":\"provider.warning\""));
    assert!(events.contains("\"warning\":\"conversation_repaired\""));
    assert!(events.contains("\"dropped_messages\":1"));
}

#[tokio::test]
async fn resume_loads_goal_contract_sidecar_for_reflex_criteria() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_resume_contract_reflex";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-contract-reflex".to_string(),
            messages: initial_messages("persist objective"),
        },
    )
    .unwrap();
    let criteria = crate::goal::parse_criteria(&["cmd: test -f touched.txt".into()]).unwrap();
    let mut contract = GoalState::new("persist objective", criteria).contract;
    contract.version = 7;
    crate::journal::save_contract(&paths.contract_path, &contract).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));

    let result = resume_solo_with_judge(
        ResumeContractReflexProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        dir.path(),
        dir.path().to_path_buf(),
        run_id.to_string(),
        None,
        OutputMode::Silent,
        PermissionPolicy::Allow,
        crate::goal::NetworkPolicy::On,
        2,
        ControlInputKind::Sentinel,
        true,
        true,
        crate::config::SearchChoice::Ddg,
        Default::default(),
        1,
        0,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(events.contains("\"type\":\"validation.checked\""));
    assert!(events.contains("\"tool_call_id\":\"check_reflex_1_c1\""));
    assert!(events.contains("\"criterion_id\":\"c1\""));
    let restored = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(restored.version, 7);
    assert_eq!(restored.criteria.len(), 1);
}

#[tokio::test]
async fn resume_with_realign_bumps_contract_emits_goal_updated_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_resume_realign";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    std::fs::write(dir.path().join("old.txt"), "ok\n").unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-realign".to_string(),
            messages: initial_messages("old objective"),
        },
    )
    .unwrap();
    let criteria = crate::goal::parse_criteria(&["cmd: test -f old.txt".into()]).unwrap();
    let contract = GoalState::new("old objective", criteria).contract;
    crate::journal::save_contract(&paths.contract_path, &contract).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen_messages = Arc::new(Mutex::new(Vec::new()));

    let result = resume_solo_with_judge(
        ResumeRealignProvider {
            calls: calls.clone(),
            seen_messages: seen_messages.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        dir.path(),
        dir.path().to_path_buf(),
        run_id.to_string(),
        None,
        OutputMode::Silent,
        PermissionPolicy::Allow,
        crate::goal::NetworkPolicy::On,
        3,
        ControlInputKind::Sentinel,
        true,
        true,
        crate::config::SearchChoice::Ddg,
        Default::default(),
        1,
        0,
        Some(crate::goal::ReAlignInput {
            objective: Some("new objective".into()),
            add_criteria: crate::goal::parse_criteria(&["cmd: test -f realigned.txt".into()])
                .unwrap(),
            reason: "user clarified after stuck_repeating".into(),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let restored = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(restored.version, 2);
    assert_eq!(restored.objective, "new objective");
    assert_eq!(restored.update_log.len(), 1);
    let ids: Vec<_> = restored
        .criteria
        .iter()
        .map(|criterion| criterion.id.as_str())
        .collect();
    assert_eq!(ids, vec!["c1", "c2"]);

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let updated = events
        .iter()
        .find(|event| event["type"] == "goal.updated" && event["payload"]["trigger"] == "realign")
        .expect("resume realign should emit goal.updated");
    assert_eq!(updated["payload"]["version"], 2);
    assert_eq!(updated["payload"]["latest_update"]["version"], 2);
    assert_eq!(
        updated["payload"]["latest_update"]["reason"],
        "user clarified after stuck_repeating"
    );
    assert_eq!(updated["payload"]["criteria"].as_array().unwrap().len(), 2);
    assert!(events.iter().any(|event| {
        event["type"] == "tool.completed"
            && event["payload"]["criterion_id"] == "c2"
            && event["payload"]["passed"] == true
    }));

    let seen = seen_messages.lock().unwrap().clone();
    let provider_system = seen[0].content.as_deref().unwrap();
    assert!(provider_system.contains("Objective: new objective"));
    assert!(provider_system.contains("[pending] c2 - 验收检查（须 exit 0）: test -f realigned.txt"));
    assert!(!provider_system.contains("cmd: test -f realigned.txt")); // C1：状态帧不再漏 cmd:
}

#[tokio::test]
async fn resume_without_realign_keeps_contract_version_and_emits_no_realign_update() {
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_resume_without_realign";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "test-local".to_string(),
            model: "resume-without-realign".to_string(),
            messages: initial_messages("persist objective"),
        },
    )
    .unwrap();
    let mut contract = GoalState::new("persist objective", passing_criteria()).contract;
    contract.version = 7;
    crate::journal::save_contract(&paths.contract_path, &contract).unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));

    let result = resume_solo_with_judge(
        StateFrameCaptorProvider {
            seen_messages: seen_messages.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        dir.path(),
        dir.path().to_path_buf(),
        run_id.to_string(),
        None,
        OutputMode::Silent,
        PermissionPolicy::Allow,
        crate::goal::NetworkPolicy::On,
        1,
        ControlInputKind::Sentinel,
        true,
        true,
        crate::config::SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    let restored = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(restored.version, 7);
    assert!(restored.update_log.is_empty());

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!events.iter().any(|event| {
        event["type"] == "goal.updated" && event["payload"]["trigger"] == "realign"
    }));
}

#[tokio::test]
async fn run_solo_writes_goal_contract_sidecar_under_journal_root() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let run_id = "run_contract_sidecar_new";
    let mut opts = options(workspace.path().to_path_buf(), "persist objective");
    opts.journal_root = journal.path().to_path_buf();
    opts.run_id = Some(run_id.to_string());
    opts.criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();

    let result = run_solo_with_judge(
        CompleteImmediatelyProvider,
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    let paths = RunPaths::new(journal.path(), run_id);
    assert!(paths.contract_path.starts_with(journal.path()));
    assert!(!paths.contract_path.starts_with(workspace.path()));
    let contract = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(contract.objective, "persist objective");
    assert_eq!(contract.version, 1);
    assert_eq!(contract.criteria.len(), 1);
}

#[tokio::test]
async fn approved_criteria_update_rewrites_goal_contract_sidecar() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let run_id = "run_contract_sidecar_update";
    let calls = Arc::new(AtomicUsize::new(0));
    let mut opts = options(workspace.path().to_path_buf(), "approve criterion");
    opts.journal_root = journal.path().to_path_buf();
    opts.run_id = Some(run_id.to_string());
    opts.max_turns = 2;
    opts.contract_policy = crate::guardrails::ContractPolicy::TrustAll;

    let result = run_solo_with_judge(
        ProposeCriterionThenFinalProvider {
            calls: calls.clone(),
        },
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let paths = RunPaths::new(journal.path(), run_id);
    let contract = crate::journal::load_contract(&paths.contract_path).unwrap();
    assert_eq!(contract.version, 1);
    assert_eq!(contract.criteria.len(), 1);
    assert_eq!(contract.criteria[0].claim, "new criterion");
}

struct OrderCaptor(Arc<Mutex<Vec<(String, bool)>>>);

#[async_trait::async_trait]
impl ProviderClient for OrderCaptor {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        events: &mut crate::events::EventRecorder,
    ) -> crate::error::Result<ProviderResponse> {
        // The second value means "this message itself is the <env> block";
        // system guidance can legitimately mention <env>.
        *self.0.lock().unwrap() = messages
            .iter()
            .map(|m| {
                (
                    m.role.clone(),
                    m.content
                        .as_deref()
                        .unwrap_or("")
                        .trim_start()
                        .starts_with("<env>"),
                )
            })
            .collect();
        events.emit_text_delta("done")?;
        Ok(ProviderResponse {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "cap".into(),
            model_id: "cap".into(),
            supports_streaming: true,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: None,
            output_token_limit: None,
            server_side_search: false,
        }
    }
}

#[tokio::test]
async fn fresh_run_wire_order_is_system_env_task() {
    let ws = tempfile::tempdir().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut opts = options(ws.path().to_path_buf(), "do the task");
    opts.memory_enabled = false;
    opts.prompt = "do the task".into();

    let _ = run_solo_with_judge(
        OrderCaptor(captured.clone()),
        Box::new(crate::judge::NoopJudge),
        opts,
    )
    .await
    .unwrap();

    let seq = captured.lock().unwrap().clone();
    assert_eq!(
        seq[0].0, "system",
        "first message must be system (executor prompt + state-frame)"
    );
    assert!(
        !seq[0].1,
        "system is not itself the <env> terrain block, even when it points to it"
    );
    assert_eq!(seq[1].0, "user");
    assert!(
        seq[1].1,
        "second message must be the user <env> terrain block"
    );
    assert_eq!(seq[2].0, "user", "third message is the task");
    assert!(!seq[2].1, "task is not an <env> block");
}

#[test]
fn executor_prompt_points_to_env_block() {
    assert!(EXECUTOR_SYSTEM_PROMPT.contains("<env>"));
    assert!(EXECUTOR_SYSTEM_PROMPT.to_lowercase().contains("absolute"));
}

async fn run_preflight_reject_case(
    provider: PreflightRejectProvider,
) -> (RunOutcome, Vec<ChatMessage>) {
    let run_id = provider.model_id;
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "preflight reject");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 4;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        provider,
        opts,
        paths,
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    (outcome, messages)
}

#[tokio::test]
async fn preflight_reject_unsupported_tool_does_not_crash() {
    let (outcome, messages) = run_preflight_reject_case(PreflightRejectProvider {
        model_id: "preflight_reject_unsupported_tool",
        tool_call_id: "call_unknown_tool",
        tool_name: "missing_tool",
        arguments: "{}".to_string(),
        expected_feedback: &["unsupported tool"],
    })
    .await;

    assert_ne!(outcome, RunOutcome::Failed);
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_unknown_tool")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("unsupported tool"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&messages);
}

#[tokio::test]
async fn preflight_reject_write_targets_path_escape_does_not_crash() {
    let (outcome, messages) = run_preflight_reject_case(PreflightRejectProvider {
        model_id: "preflight_reject_write_targets_path_escape",
        tool_call_id: "call_escape_write",
        tool_name: "fs_write",
        arguments: json!({ "path": "../escape", "content": "nope" }).to_string(),
        expected_feedback: &["invalid path", "outside"],
    })
    .await;

    assert_ne!(outcome, RunOutcome::Failed);
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_escape_write")
            && message.content.as_deref().is_some_and(|content| {
                content.contains("invalid path") && content.contains("outside")
            })
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&messages);
}

#[tokio::test]
async fn preflight_reject_write_targets_bad_args_does_not_crash() {
    let (outcome, messages) = run_preflight_reject_case(PreflightRejectProvider {
        model_id: "preflight_reject_write_targets_bad_args",
        tool_call_id: "call_bad_args_write",
        tool_name: "fs_write",
        arguments: json!({ "path": "missing-content.txt" }).to_string(),
        expected_feedback: &["invalid path or arguments"],
    })
    .await;

    assert_ne!(outcome, RunOutcome::Failed);
    assert!(messages.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("call_bad_args_write")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("invalid path or arguments"))
    }));
    assert_each_assistant_tool_call_has_exactly_one_tool_result(&messages);
}

// ---------------------------------------------------------------------------
// P1/P2（2026-07-26）：`invalidates_verification` 与 `turn_had_edit` 概念分家的回归测试。
//
// 病灶回顾：MCP 工具（`McpToolProxy::execute` 用 `ToolOutcome::success_mutating`，
// `invalidates_verification: true`）此前被 run_loop.rs 的 `if tool_result
// .invalidates_verification { turn_had_edit = true; }` 硬焊成「本轮真编辑了工作区」，
// 于是每次成功的 MCP 调用都会把 `note_safety_signals` 喂成「刚编辑过」，把
// consecutive_stale_turns / turns_since_last_real_edit 全部清零——全靠
// mcp__agentloom__* 干活、被 `--disallow-tools fs_edit,fs_write,shell_exec` 收走原生
// 写工具的 lead，复读/同参重试环永远撞不上 adaptive_safety_net 的任何一档推力，能烧穿
// 整个预算。下面几条测试分别钉住修复后三个概念（`turn_had_edit` 真编辑 /
// `turn_had_mutating_call` 有副作用调用 / K1 的 novel-call 去重）各自接对了地方。
// ---------------------------------------------------------------------------

/// 永远成功、`invalidates_verification: true` 的假 MCP 工具（`is_mcp() == true`）——
/// 模拟 `McpToolProxy` 成功调用的可观察行为，不需要真起一个 MCP server。
struct FakeMcpMutatingTool {
    name: String,
}

#[async_trait::async_trait]
impl crate::tools::Tool for FakeMcpMutatingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn definition(&self) -> Value {
        json!({ "type": "function", "function": { "name": self.name } })
    }

    fn mutates(&self) -> bool {
        true
    }

    fn is_mcp(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        _ctx: &mut crate::tools::ToolContext<'_>,
        _call: &ToolCall,
    ) -> Result<crate::tools::ToolOutcome> {
        Ok(crate::tools::ToolOutcome::success_mutating(
            "ok".to_string(),
        ))
    }
}

/// 非 MCP、非 mutating 的假只读工具——代表「本轮真的空转」（既非编辑、也非 MCP 副作用调用），
/// 用来在 `mcp_call_still_disarms_completion_gate` 里制造一个货真价实的空转轮。
struct FakePeekTool;

#[async_trait::async_trait]
impl crate::tools::Tool for FakePeekTool {
    fn name(&self) -> &str {
        "peek_tool"
    }

    fn definition(&self) -> Value {
        json!({ "type": "function", "function": { "name": "peek_tool" } })
    }

    fn mutates(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        _ctx: &mut crate::tools::ToolContext<'_>,
        _call: &ToolCall,
    ) -> Result<crate::tools::ToolOutcome> {
        Ok(crate::tools::ToolOutcome::success("peeked".to_string()))
    }
}

/// 每轮都用完全相同的参数调用同一个 MCP 工具（死循环复读的最简复现）。
struct RepeatedMcpCallProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for RepeatedMcpCallProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: "Calling the same MCP tool again.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_mcp_repeat_{call}"),
                "mcp__fake__do_thing",
                json!({ "n": 0 }),
            )],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("repeated-mcp-call")
    }
}

/// 每轮换一套参数调用同一个 MCP 工具（正常的 lead 派单节奏——每次做不同的事）。
struct NovelMcpCallProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for NovelMcpCallProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: "Calling the MCP tool with new arguments.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_mcp_novel_{call}"),
                "mcp__fake__do_thing",
                json!({ "n": call }),
            )],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("novel-mcp-call")
    }
}

#[tokio::test]
async fn repeated_identical_mcp_call_trips_stale_halt() {
    // P1 定罪场景：修复前，MCP 工具「成功调用即 turn_had_edit=true」会把 stale 计数器
    // 每轮清零，同参数死循环永远撞不上 stale halt(8 轮)。修复后，K1 的 `note_mcp_call`
    // 去重才是 MCP 型 run 唯一在管的 stale 计数入口：重复调用不再被误判「新进展」，
    // ≈8 轮就该被 halt 掐住——不是撑到预算耗尽才停。
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_repeated_identical_mcp_call";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "repeat the same mcp call");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 30; // 远大于 halt(8)，证明是被 halt 掐的，不是撑到预算耗尽。
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
    }));
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop_with_registry(
        registry,
        RepeatedMcpCallProvider {
            calls: calls.clone(),
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let total_calls = calls.load(Ordering::SeqCst);
    assert!(
        total_calls < 15,
        "重复同参 MCP 调用应在 stale halt(8) 附近被掐，实际跑了 {total_calls} 轮，\
         说明 P1 修复失效、安全网又被 MCP 调用清零了"
    );
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("stale halt should emit needs_decision");
    assert_eq!(needs_decision["payload"]["blocked_reason"], "no_progress");
}

#[tokio::test]
async fn novel_mcp_calls_never_trip_stale_halt() {
    // K1 的保护性回归：每轮参数不同的正常 MCP 派单节奏不该被误掐——novel-call 去重
    // 每轮都当「新进展」，stale 永远清零，run 应该正常跑满预算（而不是被当成死循环）。
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_novel_mcp_calls";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let max_turns = crate::adaptive_safety_net::Thresholds::DEFAULT.halt + 10;
    let mut opts = options(dir.path().to_path_buf(), "dispatch different mcp calls");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = max_turns;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
    }));
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop_with_registry(
        registry,
        NovelMcpCallProvider {
            calls: calls.clone(),
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        max_turns + 1,
        "参数各不相同的 MCP 派单不该被 stale halt 提前掐掉，run 必须撑到预算耗尽 + 1 轮收尾发言"
    );
}

#[tokio::test]
async fn mcp_call_does_not_emit_safety_net_checkpoint() {
    // 纯 MCP 轮不该触发 `git_archive::checkpoint`（对用户仓库做 `git stash create` 快照）——
    // 那是为「真编辑」准备的安全网，MCP 副作用调用不该假装成编辑去戳它。
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_mcp_call_no_checkpoint";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "call mcp tool a few times");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 3;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
    }));
    let calls = Arc::new(AtomicUsize::new(0));

    let _outcome = run_loop_with_registry(
        registry,
        NovelMcpCallProvider {
            calls: calls.clone(),
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // 断言两个事件都不该出现——`if turn_had_edit { checkpoint 或 checkpoint_skipped 二选一发 }`
    // 整个 if 分支都不该在纯 MCP 轮跑到。只查 "safety_net.checkpoint" 不够：测试用的临时目录
    // 不是 git 仓库，`git_archive::checkpoint` 即便被错误调用也只会返回 None（emit
    // checkpoint_skipped），不会命中真正的 "safety_net.checkpoint"——两个都不出现才能证明
    // 整个 `if turn_had_edit` 块被跳过，而不是走进去又刚好拿到 None。
    assert!(
        !events
            .iter()
            .any(|event| event["type"] == "safety_net.checkpoint"
                || event["type"] == "safety_net.checkpoint_skipped"),
        "纯 MCP 轮不该碰 git checkpoint 的 if turn_had_edit 分支（不管最终是否真产生快照）"
    );
}

/// 前 4 轮反复调用（参数各异的）mutating MCP 工具，第 5 轮换成非 MCP、非 mutating 的
/// 只读工具——制造一个「MCP 忙了几轮之后终于有一轮真空转」的场景。
struct McpThenIdleProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for McpThenIdleProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let tool_calls = if call < 4 {
            vec![test_tool_call(
                &format!("call_mcp_{call}"),
                "mcp__fake__do_thing",
                json!({ "n": call }),
            )]
        } else if call == 4 {
            vec![test_tool_call("call_peek_4", "peek_tool", json!({}))]
        } else {
            Vec::new()
        };
        let text = if tool_calls.is_empty() {
            "All done.".to_string()
        } else {
            "Working via MCP.".to_string()
        };
        Ok(ProviderResponse {
            text,
            reasoning: String::new(),
            tool_calls,
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("mcp-then-idle")
    }
}

#[tokio::test]
async fn mcp_call_still_disarms_completion_gate() {
    // 精确复现 P1 修复前的「陈旧武装」漏洞：
    // 轮 1-2：MCP 调用攒 verify_debt（阈值=3，还没到）；
    // 轮 3：第 3 次 MCP 调用把 debt 攒到阈值，reflex 校验干净通过 → 武装
    //       completion_gate（同轮不生效——「置位轮 ≤ 本轮」的自免疫）；
    // 轮 4：又一次 MCP 调用——本该把轮 3 的武装撤销（它调用了 MCP、旧的绿不能再当数）；
    // 轮 5：真空转（非 MCP、非 mutating 的只读工具）——如果轮 4 没有正确撤销武装，
    //       引擎会拿轮 3 那份已经过期的「验证通过」直接抢跑收尾，而中间轮 4 的 MCP
    //       调用根本没有被重新验证过。
    // 断言：run 不能在轮 5 提前 Completed——必须撑满 5 轮预算，附加一轮 K3 收尾发言。
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_mcp_call_disarms_completion_gate";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let criteria = passing_criteria();
    let mut opts = options(dir.path().to_path_buf(), "keep dispatching via mcp");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 5;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = criteria.clone();
    opts.verify_reflex_debt = 3;
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), criteria);
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
    }));
    registry.register(Box::new(FakePeekTool));
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop_with_registry(
        registry,
        McpThenIdleProvider {
            calls: calls.clone(),
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::NeedsDecision,
        "轮 4 的 MCP 调用应撤销轮 3 的陈旧武装，轮 5 的空转不该被当成「可以收尾」"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        6,
        "应该跑满 5 轮 + 1 轮 K3 收尾发言，不该在轮 5 提前退出"
    );
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        !events.iter().any(|event| event["type"] == "run.completed"),
        "不该出现 run.completed——那就是陈旧武装抢跑收尾的铁证"
    );
}

#[tokio::test]
async fn mcp_call_still_increments_verify_debt() {
    // verify reflex 的欠债计数不该被 P1 的三概念分家误伤——MCP mutating 调用仍然要
    // 累计 verify_debt，攒到阈值时照样触发一次 reflex 校验（`validation.checked` 事件
    // 里的 `debt` 字段直接钉住这一点）。
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_mcp_call_verify_debt";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let criteria = passing_criteria();
    let mut opts = options(dir.path().to_path_buf(), "call mcp tool once");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 1;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = criteria.clone();
    opts.verify_reflex_debt = 1;
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), criteria);
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
    }));
    let calls = Arc::new(AtomicUsize::new(0));

    let _outcome = run_loop_with_registry(
        registry,
        RepeatedMcpCallProvider {
            calls: calls.clone(),
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let validation_checked = events
        .iter()
        .find(|event| event["type"] == "validation.checked")
        .expect("单次 MCP mutating 调用应该把 verify_debt 攒到阈值、触发一次 reflex 校验");
    assert_eq!(
        validation_checked["payload"]["debt"], 1,
        "verify_debt 应该由 MCP 工具的 invalidates_verification 累计，不该被 P1 的三概念分家漏掉"
    );
}

/// 制造 `WorkspaceChange::Unverifiable`：先靠一次真实编辑让 evidence probe 转绿，
/// 再 `chmod 000` 目标文件让 git 没法再算出内容指纹——精确复用既有
/// `evidence_unverifiable_workspace_invalidates_green_not_keeps_it` 的手法。
#[derive(Clone)]
struct WorkspaceUnverifiableSafetyCounterProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for WorkspaceUnverifiableSafetyCounterProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let tool_calls = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => vec![
                evidence_completion_register_call(
                    "register-safety-counter-probe",
                    "if grep -q buggy target.txt; then printf 'BUG_PRESENT\n'; else printf 'fixed\n'; fi"
                        .into(),
                ),
                ToolCall {
                    id: "fix-safety-counter-target".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "shell_exec".into(),
                        arguments: json!({
                            "command": "printf 'fixed\\n' > target.txt"
                        })
                        .to_string(),
                    },
                },
            ],
            1 => vec![ToolCall {
                id: "break-git-fingerprint-safety-counter".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "shell_exec".into(),
                    arguments: json!({ "command": "chmod 000 target.txt" }).to_string(),
                },
            }],
            _ => Vec::new(),
        };
        Ok(evidence_completion_response(tool_calls))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("workspace-unverifiable-safety-counters")
    }
}

#[tokio::test]
async fn workspace_unverifiable_does_not_reset_safety_counters() {
    // 钉死 run_loop.rs 里 `WorkspaceChange::Unverifiable` 分支：验证失败不是编辑的证据，
    // 不该把 `turns_since_last_real_edit` 重置。
    // 轮 1：真实编辑（printf）让 evidence probe 转绿 → turns_since_last_real_edit 清零；
    // 轮 2：chmod 000 让 git 没法再算内容指纹 → Unverifiable——如果这里仍然错误地把
    //       `turn_had_edit` 置真（P1 修复前的行为），轮 2 结束后 turns_since_last_real_edit
    //       会又被清零成 0；修复后应该正确地涨到 1。
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=AgentLoom Test",
            "-c",
            "user.email=agentloom@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ])
        .current_dir(workspace.path())
        .status()
        .unwrap();
    assert!(commit.success());

    let run_id = "workspace-unverifiable-safety-counters";
    let mut options = task_test_run_options(workspace.path(), journal.path(), run_id, Vec::new());
    options.evidence_gate = EvidenceGate::On;
    options.max_turns = 2;

    let result = run_solo(
        WorkspaceUnverifiableSafetyCounterProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Completed);
    let events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(journal.path(), run_id).events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "evidence.workspace.unverifiable"),
        "fixture 前提：chmod 000 必须真的触发 Unverifiable 分支"
    );
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    assert_eq!(
        needs_decision["payload"]["turns_since_last_real_edit"], 1,
        "workspace 不可验证不是编辑的证据，不该把 turns_since_last_real_edit 重置回 0"
    );
}

#[tokio::test]
async fn budget_exhausted_without_write_tools_is_not_no_progress() {
    // P2 定罪场景：全靠 mcp__agentloom__* 派单、结构上没有 fs_write/fs_edit 可用的 lead
    // 从不产生「真编辑」，`turns_since_last_real_edit` 恒等于 `turns`。修完 P1 后，
    // `budget_exhausted_blocked_reason` 若不看 `write_tools_offered`，会把这种正常
    // run 误标 no_progress（app 前端 stopReason.ts 直接把「卡住了」的错误文案甩给用户，
    // 其实只是打满了预算、活照样在干）。
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_budget_exhausted_no_write_tools_mcp";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "dispatch via mcp only, no native write tools",
    );
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 5;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    // registry 里压根没有 fs_write/fs_edit——结构上不可能写文件（模拟被 --disallow-tools
    // 收走原生写工具、只留 MCP 派单通道的 lead）。
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__agentloom__dispatch_worker".to_string(),
    }));
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop_with_registry(
        registry,
        NovelMcpCallProvider {
            calls: calls.clone(),
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    assert_ne!(
        needs_decision["payload"]["blocked_reason"], "no_progress",
        "无写工具的 MCP 型 run 打满预算不该被误标 no_progress"
    );
    assert_eq!(
        needs_decision["payload"]["blocked_reason"],
        "budget_exhausted_still_progressing"
    );
}
