#![cfg(test)]

use super::*;

#[test]
fn dispatch_card_converges_to_same_terminal_content_in_both_persistence_orders() {
    let lead_first = crate::test_support::mem_db();
    let report_first = crate::test_support::mem_db();
    for (conn, session_id) in [
        (&lead_first, "dispatch-card-lead-first"),
        (&report_first, "dispatch-card-report-first"),
    ] {
        crate::db::create_session(conn, session_id, "x", "local-default", "local").unwrap();
    }
    let result = ledger_result("done", "worker final answer");

    crate::db::append_message(
        &lead_first,
        "dispatch-card-lead-first",
        "assistant",
        &[running_dispatch_card_for_terminal_ordering()],
        Some("agent-team"),
        Some("lead-agent"),
        Some("Lead"),
    )
    .unwrap();
    persist_member_result_message(
        &lead_first,
        "dispatch-card-lead-first",
        "worker-run-1",
        "worker-agent",
        "Worker",
        &result,
    )
    .unwrap();

    persist_member_result_message(
        &report_first,
        "dispatch-card-report-first",
        "worker-run-1",
        "worker-agent",
        "Worker",
        &result,
    )
    .unwrap();
    let mut report_first_lead_blocks = vec![running_dispatch_card_for_terminal_ordering()];
    crate::reconcile_running_dispatch_cards(
        &report_first,
        "dispatch-card-report-first",
        &mut report_first_lead_blocks,
    );
    crate::db::append_message(
        &report_first,
        "dispatch-card-report-first",
        "assistant",
        &report_first_lead_blocks,
        Some("agent-team"),
        Some("lead-agent"),
        Some("Lead"),
    )
    .unwrap();

    let lead_first_member = terminal_dispatch_card_member(&lead_first, "dispatch-card-lead-first");
    let report_first_member =
        terminal_dispatch_card_member(&report_first, "dispatch-card-report-first");
    assert_eq!(lead_first_member, report_first_member);
    assert_eq!(lead_first_member.status, "done");
    assert!(!lead_first_member.failed);
    let crate::db::Block::Text { text: report } = &lead_first_member.blocks[0] else {
        panic!("terminal dispatch card must contain worker report text");
    };
    assert!(report.starts_with("[Worker report]\n"));
    assert!(report.contains("assignment_id: dispatch-worker-lead-0\n"));
    assert!(report.contains("status: done\n"));
    assert!(report.contains("worker final answer"));
}

#[test]
fn git_wall_note_rendered_in_report() {
    let mut result = ledger_result("done", "worker final answer");
    result.risks.push(Risk {
        id: GIT_WALL_BLOCKED_RISK_ID.into(),
        text: "agent 试图 git 写（git revert HEAD）但被沙箱挡下".into(),
        source_refs: vec![],
        confidence: None,
        source_kind: Some("member_runner".into()),
    });

    let report = render_member_result_report("Worker", &result);
    assert!(report.contains("⚠"));
    assert!(report.contains("git revert HEAD"));

    let report_without_risk =
        render_member_result_report("Worker", &ledger_result("done", "worker final answer"));
    assert!(!report_without_risk.contains("⚠"));
}

#[test]
fn member_result_ledger_persists_success_failure_and_stopped_terminal_reports() {
    for (session_id, status) in [
        ("success-session", "done"),
        ("failure-session", "failed"),
        ("stopped-session", "stopped"),
    ] {
        let conn = crate::test_support::mem_db();
        let result = ledger_result(status, "worker final answer");
        let inserted = persist_member_result_message(
            &conn,
            session_id,
            "worker-run-1",
            "worker-agent",
            "Claude Worker",
            &result,
        )
        .unwrap();

        assert!(inserted);
        let messages = crate::db::get_messages(&conn, session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[0].engine.as_deref(), Some("agent-team"));
        assert_eq!(messages[0].agent_id.as_deref(), Some("worker-agent"));
        assert_eq!(
            messages[0].agent_name_snapshot.as_deref(),
            Some("Claude Worker")
        );
        let crate::db::Block::Text { text } = &messages[0].content[0] else {
            panic!("worker report must use a normal text block");
        };
        assert!(text.contains("[Worker report]"));
        assert!(text.contains("agent: Claude Worker"));
        assert!(text.contains(&format!("status: {status}")));
        assert!(text.contains("worker final answer"));
        assert!(text.contains("- src/lib.rs (+3/-1)"));
        if status == "failed" {
            assert!(text.contains("failure_reason: worker exited with code 1"));
        }
    }
}

#[test]
fn member_report_delivery_normal_result_branch_creates_pending_row() {
    let conn = crate::test_support::mem_db();
    let mut branch_spec = spec();
    branch_spec.assignment_id = "assignment-normal".into();
    let mut result = ledger_result("done", "normal result");
    result.assignment_id = branch_spec.assignment_id.clone();

    run_single_worker_lifecycle(
        &TeamRunning::default(),
        "member-report-delivery-normal",
        "run-normal",
        &branch_spec,
        || Ok(()),
        |()| Ok(result),
        |result| {
            persist_member_result_message(
                &conn,
                "member-report-delivery-normal",
                "run-normal",
                &branch_spec.agent_id,
                &branch_spec.agent_name,
                result,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        },
        |_: &str| Ok(()),
        |_: &str| Ok(()),
        || Ok(()),
    )
    .unwrap();

    assert_member_report_delivery_pending(
        &conn,
        "member-report-delivery-normal",
        &branch_spec.assignment_id,
    );
}

#[test]
fn member_report_delivery_lifecycle_failure_branch_creates_pending_row() {
    let conn = crate::test_support::mem_db();
    let mut branch_spec = spec();
    branch_spec.assignment_id = "assignment-lifecycle-failure".into();

    run_single_worker_lifecycle(
        &TeamRunning::default(),
        "member-report-delivery-lifecycle-failure",
        "run-lifecycle-failure",
        &branch_spec,
        || Ok(()),
        |()| Err("worker lifecycle failure".to_string()),
        |_: &MemberResult| Ok(()),
        |_: &str| Ok(()),
        |reason| {
            persist_member_failure_message(
                &conn,
                "member-report-delivery-lifecycle-failure",
                "run-lifecycle-failure",
                &branch_spec,
                reason,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        },
        || Ok(()),
    )
    .unwrap_err();

    assert_member_report_delivery_pending(
        &conn,
        "member-report-delivery-lifecycle-failure",
        &branch_spec.assignment_id,
    );
}

#[test]
fn member_report_delivery_setup_failure_branch_creates_pending_row() {
    let conn = crate::test_support::mem_db();
    let mut branch_spec = spec();
    branch_spec.assignment_id = "assignment-setup-failure".into();

    run_single_worker_lifecycle(
        &TeamRunning::default(),
        "member-report-delivery-setup-failure",
        "run-setup-failure",
        &branch_spec,
        || Err::<(), _>("setup failure".to_string()),
        |()| unreachable!("setup failure must not run the worker"),
        |_: &MemberResult| Ok(()),
        |reason| {
            persist_member_setup_failure_message(
                &conn,
                "member-report-delivery-setup-failure",
                "run-setup-failure",
                &branch_spec,
                reason,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        },
        |_: &str| Ok(()),
        || Ok(()),
    )
    .unwrap_err();

    assert_member_report_delivery_pending(
        &conn,
        "member-report-delivery-setup-failure",
        &branch_spec.assignment_id,
    );
}

#[test]
fn member_report_delivery_pre_setup_prepare_failure_branch_creates_pending_row() {
    let conn = crate::test_support::mem_db();
    let mut branch_spec = spec();
    branch_spec.assignment_id = "assignment-pre-setup-prepare".into();

    let error = finish_single_worker_setup_failure(
        "member-report-delivery-pre-setup-prepare",
        "run-pre-setup-prepare",
        &branch_spec,
        "prepare_single_worker failure".to_string(),
        |reason| {
            persist_member_failure_message(
                &conn,
                "member-report-delivery-pre-setup-prepare",
                "run-pre-setup-prepare",
                &branch_spec,
                reason,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        },
        || Ok(()),
    );
    assert_eq!(error, "prepare_single_worker failure");
    assert_member_report_delivery_pending(
        &conn,
        "member-report-delivery-pre-setup-prepare",
        &branch_spec.assignment_id,
    );
}

#[test]
fn member_report_delivery_pre_setup_snapshot_failure_branch_creates_pending_row() {
    let conn = crate::test_support::mem_db();
    let mut branch_spec = spec();
    branch_spec.assignment_id = "assignment-pre-setup-snapshot".into();

    let error = finish_single_worker_setup_failure(
        "member-report-delivery-pre-setup-snapshot",
        "run-pre-setup-snapshot",
        &branch_spec,
        "stage1 snapshot failure".to_string(),
        |reason| {
            persist_member_failure_message(
                &conn,
                "member-report-delivery-pre-setup-snapshot",
                "run-pre-setup-snapshot",
                &branch_spec,
                reason,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        },
        || Ok(()),
    );
    assert_eq!(error, "stage1 snapshot failure");
    assert_member_report_delivery_pending(
        &conn,
        "member-report-delivery-pre-setup-snapshot",
        &branch_spec.assignment_id,
    );
}

#[test]
fn member_result_ledger_deduplicates_same_dispatch() {
    let conn = crate::test_support::mem_db();
    let result = ledger_result("done", "once");

    assert!(persist_member_result_message(
        &conn,
        "s1",
        "worker-run-1",
        "worker-agent",
        "Worker",
        &result,
    )
    .unwrap());
    assert!(!persist_member_result_message(
        &conn,
        "s1",
        "worker-run-1",
        "worker-agent",
        "Worker",
        &result,
    )
    .unwrap());
    assert!(persist_member_result_message(
        &conn,
        "s1",
        "worker-run-2",
        "worker-agent",
        "Worker",
        &result,
    )
    .unwrap());

    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages WHERE session_id = 's1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2, "dedup key 必须包含 run_id");
}

#[test]
fn member_result_ledger_truncates_final_text_on_char_boundary() {
    let original = "界".repeat(MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS + 7);
    let report = render_member_result_report("Worker", &ledger_result("done", &original));

    assert!(report.contains(&"界".repeat(MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS)));
    assert!(report.contains(&format!(
        "[truncated: kept {} of {} characters]",
        MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS,
        MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS + 7
    )));
    assert!(!report.contains(&"界".repeat(MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS + 1)));
}

#[test]
fn member_result_ledger_budgets_failure_files_and_final_text_as_one_report() {
    let mut result = ledger_result("failed", &format!("正文必须保留。{}", "界".repeat(3_000)));
    result.failure_reason = Some("失败原因".repeat(200));
    result.changed_files = (0..100)
        .map(|_| ChangedFile {
            path: String::new(),
            insertions: 0,
            deletions: 0,
        })
        .collect();

    let report = render_member_result_report("Worker", &result);

    assert!(
        report.chars().count() <= MEMBER_RESULT_LEDGER_REPORT_MAX_CHARS,
        "report length was {}",
        report.chars().count()
    );
    let failure_line = report
        .lines()
        .find(|line| line.starts_with("failure_reason: "))
        .unwrap();
    assert!(
        failure_line
            .trim_start_matches("failure_reason: ")
            .chars()
            .count()
            <= MEMBER_RESULT_LEDGER_FAILURE_REASON_MAX_CHARS
    );
    assert_eq!(
        report.lines().filter(|line| *line == "-  (+0/-0)").count(),
        MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX
    );
    assert!(report.contains("- (+50 more)"));
    assert!(report.contains("正文必须保留。"));
    assert!(report.contains("[truncated: kept "));
    assert!(report.ends_with(" characters]"));
}

#[test]
fn single_worker_success_tolerates_read_only_ledger_and_still_finalizes() {
    let conn = crate::test_support::mem_db();
    conn.execute_batch("PRAGMA query_only = ON").unwrap();
    let tr = TeamRunning::default();
    let spec = spec();
    let persist_calls = std::cell::Cell::new(0);
    let finalize_calls = std::cell::Cell::new(0);

    let result = run_single_worker_lifecycle(
        &tr,
        "s1",
        "run-read-only",
        &spec,
        || Ok(()),
        |()| Ok(ledger_result("done", "main flow result")),
        |result| {
            persist_calls.set(persist_calls.get() + 1);
            persist_member_result_message(
                &conn,
                "s1",
                "run-read-only",
                "worker-agent",
                "Worker",
                result,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        },
        |_: &str| Ok(()),
        |_: &str| Ok(()),
        || {
            finalize_calls.set(finalize_calls.get() + 1);
            Ok(())
        },
    )
    .expect("账本只读不应反伤主流程");

    assert_eq!(result.status, "done");
    assert_eq!(persist_calls.get(), 1);
    assert_eq!(finalize_calls.get(), 1);
    assert!(!tr.run_member_finished("run-read-only"));
}

#[test]
fn single_worker_inner_failure_calls_failure_ledger_and_finalize() {
    let tr = TeamRunning::default();
    let spec = spec();
    let failure_ledger_calls = std::cell::Cell::new(0);
    let finalize_calls = std::cell::Cell::new(0);

    let error = run_single_worker_lifecycle(
        &tr,
        "s1",
        "run-inner-failure",
        &spec,
        || Ok(()),
        |()| Err("worker spawn failed".to_string()),
        |_: &MemberResult| Ok(()),
        |_: &str| Ok(()),
        |reason| {
            assert_eq!(reason, "worker spawn failed");
            failure_ledger_calls.set(failure_ledger_calls.get() + 1);
            Ok(())
        },
        || {
            finalize_calls.set(finalize_calls.get() + 1);
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error, "worker spawn failed");
    assert_eq!(failure_ledger_calls.get(), 1);
    assert_eq!(finalize_calls.get(), 1);
    assert!(!tr.run_member_finished("run-inner-failure"));
}

#[test]
fn single_worker_transport_registration_failure_uses_independent_dedup_key() {
    let conn = crate::test_support::mem_db();
    let tr = TeamRunning::default();
    let spec = spec();
    let run_called = std::cell::Cell::new(false);
    let finalize_calls = std::cell::Cell::new(0);
    tr.init_run("transport-run", 2);

    let error = run_single_worker_lifecycle(
        &tr,
        "transport-session",
        "transport-run",
        &spec,
        || Err("EventTransport register_run failed: AlreadyRegistered".to_string()),
        |_: ()| {
            run_called.set(true);
            Ok(ledger_result("done", "must not run"))
        },
        |_: &MemberResult| Ok(()),
        |reason| {
            persist_member_setup_failure_message(
                &conn,
                "transport-session",
                "transport-run",
                &spec,
                reason,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        },
        |_: &str| Ok(()),
        || {
            finalize_calls.set(finalize_calls.get() + 1);
            Ok(())
        },
    )
    .unwrap_err();

    assert!(error.contains("AlreadyRegistered"));
    assert!(!run_called.get());
    assert_eq!(finalize_calls.get(), 0);
    assert!(!tr.run_member_finished("transport-run"));
    assert!(tr.run_member_finished("transport-run"));
    let messages = crate::db::get_messages(&conn, "transport-session").unwrap();
    assert_eq!(messages.len(), 1);
    let crate::db::Block::Text { text } = &messages[0].content[0] else {
        panic!("setup failure must persist a text report");
    };
    assert!(text.contains("status: failed"));
    assert!(text.contains("AlreadyRegistered"));
    let mut owner_result = ledger_result("done", "owner result");
    owner_result.assignment_id = spec.assignment_id.clone();
    assert!(persist_member_result_message(
        &conn,
        "transport-session",
        "transport-run",
        &spec.agent_id,
        &spec.agent_name,
        &owner_result,
    )
    .unwrap());
}
