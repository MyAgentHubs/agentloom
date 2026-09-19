#![cfg(test)]

use super::*;

// (1) 纯函数：终态映射（codex P1-6）
#[test]
fn terminal_status_maps_stop_error_exit() {
    use crate::agent_event::StatusTransition::*;
    // (saw_error, saw_completed, exit_success, stopped)
    assert_eq!(terminal_status(true, true, true, true), Stopped); // 停优先
    assert_eq!(terminal_status(true, false, true, false), Failed); // Error 且无 Completed
    assert_eq!(terminal_status(true, true, false, false), Failed); // Completed 也不覆盖非 0 退出
    assert_eq!(terminal_status(true, true, true, false), Done); // Completed + exit 0 覆盖中途 Error
    assert_eq!(terminal_status(false, false, false, false), Failed); // 退出码非 0（即使没 Error 事件）
    assert_eq!(terminal_status(false, false, true, false), Done); // 无 Error 的既有干净退出
}

#[test]
fn detect_blocking_write_failure_matches_specific_markers() {
    assert_eq!(
        detect_blocking_write_failure("Could not edit file: Operation not permitted."),
        Some("operation not permitted".into())
    );
    assert_eq!(
        detect_blocking_write_failure("mkdir failed: PERMISSION DENIED"),
        Some("permission denied".into())
    );
    assert_eq!(
        detect_blocking_write_failure("write failed: read-only file system"),
        Some("read-only file system".into())
    );
    assert_eq!(
        detect_blocking_write_failure("apply_patch rejected the patch"),
        Some("apply_patch rejected".into())
    );
    assert_eq!(
        detect_blocking_write_failure("apply_patch failed before modifying files"),
        Some("apply_patch failed".into())
    );

    assert_eq!(detect_blocking_write_failure("done; wrote all files"), None);
    assert_eq!(
        detect_blocking_write_failure("there was an error in an unrelated summary"),
        None
    );
    assert_eq!(
        detect_blocking_write_failure("apply_patch completed successfully"),
        None
    );
}

#[test]
fn git_wall_detects_blocked_write() {
    let events = vec![
        AgentEvent::ToolStarted {
            id: "git-write".into(),
            tool: "Bash".into(),
            summary: "git revert HEAD".into(),
            card: CardKind::Command,
        },
        AgentEvent::ToolCompleted {
            id: "git-write".into(),
            status: ToolStatus::Failed,
            exit_code: Some(128),
            output: Some("fatal: unable to write: Operation not permitted".into()),
        },
    ];

    assert_eq!(
        detect_git_wall_block(&events),
        Some("git revert HEAD".into())
    );
}

#[test]
fn git_wall_ignores_normal_failures() {
    let cases = [
        ("npm test", ToolStatus::Failed, "operation not permitted"),
        ("git status", ToolStatus::Failed, "not a repo"),
        ("git status", ToolStatus::Ok, "operation not permitted"),
        ("grep needle haystack", ToolStatus::Failed, "no matches"),
    ];

    for (summary, status, output) in cases {
        let events = vec![
            AgentEvent::ToolStarted {
                id: "command".into(),
                tool: "Bash".into(),
                summary: summary.into(),
                card: CardKind::Command,
            },
            AgentEvent::ToolCompleted {
                id: "command".into(),
                status,
                exit_code: Some(1),
                output: Some(output.into()),
            },
        ];

        assert_eq!(detect_git_wall_block(&events), None, "summary: {summary}");
    }
}

#[test]
fn build_member_result_carries_hard_fields() {
    let changed_files = vec![ChangedFile {
        path: "src/lib.rs".into(),
        insertions: 3,
        deletions: 1,
    }];
    let anchor = ResultAnchor {
        base_sha: "base123".into(),
        head_sha: None,
        diff_ref: None,
        generated_from: "worktree_diff".into(),
    };
    let command_evidence = vec![CommandEvidence {
        cmd: "sed -i '' s/a/b/ src/lib.rs".into(),
        exit_code: Some(0),
        status: "ok".into(),
        source_provider: "codex".into(),
        output_ref: None,
    }];

    let result = build_member_result(
        &spec(),
        StatusTransition::Done,
        changed_files,
        anchor,
        command_evidence,
        None,
    );

    assert_eq!(result.schema_version, 1);
    assert_eq!(result.assignment_id, "run1-a1");
    assert_eq!(result.participant_id, "worker-1");
    assert_eq!(result.status, "done");
    assert_eq!(result.changed_files.len(), 1);
    assert_eq!(result.changed_files[0].path, "src/lib.rs");
    assert_eq!(result.changed_files[0].insertions, 3);
    assert_eq!(result.changed_files[0].deletions, 1);
    assert_eq!(result.anchor.base_sha, "base123");
    assert_eq!(result.anchor.generated_from, "worktree_diff");
    assert_eq!(result.command_evidence.len(), 1);
    assert_eq!(
        result.command_evidence[0].cmd,
        "sed -i '' s/a/b/ src/lib.rs"
    );
    assert_eq!(result.command_evidence[0].exit_code, Some(0));
    assert_eq!(result.command_evidence[0].source_provider, "codex");
    assert_eq!(result.risk_inputs.files_changed, 1);
    assert_eq!(result.risk_inputs.cmd_danger, "med");
    assert_eq!(result.risk_inputs.reversibility, "reversible");
    assert!(result.decisions.is_empty());
    assert!(result.risks.is_empty());
    assert_eq!(result.final_text_ref, None);
    assert!(result.artifact_refs.is_empty());
    assert_eq!(result.result_source, "raw");
}

#[test]
fn build_member_result_carries_final_text_ref() {
    let anchor = ResultAnchor {
        base_sha: "base123".into(),
        head_sha: None,
        diff_ref: None,
        generated_from: "worktree_diff".into(),
    };

    let with_final_text = build_member_result(
        &spec(),
        StatusTransition::Done,
        vec![],
        anchor.clone(),
        vec![],
        Some("答案正文"),
    );
    assert_eq!(with_final_text.final_text_ref, Some("答案正文".to_string()));

    let without_final_text = build_member_result(
        &spec(),
        StatusTransition::Done,
        vec![],
        anchor,
        vec![],
        None,
    );
    assert_eq!(without_final_text.final_text_ref, None);
}

#[test]
fn derive_command_evidence_handles_both_providers() {
    let codex_events = vec![
        AgentEvent::ToolStarted {
            id: "1".into(),
            tool: "shell".into(),
            summary: "cargo test".into(),
            card: CardKind::Command,
        },
        AgentEvent::ToolCompleted {
            id: "1".into(),
            status: ToolStatus::Ok,
            exit_code: Some(0),
            output: None,
        },
    ];
    let codex = derive_command_evidence(&codex_events, "codex");
    assert_eq!(codex.len(), 1);
    assert_eq!(codex[0].cmd, "cargo test");
    assert_eq!(codex[0].exit_code, Some(0));
    assert_eq!(codex[0].status, "ok");
    assert_eq!(codex[0].source_provider, "codex");
    assert_eq!(codex[0].output_ref, None);

    let claude_events = vec![
        AgentEvent::ToolStarted {
            id: "2".into(),
            tool: "Bash".into(),
            summary: "npm test".into(),
            card: CardKind::Command,
        },
        AgentEvent::ToolCompleted {
            id: "2".into(),
            status: ToolStatus::Failed,
            exit_code: None,
            output: None,
        },
    ];
    let claude = derive_command_evidence(&claude_events, "claude");
    assert_eq!(claude.len(), 1);
    assert_eq!(claude[0].cmd, "npm test");
    assert_eq!(claude[0].exit_code, None);
    assert_eq!(claude[0].status, "failed");
    assert_eq!(claude[0].source_provider, "claude");
}

#[test]
fn derive_command_evidence_pairs_out_of_order_events_by_id() {
    let events = vec![
        AgentEvent::ToolCompleted {
            id: "b".into(),
            status: ToolStatus::Ok,
            exit_code: Some(0),
            output: None,
        },
        AgentEvent::ToolStarted {
            id: "a".into(),
            tool: "Bash".into(),
            summary: "cargo test".into(),
            card: CardKind::Command,
        },
        AgentEvent::ToolStarted {
            id: "b".into(),
            tool: "Bash".into(),
            summary: "npm test".into(),
            card: CardKind::Command,
        },
        AgentEvent::ToolCompleted {
            id: "a".into(),
            status: ToolStatus::Failed,
            exit_code: Some(101),
            output: None,
        },
    ];

    let evidence = derive_command_evidence(&events, "claude");

    assert_eq!(evidence.len(), 2);
    assert_eq!(evidence[0].cmd, "cargo test");
    assert_eq!(evidence[0].status, "failed");
    assert_eq!(evidence[0].exit_code, Some(101));
    assert_eq!(evidence[1].cmd, "npm test");
    assert_eq!(evidence[1].status, "ok");
    assert_eq!(evidence[1].exit_code, Some(0));
}

#[test]
fn derive_command_evidence_discards_unpaired_tool_events() {
    let completed_only = vec![AgentEvent::ToolCompleted {
        id: "done-only".into(),
        status: ToolStatus::Ok,
        exit_code: Some(0),
        output: None,
    }];
    let started_only = vec![AgentEvent::ToolStarted {
        id: "start-only".into(),
        tool: "Bash".into(),
        summary: "cargo test".into(),
        card: CardKind::Command,
    }];

    assert!(derive_command_evidence(&completed_only, "codex").is_empty());
    assert!(derive_command_evidence(&started_only, "codex").is_empty());
}

#[test]
fn derive_command_evidence_keeps_first_started_for_duplicate_id() {
    let events = vec![
        AgentEvent::ToolStarted {
            id: "dup".into(),
            tool: "Bash".into(),
            summary: "cargo test".into(),
            card: CardKind::Command,
        },
        AgentEvent::ToolStarted {
            id: "dup".into(),
            tool: "Write".into(),
            summary: "src/lib.rs".into(),
            card: CardKind::Compact,
        },
        AgentEvent::ToolCompleted {
            id: "dup".into(),
            status: ToolStatus::Ok,
            exit_code: Some(0),
            output: None,
        },
    ];

    let evidence = derive_command_evidence(&events, "claude");

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].cmd, "cargo test");
}

#[test]
fn derive_risk_inputs_thresholds() {
    let many: Vec<ChangedFile> = (0..12)
        .map(|i| ChangedFile {
            path: format!("f{i}.rs"),
            insertions: 1,
            deletions: 0,
        })
        .collect();

    let low = derive_risk_inputs(&many, &[]);
    assert_eq!(low.files_changed, 12);
    assert_eq!(low.cmd_danger, "low");
    assert_eq!(low.reversibility, "reversible");

    let write_cmd = vec![crate::agent_event::CommandEvidence {
        cmd: "echo updated > src/lib.rs".into(),
        exit_code: Some(0),
        status: "ok".into(),
        source_provider: "codex".into(),
        output_ref: None,
    }];
    let med = derive_risk_inputs(&many, &write_cmd);
    assert_eq!(med.files_changed, 12);
    assert_eq!(med.cmd_danger, "med");
    assert_eq!(med.reversibility, "reversible");
}

#[test]
fn derive_risk_inputs_marks_claude_write_tool_as_med() {
    let events = vec![
        AgentEvent::ToolStarted {
            id: "write-1".into(),
            tool: "Write".into(),
            summary: "src/lib.rs".into(),
            card: CardKind::Compact,
        },
        AgentEvent::ToolCompleted {
            id: "write-1".into(),
            status: ToolStatus::Ok,
            exit_code: Some(0),
            output: None,
        },
    ];

    let command_evidence = derive_command_evidence(&events, "claude");
    let risk_inputs = derive_risk_inputs(&[], &command_evidence);

    assert_eq!(command_evidence[0].cmd, "Write src/lib.rs");
    assert_eq!(risk_inputs.cmd_danger, "med");
}

#[test]
fn command_is_write_like_detects_tokenized_write_commands() {
    assert!(command_is_write_like("prettier --write src/"));
    assert!(command_is_write_like("cargo fmt"));
    assert!(command_is_write_like("rustfmt src/lib.rs"));
    assert!(command_is_write_like("git apply /tmp/fix.patch"));
    assert!(command_is_write_like("mkdir tmp-output"));
    assert!(command_is_write_like("sed -i s/a/b/ src/lib.rs"));
    assert!(command_is_write_like("echo updated > src/lib.rs"));
    assert!(command_is_write_like("echo updated | tee src/lib.rs"));
}

#[test]
fn command_is_write_like_avoids_write_text_and_dev_null_false_positives() {
    assert!(!command_is_write_like("grep write_text src/"));
    assert!(!command_is_write_like("rg writeFile src/"));
    assert!(!command_is_write_like("cat a.txt > /dev/null"));
    assert!(!command_is_write_like("echo updated | tee /dev/null"));
    assert!(!command_is_write_like("cargo test"));
}
