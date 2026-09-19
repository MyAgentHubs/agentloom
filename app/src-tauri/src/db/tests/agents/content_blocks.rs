#![cfg(test)]

use super::*;

// 刀 R P0-1：新增 3 个 Block 变体的 serde 往返。

#[test]
fn block_approval_roundtrip_with_missing_request_kind_defaults_none() {
    // 前端形状：request_kind 缺省（未带这个 key）时应反序列化成 None。
    let json = r#"{
            "type": "approval",
            "approval_id": "ap1",
            "run_id": "r1",
            "tool": "bash",
            "command": "rm -rf /tmp/x",
            "summary": "删除临时文件",
            "cwd": "/repo",
            "status": "pending"
        }"#;
    let block: Block = serde_json::from_str(json).unwrap();
    assert_eq!(
        block,
        Block::Approval {
            approval_id: "ap1".into(),
            run_id: "r1".into(),
            tool: "bash".into(),
            command: "rm -rf /tmp/x".into(),
            summary: "删除临时文件".into(),
            cwd: "/repo".into(),
            request_kind: None,
            status: "pending".into(),
        }
    );
    // 往返：再序列化回 JSON、再解回应相等。
    let round: Block = serde_json::from_str(&serde_json::to_string(&block).unwrap()).unwrap();
    assert_eq!(round, block);
}

#[test]
fn block_approval_roundtrip_with_request_kind_present() {
    let json = r#"{
            "type": "approval",
            "approval_id": "ap2",
            "run_id": "r1",
            "tool": "bash",
            "command": "git push",
            "summary": "push",
            "cwd": "/repo",
            "request_kind": "scope_change",
            "status": "approved"
        }"#;
    let block: Block = serde_json::from_str(json).unwrap();
    assert_eq!(
        block,
        Block::Approval {
            approval_id: "ap2".into(),
            run_id: "r1".into(),
            tool: "bash".into(),
            command: "git push".into(),
            summary: "push".into(),
            cwd: "/repo".into(),
            request_kind: Some("scope_change".into()),
            status: "approved".into(),
        }
    );
}

#[test]
fn block_scope_change_roundtrip() {
    let json = r#"{
            "type": "scope_change",
            "changes": [
                {
                    "proposal_id": "p1",
                    "kind": "objective",
                    "detail_text": "把范围从 A 扩到 A+B",
                    "detail_summary": "扩范围"
                }
            ]
        }"#;
    let block: Block = serde_json::from_str(json).unwrap();
    assert_eq!(
        block,
        Block::ScopeChange {
            changes: vec![crate::agent_event::ScopeChange {
                proposal_id: "p1".into(),
                kind: "objective".into(),
                detail_text: "把范围从 A 扩到 A+B".into(),
                detail_summary: Some("扩范围".into()),
            }],
        }
    );
    let round: Block = serde_json::from_str(&serde_json::to_string(&block).unwrap()).unwrap();
    assert_eq!(round, block);
}

#[test]
fn block_run_terminal_roundtrip_with_message() {
    let json = r#"{
            "type": "run_terminal",
            "run_id": "r1",
            "status": "error",
            "message": "工具异常退出：exit code 1"
        }"#;
    let block: Block = serde_json::from_str(json).unwrap();
    assert_eq!(
        block,
        Block::RunTerminal {
            run_id: "r1".into(),
            status: "error".into(),
            message: Some("工具异常退出：exit code 1".into()),
        }
    );
    let round: Block = serde_json::from_str(&serde_json::to_string(&block).unwrap()).unwrap();
    assert_eq!(round, block);
}

#[test]
fn block_run_terminal_roundtrip_without_message() {
    let json = r#"{
            "type": "run_terminal",
            "run_id": "r1",
            "status": "completed"
        }"#;
    let block: Block = serde_json::from_str(json).unwrap();
    assert_eq!(
        block,
        Block::RunTerminal {
            run_id: "r1".into(),
            status: "completed".into(),
            message: None,
        }
    );
    // skip_serializing_if：序列化回去应不含 "message" key。
    let serialized = serde_json::to_string(&block).unwrap();
    assert!(
        !serialized.contains("\"message\""),
        "message=None 应被 skip_serializing_if 略去：{serialized}"
    );
}
