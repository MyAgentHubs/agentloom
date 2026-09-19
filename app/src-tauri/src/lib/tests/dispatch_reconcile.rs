#![cfg(test)]

use super::*;

fn running_dispatch_card_for_reconcile(assignment_id: &str) -> db::Block {
    db::Block::DispatchCard {
        run_id: format!("worker-run-{assignment_id}"),
        member: db::MemberSnapshot {
            participant_id: "worker-1".into(),
            assignment_id: assignment_id.into(),
            task_id: "task-1".into(),
            name: "Codex Worker".into(),
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

#[test]
fn reconcile_running_dispatch_cards_stamps_report_before_lead_message_persists() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-dispatch-card-report-first",
        "x",
        "local-default",
        "local",
    )
    .unwrap();
    let report = "[Worker report]\nagent: Codex Worker\nassignment_id: assignment-1\nstatus: done\nfinal_text:\nfinished";
    db::append_message(
        &conn,
        "s-dispatch-card-report-first",
        "assistant",
        &[db::Block::Text {
            text: report.into(),
        }],
        Some("agent-team"),
        Some("worker-agent"),
        Some("Codex Worker"),
    )
    .unwrap();
    let mut lead_blocks = vec![running_dispatch_card_for_reconcile("assignment-1")];

    reconcile_running_dispatch_cards(&conn, "s-dispatch-card-report-first", &mut lead_blocks);
    db::append_message(
        &conn,
        "s-dispatch-card-report-first",
        "assistant",
        &lead_blocks,
        Some("agent-team"),
        Some("lead-agent"),
        Some("Lead"),
    )
    .unwrap();

    let messages = db::get_messages(&conn, "s-dispatch-card-report-first").unwrap();
    let db::Block::DispatchCard { member, .. } = &messages[1].content[0] else {
        panic!("expected dispatch card");
    };
    assert_eq!(member.status, "done");
    assert!(!member.failed);
    assert_eq!(
        member.blocks,
        vec![db::Block::Text {
            text: report.into()
        }]
    );
}

#[test]
fn reconcile_running_dispatch_cards_hydrates_empty_done_card_without_changing_terminal_state() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-dispatch-card-empty-done",
        "x",
        "local-default",
        "local",
    )
    .unwrap();
    let report = "[Worker report]\nagent: Codex Worker\nassignment_id: assignment-1\nstatus: failed\nfinal_text:\nfinished";
    db::append_message(
        &conn,
        "s-dispatch-card-empty-done",
        "assistant",
        &[db::Block::Text {
            text: report.into(),
        }],
        Some("agent-team"),
        Some("worker-agent"),
        Some("Codex Worker"),
    )
    .unwrap();
    let mut lead_blocks = vec![running_dispatch_card_for_reconcile("assignment-1")];
    let db::Block::DispatchCard { member, .. } = &mut lead_blocks[0] else {
        unreachable!();
    };
    member.status = "done".into();

    reconcile_running_dispatch_cards(&conn, "s-dispatch-card-empty-done", &mut lead_blocks);

    let db::Block::DispatchCard { member, .. } = &lead_blocks[0] else {
        panic!("expected dispatch card");
    };
    assert_eq!(member.status, "done");
    assert!(!member.failed);
    assert_eq!(
        member.blocks,
        vec![db::Block::Text {
            text: report.into()
        }]
    );
}

#[test]
fn reconcile_running_dispatch_cards_keeps_populated_done_card_unchanged() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-dispatch-card-populated-done",
        "x",
        "local-default",
        "local",
    )
    .unwrap();
    let report = "[Worker report]\nagent: Codex Worker\nassignment_id: assignment-1\nstatus: failed\nfinal_text:\nnew report";
    db::append_message(
        &conn,
        "s-dispatch-card-populated-done",
        "assistant",
        &[db::Block::Text {
            text: report.into(),
        }],
        Some("agent-team"),
        Some("worker-agent"),
        Some("Codex Worker"),
    )
    .unwrap();
    let mut lead_blocks = vec![running_dispatch_card_for_reconcile("assignment-1")];
    let db::Block::DispatchCard { member, .. } = &mut lead_blocks[0] else {
        unreachable!();
    };
    member.status = "done".into();
    member.blocks = vec![db::Block::Text {
        text: "existing report".into(),
    }];
    let expected = lead_blocks.clone();

    reconcile_running_dispatch_cards(&conn, "s-dispatch-card-populated-done", &mut lead_blocks);

    assert_eq!(lead_blocks, expected);
}

#[test]
fn reconcile_running_dispatch_cards_keeps_running_card_without_report() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-dispatch-card-no-report",
        "x",
        "local-default",
        "local",
    )
    .unwrap();
    let mut lead_blocks = vec![running_dispatch_card_for_reconcile("assignment-1")];
    let expected = lead_blocks.clone();

    reconcile_running_dispatch_cards(&conn, "s-dispatch-card-no-report", &mut lead_blocks);
    db::append_message(
        &conn,
        "s-dispatch-card-no-report",
        "assistant",
        &lead_blocks,
        Some("agent-team"),
        Some("lead-agent"),
        Some("Lead"),
    )
    .unwrap();

    assert_eq!(
        db::get_messages(&conn, "s-dispatch-card-no-report").unwrap()[0].content,
        expected
    );
}

#[test]
fn generation_commands_require_repo_and_agent_ids() {
    assert!(validate_generation_ids("repo-1", "agent-1").is_ok());
    assert_eq!(
        validate_generation_ids(" ", "agent-1").unwrap_err(),
        "repo_id and agent_id are required"
    );
    assert_eq!(
        validate_generation_ids("repo-1", "").unwrap_err(),
        "repo_id and agent_id are required"
    );
}
