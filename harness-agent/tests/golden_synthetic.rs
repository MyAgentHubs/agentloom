use myagent::control::ControlCommand;
use myagent::events::{EventEnvelope, SCHEMA_VERSION};

fn envelope(seq: u64, ty: &str, payload: serde_json::Value) -> EventEnvelope {
    EventEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        event_id: format!("evt_{seq:06}"),
        seq,
        ts: "2026-06-09T00:00:00+00:00".into(),
        run_id: "run_synth".into(),
        client_session_id: None,
        workspace: None,
        event_type: ty.into(),
        payload,
    }
}

#[test]
fn needs_decision_shape_round_trips() {
    let e = envelope(
        7,
        "run.needs_decision",
        serde_json::json!({
            "reason": "scope_change",
            "changes": [{
                "proposal_id": "proposal_call_scope_1",
                "kind": "scope",
                "detail": { "text": "refactor whole module", "summary": "widen scope" }
            }]
        }),
    );
    let back: EventEnvelope = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    assert_eq!(back.event_type, "run.needs_decision");
    assert_eq!(back.payload["reason"].as_str().unwrap(), "scope_change");
    assert_eq!(
        back.payload["changes"][0]["kind"].as_str().unwrap(),
        "scope"
    );
    assert_eq!(
        back.payload["changes"][0]["detail"]["summary"]
            .as_str()
            .unwrap(),
        "widen scope"
    );
}

#[test]
fn needs_decision_long_task_shape_round_trips_single_handle() {
    let e = envelope(
        9,
        "run.needs_decision",
        serde_json::json!({
            "reason": "long_task",
            "handles": [{
                "handle_id": "ci-run-4821",
                "kind": "ci_run",
                "description": "wait for the deploy workflow (~40 min)"
            }]
        }),
    );
    let back: EventEnvelope = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    assert_eq!(back.event_type, "run.needs_decision");
    assert_eq!(back.payload["reason"].as_str().unwrap(), "long_task");
    let handles = back.payload["handles"].as_array().unwrap();
    assert_eq!(handles.len(), 1, "单句柄也必须是数组");
    assert_eq!(handles[0]["handle_id"].as_str().unwrap(), "ci-run-4821");
    assert_eq!(handles[0]["kind"].as_str().unwrap(), "ci_run");
}

#[test]
fn needs_decision_long_task_multi_handles_description_optional() {
    let e = envelope(
        10,
        "run.needs_decision",
        serde_json::json!({
            "reason": "long_task",
            "handles": [
                { "handle_id": "h1", "kind": "process" },
                { "handle_id": "h2", "kind": "deploy", "description": "prod rollout" }
            ]
        }),
    );
    let back: EventEnvelope = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    let handles = back.payload["handles"].as_array().unwrap();
    assert_eq!(handles.len(), 2);
    // description 可选：缺失（== null）合法
    assert!(handles[0].get("description").is_none());
    assert_eq!(handles[1]["description"].as_str().unwrap(), "prod rollout");
    // handle_id / kind 必填
    for h in handles {
        assert!(h["handle_id"].is_string());
        assert!(h["kind"].is_string());
    }
}

#[test]
fn goal_change_proposed_criterion_shape_round_trips() {
    let e = envelope(
        5,
        "goal.change.proposed",
        serde_json::json!({
            "proposal_id": "proposal_call_crit_1",
            "kind": "criterion",
            "summary": "tests pass",
            "authored_by": "agent",
            "draft": {
                "id": "c2", "claim": "tests pass", "authored_by": "agent", "approval": "pending",
                "verifier": { "kind": "verifiable", "check_cmd": "cargo test", "success": "exit_zero", "timeout_s": 120 },
                "status": "pending"
            }
        }),
    );
    let back: EventEnvelope = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    assert_eq!(back.payload["kind"].as_str().unwrap(), "criterion");
    assert_eq!(
        back.payload["draft"]["approval"].as_str().unwrap(),
        "pending"
    );
}

#[test]
fn goal_change_proposed_scope_shape_round_trips() {
    // scope 类 proposed 带 detail 对象（R-opus·让「形状一次定死」名副其实）
    let e = envelope(
        5,
        "goal.change.proposed",
        serde_json::json!({
            "proposal_id": "proposal_call_scope_1",
            "kind": "scope",
            "summary": "widen scope",
            "authored_by": "agent",
            "detail": { "text": "refactor whole module", "summary": "widen scope" }
        }),
    );
    let back: EventEnvelope = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    assert_eq!(back.payload["kind"].as_str().unwrap(), "scope");
    assert_eq!(
        back.payload["detail"]["summary"].as_str().unwrap(),
        "widen scope"
    );
}

#[test]
fn goal_change_approved_and_rejected_shape_round_trip() {
    let approved = envelope(
        6,
        "goal.change.approved",
        serde_json::json!({
            "proposal_id": "proposal_call_crit_1", "kind": "criterion",
            "criterion_id": "c2", "applied": true
        }),
    );
    let back: EventEnvelope =
        serde_json::from_str(&serde_json::to_string(&approved).unwrap()).unwrap();
    assert!(back.payload["applied"].as_bool().unwrap());

    let rejected = envelope(
        6,
        "goal.change.rejected",
        serde_json::json!({
            "proposal_id": "proposal_call_crit_1", "kind": "criterion", "reason": "not relevant"
        }),
    );
    let back: EventEnvelope =
        serde_json::from_str(&serde_json::to_string(&rejected).unwrap()).unwrap();
    assert_eq!(
        back.payload["proposal_id"].as_str().unwrap(),
        "proposal_call_crit_1"
    );
}

#[test]
fn goal_updated_shape_round_trips() {
    let e = envelope(
        7,
        "goal.updated",
        serde_json::json!({
            "proposal_id": "proposal_call_crit_1",
            "criteria": [{
                "id": "c2", "claim": "tests pass", "authored_by": "agent", "approval": "approved",
                "verifier": { "kind": "verifiable", "check_cmd": "cargo test", "success": "exit_zero", "timeout_s": 120 },
                "status": "pending"
            }]
        }),
    );
    let back: EventEnvelope = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    assert_eq!(
        back.payload["criteria"][0]["approval"].as_str().unwrap(),
        "approved"
    );
}

#[test]
fn provider_warning_shape_round_trips() {
    // 真实 emit 形状 = {warning, error}（openai_compatible.rs），mock 驱不出 → 合成
    let e = envelope(
        3,
        "provider.warning",
        serde_json::json!({ "warning": "invalid_sse_json", "error": "expected value at line 1" }),
    );
    let back: EventEnvelope = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    assert_eq!(back.event_type, "provider.warning");
    assert_eq!(
        back.payload["warning"].as_str().unwrap(),
        "invalid_sse_json"
    );
    assert!(back.payload.get("error").is_some());
}

#[test]
fn control_commands_serde_round_trip_and_fixture() {
    // 冻结 stdin 控制命令的线上 JSON 形状（tag="type" + snake_case）
    let cmds = [
        ControlCommand::Stop { run_id: "r".into() },
        ControlCommand::Approve {
            run_id: "r".into(),
            approval_id: "a".into(),
        },
        ControlCommand::Reject {
            run_id: "r".into(),
            approval_id: "a".into(),
        },
        ControlCommand::Pause { run_id: "r".into() },
        ControlCommand::Resume { run_id: "r".into() },
        ControlCommand::Revise {
            run_id: "r".into(),
            message: "m".into(),
        },
        ControlCommand::InspectRuntime { run_id: "r".into() },
    ];
    let lines: Vec<String> = cmds
        .iter()
        .map(|c| serde_json::to_string(c).unwrap())
        .collect();
    // 形状钉桩
    assert!(lines[0].contains("\"type\":\"stop\""));
    assert!(
        lines[1].contains("\"type\":\"approve\"") && lines[1].contains("\"approval_id\":\"a\"")
    );
    // 每条都能 round-trip
    for (c, l) in cmds.iter().zip(&lines) {
        let _back: ControlCommand = serde_json::from_str(l).unwrap();
        let _ = c;
    }
    // 落盘 fixture（缺则写）
    let got = lines.join("\n") + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/synthetic/control_commands.jsonl");
    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
        panic!("control fixture created — review & re-run");
    }
    assert_eq!(
        got,
        std::fs::read_to_string(&path).unwrap(),
        "control fixture drift"
    );
}
