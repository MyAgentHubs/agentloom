#![cfg(test)]

use super::*;

#[test]
fn display_reduce_hides_orchestration_tool_ask_user() {
    let mut reducer = display_reduce::DisplayReducer::new("run-hide-1");
    reducer.feed(&agent_event::AgentEvent::ToolStarted {
        id: "call-1".into(),
        tool: "mcp__agentloom__ask_user".into(),
        summary: "问用户".into(),
        card: agent_event::CardKind::Command,
    });
    reducer.feed(&agent_event::AgentEvent::ToolCompleted {
        id: "call-1".into(),
        status: agent_event::ToolStatus::Ok,
        exit_code: Some(0),
        output: Some("ok".into()),
    });
    reducer.feed(&agent_event::AgentEvent::TextDelta {
        text: "继续处理任务".into(),
    });
    reducer.feed(&agent_event::AgentEvent::Completed {
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        final_text: None,
        result: None,
        run_id: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: None,
    });
    let outcome = base_run_outcome("run-hide-1");
    let msg = reducer.finish(&outcome).expect("run 门槛已过·应有产出");
    assert!(
        !msg.blocks.iter().any(|b| matches!(b, Block::Tool { .. })),
        "隐藏工具不应建卡：{:?}",
        msg.blocks
    );
    assert!(msg
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Text { text } if text == "继续处理任务")));
    assert!(msg
        .blocks
        .iter()
        .any(|b| matches!(b, Block::RunTerminal { status, .. } if status == "completed")));
}

#[test]
fn display_reduce_hides_tool_search_keeps_normal_tool() {
    let mut reducer = display_reduce::DisplayReducer::new("run-hide-2");
    reducer.feed(&agent_event::AgentEvent::ToolStarted {
        id: "call-search".into(),
        tool: "ToolSearch".into(),
        summary: "搜索工具".into(),
        card: agent_event::CardKind::Command,
    });
    reducer.feed(&agent_event::AgentEvent::ToolCompleted {
        id: "call-search".into(),
        status: agent_event::ToolStatus::Ok,
        exit_code: Some(0),
        output: Some("found".into()),
    });
    reducer.feed(&agent_event::AgentEvent::ToolStarted {
        id: "call-bash".into(),
        tool: "Bash".into(),
        summary: "ls".into(),
        card: agent_event::CardKind::Command,
    });
    reducer.feed(&agent_event::AgentEvent::ToolCompleted {
        id: "call-bash".into(),
        status: agent_event::ToolStatus::Ok,
        exit_code: Some(0),
        output: Some("file.txt".into()),
    });
    let outcome = base_run_outcome("run-hide-2");
    let msg = reducer.finish(&outcome).expect("run 门槛已过·应有产出");
    let tool_blocks: Vec<&Block> = msg
        .blocks
        .iter()
        .filter(|b| matches!(b, Block::Tool { .. }))
        .collect();
    assert_eq!(
        tool_blocks.len(),
        1,
        "只应有 Bash 一张工具卡：{:?}",
        msg.blocks
    );
    if let Block::Tool { tool, .. } = tool_blocks[0] {
        assert_eq!(tool, "Bash");
    } else {
        unreachable!();
    }
}

#[test]
fn display_reduce_hidden_tool_output_delta_does_not_leak_into_blocks() {
    let mut reducer = display_reduce::DisplayReducer::new("run-hide-3");
    reducer.feed(&agent_event::AgentEvent::ToolStarted {
        id: "call-hidden".into(),
        tool: "mcp__agentloom__dispatch_worker".into(),
        summary: "派单".into(),
        card: agent_event::CardKind::Command,
    });
    // 大量输出 delta 落在隐藏工具 id 上——归约器应静默丢弃、不建块也不累积到任何块。
    for _ in 0..50 {
        reducer.feed(&agent_event::AgentEvent::ToolOutputDelta {
            id: "call-hidden".into(),
            text: "x".repeat(1024),
        });
    }
    reducer.feed(&agent_event::AgentEvent::ToolCompleted {
        id: "call-hidden".into(),
        status: agent_event::ToolStatus::Ok,
        exit_code: Some(0),
        output: None,
    });
    // Finding C：整轮只有隐藏工具、无任何可见块 = 空气泡，finish 会返回 None
    // （见 display_reduce_empty_completed_run_skips_flush）。这里补一条可见叙述，
    // 让这条用例继续聚焦本题——隐藏工具的输出 delta 不应催生任何工具块。
    reducer.feed(&agent_event::AgentEvent::TextDelta {
        text: "派单完成".into(),
    });
    reducer.feed(&agent_event::AgentEvent::Completed {
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        final_text: None,
        result: None,
        run_id: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: None,
    });
    let outcome = base_run_outcome("run-hide-3");
    let msg = reducer.finish(&outcome).expect("run 门槛已过·应有产出");
    assert!(
        !msg.blocks.iter().any(|b| matches!(b, Block::Tool { .. })),
        "隐藏工具的输出 delta 不应催生任何工具块：{:?}",
        msg.blocks
    );
    assert!(msg
        .blocks
        .iter()
        .any(|b| matches!(b, Block::RunTerminal { status, .. } if status == "completed")));
}

#[test]
fn display_reduce_empty_completed_run_skips_flush() {
    // Finding C（空气泡）：只喂 SessionStarted + Completed（无任何内容块）——
    // finish 应返回 None，避免落一条只有空 RunTerminal 的「空气泡」消息。
    let mut reducer = display_reduce::DisplayReducer::new("run-empty-1");
    reducer.feed(&agent_event::AgentEvent::SessionStarted {
        conversation_id: "sess-empty".into(),
    });
    reducer.feed(&agent_event::AgentEvent::Completed {
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        final_text: None,
        result: None,
        run_id: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: None,
    });
    let outcome = base_run_outcome("run-empty-1");
    assert!(
        reducer.finish(&outcome).is_none(),
        "空轮 completed 不应落库"
    );
}
