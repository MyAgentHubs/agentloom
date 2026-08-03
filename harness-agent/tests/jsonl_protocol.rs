use myagent::events::{EventEnvelope, EventRecorder, OutputMode, SCHEMA_VERSION};

#[test]
fn schema_version_is_frozen_v1() {
    assert_eq!(SCHEMA_VERSION, "harness.runtime.v1");
}

#[test]
fn each_event_type_round_trips_through_journal() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("events.jsonl");
    let mut rec = EventRecorder::new(
        "run_golden",
        Some("sid".into()),
        Some("/ws".into()),
        &journal,
        OutputMode::Silent,
    )
    .unwrap();
    let types = [
        "run.started",
        "goal.created",
        "orchestration.step.started",
        "agent.note.delta",
        "agent.reasoning.delta",
        "tool.started",
        "tool.completed",
        "completion.evaluated",
        "run.completed",
    ];
    for t in types {
        rec.emit(t, serde_json::json!({"k":"v"})).unwrap();
    }
    let lines: Vec<EventEnvelope> = std::fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), types.len());
    for (i, e) in lines.iter().enumerate() {
        assert_eq!(e.schema_version, "harness.runtime.v1");
        assert_eq!(e.seq, (i as u64) + 1);
        assert_eq!(e.run_id, "run_golden");
        assert_eq!(e.client_session_id.as_deref(), Some("sid"));
        assert_eq!(e.workspace.as_deref(), Some("/ws"));
        assert_eq!(e.event_type, types[i]);
        assert!(!e.event_id.is_empty());
        assert!(!e.ts.is_empty());
    }
}

#[test]
fn type_field_serializes_as_type_not_event_type() {
    // guard the serde rename so IDE/product consumers see "type"
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("e.jsonl");
    let mut rec = EventRecorder::new("run_x", None, None, &journal, OutputMode::Silent).unwrap();
    rec.emit("run.started", serde_json::json!({})).unwrap();
    let raw = std::fs::read_to_string(&journal).unwrap();
    assert!(
        raw.contains("\"type\":\"run.started\""),
        "envelope must serialize key as \"type\": {raw}"
    );
    assert!(raw.contains("\"schema_version\":\"harness.runtime.v1\""));
}
