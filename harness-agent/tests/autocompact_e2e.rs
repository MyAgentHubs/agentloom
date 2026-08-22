use std::sync::{Arc, Mutex};

use myagent::context_budget::autocompact::{
    parse_transcript, plan_compaction, rebuild, render_transcript, should_compact, CompactPlan,
    Msg, OldSummary, ParsedTranscript,
};
use myagent::context_budget::{estimate_tokens, BudgetLimits};
use myagent::events::{EventRecorder, OutputMode};
use myagent::journal::{load_conversation, save_conversation, RunPaths, SavedConversation};
use myagent::orchestrator::{resume_solo, run_solo, ControlInputKind, RunOptions, RunOutcome};
use myagent::provider::{
    ChatMessage, FinishReason, ProviderCapabilities, ProviderClient, ProviderResponse,
};
use serde_json::Value;

const NONCE: &str = "0123456789abcdef0123456789abcdef";
const SUMMARY: &str = "## Primary Request and Intent\nfinish the task\n## Key Technical Concepts\ncheckpoint\n## Files and Code\n(none)\n## Errors and Fixes\n(none)\n## Pending Jobs\n(none)\n## Current Work\ncontinue\n## Next Step\nrespond\n## Critical Context\npreserve exact facts";
const TRANSCRIPT_GOLDEN: &str = include_str!("fixtures/transcript-marker-golden.txt");
const LEAD_TRANSCRIPT_GOLDEN: &str = include_str!("fixtures/lead-transcript-marker-golden.txt");

fn unit_limits(budget: usize) -> BudgetLimits {
    BudgetLimits {
        context_tokens: budget,
        output_headroom: 0,
        safety_buffer: 0,
        recent_turns_keep: 3,
        min_recent: 1,
        chars_per_token: 1,
        per_msg_overhead: 0,
    }
}

fn parsed_transcript(messages: &[(i64, &str, &str)]) -> ParsedTranscript {
    ParsedTranscript {
        preamble: "preamble\n".to_string(),
        old_summary: None,
        messages: messages
            .iter()
            .map(|(id, role, text)| Msg {
                id: *id,
                role: (*role).to_string(),
                text: (*text).to_string(),
            })
            .collect(),
        trailing: "trailing\n".to_string(),
        nonce: NONCE.to_string(),
    }
}

#[derive(Clone, Copy)]
enum SummaryBehavior {
    Success,
    MissingSections,
    Error,
}

struct AutoCompactProvider {
    behavior: SummaryBehavior,
    context_tokens: u32,
    seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    summary_calls: Arc<Mutex<usize>>,
}

fn capabilities(context_tokens: u32) -> ProviderCapabilities {
    ProviderCapabilities {
        provider_id: "autocompact-e2e".into(),
        model_id: "autocompact-e2e".into(),
        supports_streaming: false,
        supports_reasoning_deltas: false,
        supports_tool_calling: true,
        supports_images: false,
        supports_computer_use: false,
        supports_shell_tool: true,
        max_context_tokens: Some(context_tokens),
        output_token_limit: Some(500),
        server_side_search: false,
    }
}

#[async_trait::async_trait]
impl ProviderClient for AutoCompactProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        self.seen.lock().unwrap().push(messages.to_vec());
        let is_summary = messages.first().is_some_and(|message| {
            message.content.as_deref().is_some_and(|content| {
                content.contains("context checkpoint compaction for a coding-agent session")
            })
        });
        if is_summary {
            *self.summary_calls.lock().unwrap() += 1;
            if matches!(self.behavior, SummaryBehavior::Error) {
                return Err(myagent::error::HarnessError::Provider(
                    "summary unavailable".into(),
                ));
            }
            let summary = match self.behavior {
                SummaryBehavior::Success => SUMMARY,
                SummaryBehavior::MissingSections => "incomplete summary",
                SummaryBehavior::Error => unreachable!("error returned above"),
            };
            return Ok(response(summary));
        }
        Ok(response("done"))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities(self.context_tokens)
    }
}

struct NoCompactProvider {
    seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

#[async_trait::async_trait]
impl ProviderClient for NoCompactProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        self.seen.lock().unwrap().push(messages.to_vec());
        Ok(response("done"))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities(100_000)
    }
}

fn response(text: &str) -> ProviderResponse {
    ProviderResponse {
        text: text.into(),
        reasoning: String::new(),
        tool_calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
        interruption: None,
    }
}

fn opts(ws: &std::path::Path, prompt: &str) -> RunOptions {
    RunOptions {
        prompt: prompt.into(),
        workspace: ws.to_path_buf(),
        provider_id: "autocompact-e2e".into(),
        model: "autocompact-e2e".into(),
        client_session_id: None,
        output_mode: myagent::events::OutputMode::Silent,
        control_input: ControlInputKind::Sentinel,
        permission: myagent::shell::PermissionPolicy::Allow,
        network: myagent::goal::NetworkPolicy::On,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
        fs_write_fence: myagent::exec::sandbox::FsWriteFence::Off,
        evidence_gate: myagent::orchestrator::EvidenceGate::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: false,
        search: myagent::config::SearchChoice::Ddg,
        max_turns: 2,
        run_id: None,
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 1,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        journal_root: ws.to_path_buf(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

fn marked_prompt(chunk_len: usize) -> String {
    let old_one = format!("OLD_FOLDED_ONE_{}", "a".repeat(chunk_len));
    let old_two = format!("OLD_FOLDED_TWO_{}", "b".repeat(chunk_len));
    let kept = format!("KEPT_TAIL_{}", "c".repeat(chunk_len));
    format!(
        "task preamble\n===== AGENTLOOM-MSG {NONCE} id=1 role=user =====\n{old_one}\n===== AGENTLOOM-MSG {NONCE} id=2 role=assistant =====\n{old_two}\n===== AGENTLOOM-MSG {NONCE} id=3 role=user =====\n{kept}\n===== AGENTLOOM-HISTORY-END {NONCE} =====\ncontinue naturally\n"
    )
}

fn long_transcript(message_count: usize, chunk_len: usize, marked: bool) -> String {
    let mut prompt = String::from("task preamble\n");
    for id in 1..=message_count {
        let role = if id % 2 == 0 { "assistant" } else { "user" };
        if marked {
            prompt.push_str(&format!(
                "===== AGENTLOOM-MSG {NONCE} id={id} role={role} =====\n"
            ));
        } else {
            prompt.push_str(&format!("message {id} role={role}\n"));
        }
        prompt.push_str(&format!("LONG_MESSAGE_{id:02}_{}\n", "x".repeat(chunk_len)));
    }
    if marked {
        prompt.push_str(&format!("===== AGENTLOOM-HISTORY-END {NONCE} =====\n"));
    }
    prompt.push_str("continue naturally\n");
    prompt
}

fn run_files(ws: &std::path::Path, run_id: &str) -> (Vec<Value>, SavedConversation<ChatMessage>) {
    let run_dir = ws.join(".myagenthubs/runs").join(run_id);
    let events = std::fs::read_to_string(run_dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let conversation = load_conversation(&run_dir.join("conversation.json")).unwrap();
    (events, conversation)
}

fn compact_events(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|event| event["type"] == "orchestration.step.completed")
        .filter(|event| event["payload"]["step_id"] == "solo.compact")
        .collect()
}

fn assert_complete_journal_sequence(events: &[Value], compact_event: &Value) {
    assert_eq!(events.first().unwrap()["type"], "run.started");
    assert_eq!(events.last().unwrap()["type"], "run.completed");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event["seq"],
            (index + 1) as u64,
            "journal seq must be gapless"
        );
    }
    let compact_index = events
        .iter()
        .position(|event| std::ptr::eq(event, compact_event))
        .expect("compact event should come from the journal");
    assert!(compact_index > 0 && compact_index + 1 < events.len());
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn assert_payload_shape_matches_golden(actual_event: &Value) {
    let golden: Value = serde_json::from_str(include_str!(
        "fixtures/objective-compacted-event-golden.json"
    ))
    .unwrap();
    let actual = actual_event["payload"].as_object().unwrap();
    let expected = golden["payload"].as_object().unwrap();
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "objective_compacted payload keys drifted"
    );
    for (key, expected_value) in expected {
        assert_eq!(
            json_type(&actual[key]),
            json_type(expected_value),
            "objective_compacted payload type drifted for {key}"
        );
    }
}

fn objective_message<'a>(
    conversation: &'a SavedConversation<ChatMessage>,
    original: &str,
) -> &'a str {
    conversation
        .messages
        .iter()
        .find(|message| {
            message.role == "user"
                && message.content.as_deref().is_some_and(|content| {
                    content == original || content.contains("AGENTLOOM-COMPACT-SUMMARY")
                })
        })
        .and_then(|message| message.content.as_deref())
        .expect("conversation should contain the objective user message")
}

#[tokio::test]
async fn compacts_objective_once_before_first_normal_turn() {
    let ws = tempfile::tempdir().unwrap();
    let original = marked_prompt(5_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(Mutex::new(0));
    let result = run_solo(
        AutoCompactProvider {
            behavior: SummaryBehavior::Success,
            context_tokens: 19_000,
            seen: seen.clone(),
            summary_calls: summary_calls.clone(),
        },
        opts(ws.path(), &original),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(*summary_calls.lock().unwrap(), 1);
    let (events, conversation) = run_files(ws.path(), &result.run_id);
    let compact = compact_events(&events);
    assert_eq!(compact.len(), 1);
    assert_eq!(compact[0]["payload"]["outcome"], "objective_compacted");
    assert_eq!(compact[0]["payload"]["through_message_id"], 2);
    assert_eq!(compact[0]["payload"]["summary"], SUMMARY);

    let objective = objective_message(&conversation, &original);
    assert!(objective.contains("AGENTLOOM-COMPACT-SUMMARY"));
    assert!(objective.contains(SUMMARY));
    assert!(!objective.contains("OLD_FOLDED_ONE_"));
    assert!(!objective.contains("OLD_FOLDED_TWO_"));
    assert!(objective.contains("KEPT_TAIL_"));

    let normal_wire = seen
        .lock()
        .unwrap()
        .iter()
        .find(|wire| {
            wire.first().is_some_and(|message| {
                !message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .contains("context checkpoint compaction for a coding-agent session")
            })
        })
        .cloned()
        .unwrap();
    let wire_frame = normal_wire
        .first()
        .filter(|message| message.role == "system")
        .and_then(|message| message.content.as_deref())
        .expect("normal wire should start with the system prompt and state frame");
    assert!(wire_frame.contains(SUMMARY));
    assert!(!wire_frame.contains("OLD_FOLDED_ONE_"));
    assert!(!wire_frame.contains("OLD_FOLDED_TWO_"));

    let wire_objective = normal_wire
        .iter()
        .find(|message| {
            message.role == "user"
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("AGENTLOOM-COMPACT-SUMMARY"))
        })
        .and_then(|message| message.content.as_deref())
        .expect("normal wire should contain the canonical objective user message");
    assert!(wire_objective.contains(SUMMARY));
    assert!(!wire_objective.contains("OLD_FOLDED_ONE_"));
    assert!(!wire_objective.contains("OLD_FOLDED_TWO_"));
}

#[tokio::test]
async fn long_marked_session_compacts_before_wire_and_completes() {
    const MESSAGE_COUNT: usize = 32;
    let ws = tempfile::tempdir().unwrap();
    let original = long_transcript(MESSAGE_COUNT, 1_200, true);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(Mutex::new(0));
    let result = run_solo(
        AutoCompactProvider {
            behavior: SummaryBehavior::Success,
            context_tokens: 19_000,
            seen: seen.clone(),
            summary_calls: summary_calls.clone(),
        },
        opts(ws.path(), &original),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(*summary_calls.lock().unwrap(), 1);
    let (events, conversation) = run_files(ws.path(), &result.run_id);
    let compact = compact_events(&events);
    assert_eq!(compact.len(), 1);
    let compact_event = compact[0];
    assert_eq!(compact_event["payload"]["outcome"], "objective_compacted");
    let through_message_id = compact_event["payload"]["through_message_id"]
        .as_i64()
        .unwrap();
    assert_eq!(through_message_id, 27);
    assert!((1..MESSAGE_COUNT as i64).contains(&through_message_id));
    assert_payload_shape_matches_golden(compact_event);
    assert_complete_journal_sequence(&events, compact_event);

    let objective = objective_message(&conversation, &original);
    assert!(objective.contains(&format!("through={through_message_id}")));
    assert!(!objective.contains("LONG_MESSAGE_27_"));
    assert!(objective.contains("LONG_MESSAGE_28_"));

    let normal_wire = seen
        .lock()
        .unwrap()
        .iter()
        .find(|wire| {
            wire.first().is_some_and(|message| {
                !message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .contains("context checkpoint compaction for a coding-agent session")
            })
        })
        .cloned()
        .expect("run should reach the normal provider turn");
    let limits = BudgetLimits::from_capabilities(&capabilities(19_000));
    let original_tokens = compact_event["payload"]["original_tokens"]
        .as_u64()
        .unwrap() as usize;
    let compacted_wire_tokens = estimate_tokens(&normal_wire, &limits);
    assert!(
        compacted_wire_tokens < original_tokens / 2
            || compacted_wire_tokens < limits.budget() * 35 / 100,
        "wire should shrink significantly: original={original_tokens}, compacted_wire={compacted_wire_tokens}"
    );
}

#[tokio::test]
async fn long_unmarked_session_salvages_head_and_completes() {
    let ws = tempfile::tempdir().unwrap();
    let original = long_transcript(32, 1_200, false);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(Mutex::new(0));
    let result = run_solo(
        AutoCompactProvider {
            behavior: SummaryBehavior::Success,
            context_tokens: 19_000,
            seen: seen.clone(),
            summary_calls: summary_calls.clone(),
        },
        opts(ws.path(), &original),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(*summary_calls.lock().unwrap(), 0);
    assert!(!seen.lock().unwrap().is_empty());
    let (events, _) = run_files(ws.path(), &result.run_id);
    let salvage: Vec<&Value> = compact_events(&events)
        .into_iter()
        .filter(|event| event["payload"]["outcome"] == "head_truncated_continue")
        .collect();
    assert_eq!(salvage.len(), 1);
    assert_complete_journal_sequence(&events, salvage[0]);
}

#[tokio::test]
async fn summary_failure_is_reported_once_and_run_continues_unchanged() {
    let ws = tempfile::tempdir().unwrap();
    let original = marked_prompt(5_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(Mutex::new(0));
    let result = run_solo(
        AutoCompactProvider {
            behavior: SummaryBehavior::Error,
            context_tokens: 19_000,
            seen,
            summary_calls: summary_calls.clone(),
        },
        opts(ws.path(), &original),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(*summary_calls.lock().unwrap(), 1);
    let (events, conversation) = run_files(ws.path(), &result.run_id);
    let compact = compact_events(&events);
    assert_eq!(compact.len(), 1);
    assert_eq!(compact[0]["payload"]["outcome"], "objective_compact_failed");
    assert_eq!(compact[0]["payload"]["reason"], "summary unavailable");
    let objective = objective_message(&conversation, &original);
    assert_eq!(objective, original);
}

#[tokio::test]
async fn summary_missing_sections_is_reported_once_and_run_continues_unchanged() {
    let ws = tempfile::tempdir().unwrap();
    let original = marked_prompt(5_000);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(Mutex::new(0));
    let result = run_solo(
        AutoCompactProvider {
            behavior: SummaryBehavior::MissingSections,
            context_tokens: 19_000,
            seen: seen.clone(),
            summary_calls: summary_calls.clone(),
        },
        opts(ws.path(), &original),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(*summary_calls.lock().unwrap(), 1);
    assert!(seen.lock().unwrap().iter().any(|wire| {
        wire.first().is_some_and(|message| {
            !message
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("context checkpoint compaction for a coding-agent session")
        })
    }));
    let (events, conversation) = run_files(ws.path(), &result.run_id);
    let compact = compact_events(&events);
    assert_eq!(compact.len(), 1);
    assert_eq!(compact[0]["payload"]["outcome"], "objective_compact_failed");
    assert_eq!(compact[0]["payload"]["reason"], "summary_missing_sections");
    let objective = objective_message(&conversation, &original);
    assert_eq!(objective.as_bytes(), original.as_bytes());
}

#[tokio::test]
async fn below_threshold_has_no_compaction_side_effects() {
    let ws = tempfile::tempdir().unwrap();
    let original = marked_prompt(20);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let result = run_solo(
        NoCompactProvider { seen: seen.clone() },
        opts(ws.path(), &original),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(seen.lock().unwrap().len(), 1);
    let (events, conversation) = run_files(ws.path(), &result.run_id);
    assert!(compact_events(&events).is_empty());
    let objective = objective_message(&conversation, &original);
    assert_eq!(objective, original);
}

#[tokio::test]
async fn post_compaction_still_over_threshold_emits_failure_and_preserves_objective() {
    let ws = tempfile::tempdir().unwrap();
    let original = format!(
        "{}{}",
        long_transcript(32, 1_400, true),
        "t".repeat(140_000)
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(Mutex::new(0));
    let result = run_solo(
        AutoCompactProvider {
            behavior: SummaryBehavior::Success,
            context_tokens: 142_548,
            seen,
            summary_calls: summary_calls.clone(),
        },
        opts(ws.path(), &original),
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(*summary_calls.lock().unwrap(), 1);
    let (events, conversation) = run_files(ws.path(), &result.run_id);
    let compact = compact_events(&events);
    assert_eq!(compact.len(), 1);
    assert_eq!(compact[0]["payload"]["outcome"], "objective_compact_failed");
    assert_eq!(
        compact[0]["payload"]["reason"],
        "summary_still_over_threshold"
    );
    assert_eq!(objective_message(&conversation, &original), original);
}

#[tokio::test]
async fn resume_compaction_threshold_ignores_messages_from_first_assistant_onward() {
    let ws = tempfile::tempdir().unwrap();
    let run_id = "resume-head-threshold";
    let original = marked_prompt(10);
    let paths = RunPaths::new(ws.path(), run_id);
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "autocompact-e2e".to_string(),
            model: "autocompact-e2e".to_string(),
            messages: vec![
                ChatMessage::system("small system prompt"),
                ChatMessage::user(&original),
                ChatMessage::assistant("x".repeat(30_000), None, Vec::new()),
            ],
        },
    )
    .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(Mutex::new(0));

    let result = resume_solo(
        AutoCompactProvider {
            behavior: SummaryBehavior::Success,
            context_tokens: 19_000,
            seen: seen.clone(),
            summary_calls: summary_calls.clone(),
        },
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        None,
        OutputMode::Silent,
        myagent::shell::PermissionPolicy::Allow,
        myagent::goal::NetworkPolicy::On,
        2,
        ControlInputKind::Sentinel,
        myagent::config::SearchChoice::Ddg,
        0,
        0,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(*summary_calls.lock().unwrap(), 0);
    assert_eq!(seen.lock().unwrap().len(), 1);
    let (events, conversation) = run_files(ws.path(), run_id);
    assert!(compact_events(&events).is_empty());
    assert_eq!(objective_message(&conversation, &original), original);
}

#[test]
fn parses_golden_transcript_and_ignores_foreign_nonce_marker() {
    let parsed = parse_transcript(TRANSCRIPT_GOLDEN).expect("golden transcript should parse");
    assert_eq!(parsed.old_summary.as_ref().unwrap().through, 2);
    assert_eq!(
        parsed.messages.iter().map(|msg| msg.id).collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    assert!(parsed.messages[2]
        .text
        .contains("===== AGENTLOOM-MSG deadbeefdeadbeefdeadbeefdeadbeef id=99 role=user ====="));
    assert!(!parsed.trailing.is_empty());
}

#[test]
fn golden_parse_render_round_trip_is_byte_exact() {
    let parsed = parse_transcript(TRANSCRIPT_GOLDEN).unwrap();
    assert_eq!(render_transcript(&parsed), TRANSCRIPT_GOLDEN);
}

#[test]
fn parses_lead_golden_transcript_and_preserves_contract() {
    let parsed =
        parse_transcript(LEAD_TRANSCRIPT_GOLDEN).expect("lead golden transcript should parse");
    let summary = parsed.old_summary.as_ref().expect("compact summary");
    assert_eq!(summary.through, 2);
    assert_eq!(
        summary.text,
        "Golden fixture captures the lead transcript contract.\n\
Both consumers must stay byte-compatible.\n"
    );
    assert_eq!(
        parsed
            .messages
            .iter()
            .map(|message| (message.id, message.role.as_str()))
            .collect::<Vec<_>>(),
        vec![(3, "user"), (4, "assistant"), (5, "user")]
    );
    assert!(parsed.messages[0]
        .text
        .contains("===== AGENTLOOM-MSG deadbeefdeadbeefdeadbeefdeadbeef id=99 role=user ====="));
    assert_eq!(
        parsed.messages.len(),
        3,
        "foreign nonce must not split a message"
    );

    let history_end = "===== AGENTLOOM-HISTORY-END 0123456789abcdef0123456789abcdef =====\n";
    let expected_trailing = LEAD_TRANSCRIPT_GOLDEN
        .split_once(history_end)
        .expect("history end in lead golden")
        .1;
    assert_eq!(parsed.trailing, expected_trailing);
    assert!(parsed
        .trailing
        .starts_with("\n\nRestate next step: Run both consumer tests"));
    assert!(parsed
        .trailing
        .ends_with("case-card or these memory updates in your reply to the user."));
}

#[test]
fn lead_golden_parse_render_round_trip_is_byte_exact() {
    let parsed = parse_transcript(LEAD_TRANSCRIPT_GOLDEN).unwrap();
    assert_eq!(render_transcript(&parsed), LEAD_TRANSCRIPT_GOLDEN);
}

#[test]
fn no_summary_parse_render_round_trip_is_byte_exact() {
    let input = format!(
        "prefix\n===== AGENTLOOM-MSG {NONCE} id=-7 role=user =====\nfirst\n\n===== AGENTLOOM-MSG {NONCE} id=8 role=assistant =====\nsecond\n===== AGENTLOOM-HISTORY-END {NONCE} =====\nlatest tail\n"
    );
    let parsed = parse_transcript(&input).unwrap();
    assert!(parsed.old_summary.is_none());
    assert_eq!(render_transcript(&parsed), input);
}

#[test]
fn rejects_malformed_transcript_boundaries() {
    assert!(parse_transcript("ordinary task text\n").is_none());
    let missing_end = format!("===== AGENTLOOM-MSG {NONCE} id=1 role=user =====\nmessage\n");
    assert!(parse_transcript(&missing_end).is_none());
    let unclosed_summary = format!(
        "===== AGENTLOOM-COMPACT-SUMMARY {NONCE} through=1 =====\nsummary\n===== AGENTLOOM-MSG {NONCE} id=2 role=user =====\nmessage\n===== AGENTLOOM-HISTORY-END {NONCE} =====\ntail\n"
    );
    assert!(parse_transcript(&unclosed_summary).is_none());
    let selected_nonce_after_end = format!(
        "===== AGENTLOOM-MSG {NONCE} id=1 role=user =====\nmessage\n===== AGENTLOOM-HISTORY-END {NONCE} =====\ntail\n===== AGENTLOOM-MSG {NONCE} id=2 role=user =====\ninjected\n"
    );
    assert!(parse_transcript(&selected_nonce_after_end).is_none());
}

#[test]
fn should_compact_at_exact_35_percent_boundary() {
    let limits = unit_limits(100);
    assert!(!should_compact(34, &limits));
    assert!(should_compact(35, &limits));
}

#[test]
fn tail_selection_keeps_whole_messages() {
    let mut parsed = parsed_transcript(&[
        (1, "user", "1111"),
        (2, "user", "2222"),
        (3, "user", "3333"),
    ]);
    parsed.old_summary = Some(OldSummary {
        through: 0,
        text: "older checkpoint\n".to_string(),
    });
    let plan = plan_compaction(&parsed, &unit_limits(100)).unwrap();
    assert_eq!(plan.tail_start_idx, 1);
    assert_eq!(plan.through_message_id, 1);
    assert!(plan.fold_input.contains("先前检查点摘要"));
    assert!(plan.fold_input.contains("older checkpoint"));
    assert!(plan.fold_input.contains("id=1 role=user"));
    assert!(!plan.fold_input.contains("id=2 role=user"));
}

#[test]
fn tail_selection_boundaries_are_stable() {
    let parsed = parsed_transcript(&[
        (1, "user", "old"),
        (2, "user", "new message far over budget"),
    ]);
    let plan = plan_compaction(&parsed, &unit_limits(1)).unwrap();
    assert_eq!(plan.tail_start_idx, 1);
    assert_eq!(plan.through_message_id, 1);

    let large = "x".repeat(40_000);
    let parsed = parsed_transcript(&[
        (1, "user", &large),
        (2, "user", &large),
        (3, "user", &large),
    ]);
    let plan = plan_compaction(&parsed, &unit_limits(1_000_000)).unwrap();
    assert_eq!(plan.tail_start_idx, 1);
    assert_eq!(plan.through_message_id, 1);

    let parsed = parsed_transcript(&[(1, "user", "one"), (2, "assistant", "two")]);
    assert!(plan_compaction(&parsed, &unit_limits(1_000)).is_none());
}

#[test]
fn rebuild_updates_watermark_and_preserves_tail_and_trailing() {
    let parsed = parse_transcript(TRANSCRIPT_GOLDEN).unwrap();
    let plan = CompactPlan {
        fold_input: "unused in rebuild".to_string(),
        tail_start_idx: 1,
        through_message_id: 3,
    };
    let tail = format!(
        "===== AGENTLOOM-MSG {NONCE} id=4 role=assistant =====\n{}===== AGENTLOOM-MSG {NONCE} id=5 role=user =====\n{}",
        parsed.messages[1].text, parsed.messages[2].text
    );
    let rebuilt = rebuild(&parsed, &plan, "new compact summary");
    assert!(rebuilt.contains(&format!(
        "===== AGENTLOOM-COMPACT-SUMMARY {NONCE} through=3 =====\nnew compact summary\n===== /AGENTLOOM-COMPACT-SUMMARY {NONCE} =====\n"
    )));
    assert!(rebuilt.contains(&tail));
    assert!(rebuilt.ends_with(&parsed.trailing));
    assert!(!rebuilt.contains("id=3 role=user"));
}
