#![cfg(test)]

use super::*;

#[test]
fn agent_event_envelope_serializes_flat_session_id() {
    let cases = [
        agent_event::AgentEvent::TextDelta { text: "hi".into() },
        agent_event::AgentEvent::Completed {
            cost_usd: Some(0.12),
            input_tokens: Some(10),
            output_tokens: Some(20),
            final_text: Some("done".into()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        },
        agent_event::AgentEvent::Error {
            message: "boom".into(),
        },
    ];

    for event in &cases {
        let v = serde_json::to_value(AgentEventEnvelope {
            session_id: "s-1",
            dispatch: None,
            event,
        })
        .unwrap();
        assert_eq!(v["session_id"], "s-1");
        assert!(v.get("kind").is_some(), "{v}");
        assert!(
            v.get("event").is_none(),
            "event 字段必须被 flatten 到顶层：{v}"
        );
    }
}

#[test]
fn envelope_without_dispatch_omits_dispatch_key() {
    let ev = agent_event::AgentEvent::TextDelta { text: "hi".into() };
    let v = serde_json::to_value(AgentEventEnvelope {
        session_id: "s1",
        dispatch: None,
        event: &ev,
    })
    .unwrap();
    assert_eq!(v["session_id"], "s1");
    assert_eq!(v["kind"], "text_delta");
    assert_eq!(v["text"], "hi");
    // Normal 路径：不出现 dispatch 键、也不出现任何派单字段（逐字节兼容旧前端）
    assert!(v.get("dispatch").is_none());
    assert!(v.get("run_id").is_none());
    assert!(v.get("assignment_id").is_none());
}

#[test]
fn envelope_with_dispatch_nests_under_dispatch_key() {
    // R1：dispatch 是嵌套对象（不 flatten）——派单 run_id 落在 dispatch.run_id，
    // 与 Completed.run_id（git run）永不在顶层撞 key。
    let ev = agent_event::AgentEvent::Completed {
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        final_text: None,
        result: None,
        run_id: Some("gitrun-9".into()),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: None,
    };
    let d = agent_event::DispatchMeta {
        run_id: Some("dispatch-r1".into()),
        assignment_id: Some("a1".into()),
        ..Default::default()
    };
    let v = serde_json::to_value(AgentEventEnvelope {
        session_id: "s1",
        dispatch: Some(d),
        event: &ev,
    })
    .unwrap();
    assert_eq!(v["kind"], "completed");
    assert_eq!(v["dispatch"]["run_id"], "dispatch-r1"); // 派单 run
    assert_eq!(v["dispatch"]["assignment_id"], "a1");
    assert_eq!(v["run_id"], "gitrun-9"); // git run（顶层·event flatten）
}
