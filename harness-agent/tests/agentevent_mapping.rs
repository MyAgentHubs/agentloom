// Locks spec §9.1 mapping (harness JSONL event type -> product AgentEvent) before M2.
// Test-only mirror of the product's AgentEvent variants; not a dependency on product code.

#[derive(Debug, PartialEq)]
enum Mapped {
    SessionStarted,
    TextDelta,
    ThinkingDelta,
    ToolStarted { card: &'static str },
    ToolCompleted,
    Error,
    FinalizerOwned, // run.completed -> product finalizer synthesizes Completed (+git fields); not direct render
    Dropped, // product AgentEvent currently has no matching variant (M2 must add: approval/blocked/...)
}

fn map(event_type: &str, payload: &serde_json::Value) -> Mapped {
    match event_type {
        "run.started" => Mapped::SessionStarted,
        "agent.note.delta" => Mapped::TextDelta,
        "agent.reasoning.delta" => Mapped::ThinkingDelta,
        "tool.started" => {
            let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
            Mapped::ToolStarted {
                card: if tool == "shell_exec" {
                    "command"
                } else {
                    "compact"
                },
            }
        }
        "tool.completed" | "tool.failed" => Mapped::ToolCompleted,
        "run.completed" => Mapped::FinalizerOwned,
        "run.failed" => Mapped::Error,
        "approval.requested"
        | "approval.resolved"
        | "goal.change.proposed"
        | "goal.updated"
        | "goal.change.approved"
        | "goal.change.rejected"
        | "evidence.probe.registered"
        | "evidence.probe.rejected"
        | "evidence.gate.bypassed"
        | "evidence.edit.blocked"
        | "evidence.probe.green"
        | "evidence.probe.still_red"
        | "evidence.probe.infra"
        | "evidence.probe.workspace_mutated"
        | "run.blocked"
        | "run.needs_decision"
        | "plan.worklist.accepted"
        | "plan.worklist.bounced"
        | "plan.preflight.considered"
        | "plan.preflight.proceed"
        | "plan.preflight.pre_green"
        | "plan.preflight.refine_requested"
        | "plan.preflight.refine_planned"
        | "plan.preflight.refine_bounced"
        | "plan.preflight.refine_escalated"
        | "plan.preflight.refine_appended"
        | "plan.preflight.superseded"
        | "plan.preflight.suspended"
        | "plan.preflight.escalated"
        | "plan.task.report"
        | "plan.task.decision"
        | "plan.task.done"
        | "plan.task.blocked"
        | "plan.task.reverified"
        | "plan.task.advisory"
        | "plan.task.scope_formatting_advisory"
        | "plan.replan.considered"
        | "plan.replan.planned"
        | "plan.replan.bounced"
        | "plan.replan.escalated"
        | "plan.replan.appended"
        | "plan.replan.reverified"
        | "artifact.created"
        | "completion.evaluated"
        | "completion.rejected"
        | "capabilities.declared"
        | "goal.created"
        | "orchestration.step.started"
        | "orchestration.step.completed"
        | "context.pack.attached"
        | "memory.lessons.retrieved"
        | "tool.stdout.delta"
        | "tool.stderr.delta"
        | "judge.evaluated"
        | "validation.checked"
        | "provider.turn.finished"
        | "provider.warning"
        | "mcp.server.failed"
        | "run.resumed"
        | "run.interrupted" => Mapped::Dropped, // M2: add product variants for approval/blocked (others may stay dropped)
        other => panic!("unmapped event type: {other}"),
    }
}

fn map_optional(event_type: &str, payload: &serde_json::Value) -> Option<Mapped> {
    if myagent::vocabulary::is_known(event_type) {
        Some(map(event_type, payload))
    } else {
        None
    }
}

#[test]
fn mapping_matches_spec_9_1() {
    use serde_json::json;
    assert_eq!(map("run.started", &json!({})), Mapped::SessionStarted);
    assert_eq!(map("agent.note.delta", &json!({})), Mapped::TextDelta);
    assert_eq!(
        map("agent.reasoning.delta", &json!({})),
        Mapped::ThinkingDelta
    );
    assert_eq!(
        map("tool.started", &json!({"tool":"shell_exec"})),
        Mapped::ToolStarted { card: "command" }
    );
    assert_eq!(
        map("tool.started", &json!({"tool":"fs_edit"})),
        Mapped::ToolStarted { card: "compact" }
    );
    assert_eq!(map("tool.completed", &json!({})), Mapped::ToolCompleted);
    assert_eq!(map("tool.failed", &json!({})), Mapped::ToolCompleted);
    assert_eq!(map("run.completed", &json!({})), Mapped::FinalizerOwned);
    assert_eq!(map("run.failed", &json!({})), Mapped::Error);
    assert_eq!(map("approval.requested", &json!({})), Mapped::Dropped);
    assert_eq!(map("run.blocked", &json!({})), Mapped::Dropped);
}

#[test]
fn vocabulary_has_at_least_58_types_and_excludes_ghosts() {
    use myagent::vocabulary::{is_known, VOCABULARY};
    assert!(VOCABULARY.len() >= 58);
    assert!(!is_known("error"), "ghost error must not be in vocabulary");
    for real in [
        "run.started",
        "run.needs_decision",
        "plan.worklist.accepted",
        "plan.task.done",
        "plan.replan.appended",
        "goal.updated",
        "goal.change.approved",
        "goal.change.rejected",
        "goal.change.proposed",
        "approval.requested",
    ] {
        assert!(is_known(real), "{real} must be in vocabulary");
    }
}

#[test]
fn unknown_event_type_is_ignored_not_panicked() {
    assert!(map_optional("totally.unknown.future.event", &serde_json::json!({})).is_none());
}

#[test]
fn every_vocabulary_type_has_disposition() {
    // 词汇表内每个 type 都有明确处置（映射或显式 drop），不 panic
    for ty in myagent::vocabulary::VOCABULARY {
        let _ = map_optional(ty, &serde_json::json!({}));
    }
}
