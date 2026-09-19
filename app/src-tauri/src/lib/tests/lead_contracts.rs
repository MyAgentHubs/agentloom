#![cfg(test)]

use super::*;

#[test]
fn lead_mcp_surface_has_no_undo() {
    assert_eq!(
        LEAD_MCP_TOOL_NAMES,
        [
            "dispatch_worker",
            "finish",
            "memory_set",
            "memory_add",
            "memory_read_source",
            "ask_user",
            "propose_verifier",
            "commit",
            "push",
            "create_pr",
            "publish",
        ]
    );
    assert!(LEAD_MCP_TOOL_NAMES
        .iter()
        .all(|name| !name.contains("undo")));
}

#[test]
fn lead_mcp_tool_descriptions_have_no_cjk() {
    let descriptions = [
        (
            "dispatch_worker",
            lead_tools::dispatch_worker_description(&[]),
        ),
        ("finish", LEAD_FINISH_DESCRIPTION.to_string()),
        ("memory_set", LEAD_MEMORY_SET_DESCRIPTION.to_string()),
        ("memory_add", LEAD_MEMORY_ADD_DESCRIPTION.to_string()),
        (
            "memory_read_source",
            LEAD_MEMORY_READ_SOURCE_DESCRIPTION.to_string(),
        ),
        ("ask_user", LEAD_ASK_USER_DESCRIPTION.to_string()),
        (
            "propose_verifier",
            LEAD_PROPOSE_VERIFIER_DESCRIPTION.to_string(),
        ),
        ("commit", LEAD_COMMIT_DESCRIPTION.to_string()),
        ("push", LEAD_PUSH_DESCRIPTION.to_string()),
        ("create_pr", LEAD_CREATE_PR_DESCRIPTION.to_string()),
        ("publish", LEAD_PUBLISH_DESCRIPTION.to_string()),
    ];

    assert_eq!(
        descriptions
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
        LEAD_MCP_TOOL_NAMES
    );
    for (name, description) in descriptions {
        assert!(
            !description.trim().is_empty(),
            "{name} description is empty"
        );
        assert!(
            !description
                .chars()
                .any(|ch| ('\u{4E00}'..='\u{9FFF}').contains(&ch)),
            "{name} description contains CJK unified ideographs: {description}"
        );
    }
}

#[test]
fn lead_commit_preview_lists_every_selected_path() {
    let selection = commit_broker::CommittableSelection {
        exact_paths: vec![
            std::path::PathBuf::from("src/ready.rs"),
            std::path::PathBuf::from("src/pre-dirty.rs"),
        ],
        deleted_paths: Default::default(),
    };

    let preview = format_lead_commit_preview(&selection, Locale::Zh);
    assert!(preview.contains("将提交：\n- src/ready.rs"));
    assert!(preview.contains("\n- src/pre-dirty.rs"));
    assert!(!preview.contains("不提交"));
}

#[test]
fn lead_commit_preview_flags_deletions_distinctly_from_modifications() {
    let selection = commit_broker::CommittableSelection {
        exact_paths: vec![
            std::path::PathBuf::from("src/deleted.rs"),
            std::path::PathBuf::from("src/modified.rs"),
        ],
        deleted_paths: std::collections::HashSet::from([std::path::PathBuf::from(
            "src/deleted.rs",
        )]),
    };

    let preview = format_lead_commit_preview(&selection, Locale::Zh);
    assert!(preview.contains("\n- [删除] src/deleted.rs"), "{preview}");
    assert!(preview.contains("\n- src/modified.rs"), "{preview}");
    assert!(!preview.contains("[删除] src/modified.rs"), "{preview}");
}

#[test]
fn lead_commit_preview_sanitizes_control_characters_in_paths() {
    let selection = commit_broker::CommittableSelection {
        exact_paths: vec![std::path::PathBuf::from("src/ready\nforged.rs")],
        deleted_paths: Default::default(),
    };

    let preview = format_lead_commit_preview(&selection, Locale::Zh);

    assert_eq!(preview, "将提交：\n- src/ready?forged.rs");
}

#[test]
fn lead_commit_preview_localizes_chinese_and_english_including_deletions() {
    let selection = commit_broker::CommittableSelection {
        exact_paths: vec![std::path::PathBuf::from("src/deleted.rs")],
        deleted_paths: std::collections::HashSet::from([std::path::PathBuf::from(
            "src/deleted.rs",
        )]),
    };

    assert_eq!(
        format_lead_commit_preview(&selection, Locale::Zh),
        "将提交：\n- [删除] src/deleted.rs"
    );
    assert_eq!(
        format_lead_commit_preview(&selection, Locale::En),
        "Will commit:\n- [deleted] src/deleted.rs"
    );

    let empty_selection = commit_broker::CommittableSelection {
        exact_paths: Vec::new(),
        deleted_paths: Default::default(),
    };
    assert_eq!(
        format_lead_commit_preview(&empty_selection, Locale::Zh),
        "将提交：\n（无）"
    );
    assert_eq!(
        format_lead_commit_preview(&empty_selection, Locale::En),
        "Will commit:\n(none)"
    );
}

#[test]
fn lead_commit_confirmation_options_match_answer_validation_for_each_locale() {
    for (locale, expected_copy) in [
        (
            Locale::Zh,
            ("提交", "取消", "提交前请核对本次请求将提交的文件清单。"),
        ),
        (
            Locale::En,
            (
                "Commit",
                "Cancel",
                "Check the file list this request is about to commit.",
            ),
        ),
    ] {
        let args = lead_commit_confirmation_args("question".to_string(), locale);
        assert_eq!(args.question, "question");
        assert_eq!(
            args.options.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![expected_copy.0, expected_copy.1]
        );
        assert_eq!(args.recommended.as_deref(), Some(expected_copy.0));
        assert_eq!(args.rationale.as_deref(), Some(expected_copy.2));

        assert_eq!(
            lead_commit_confirmation_is_cancelled(
                &args.options[0],
                &args.options[0],
                &args.options[1],
            ),
            Ok(false)
        );
        assert_eq!(
            lead_commit_confirmation_is_cancelled(
                &args.options[1],
                &args.options[0],
                &args.options[1],
            ),
            Ok(true)
        );
        assert!(lead_commit_confirmation_is_cancelled(
            "not-an-option",
            &args.options[0],
            &args.options[1],
        )
        .is_err());
    }
}

#[test]
fn lead_commit_preview_is_required_only_before_repository_authorization() {
    assert!(lead_commit_requires_preview(false));
    assert!(!lead_commit_requires_preview(true));
}

#[test]
fn list_and_undo_reject_solo_and_team_sessions_while_running() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    running
        .0
        .lock()
        .unwrap()
        .insert("s1".into(), RunSlot::Running(42));

    assert!(
        list_run_undo_entries_checked(&conn, &running, &team_running, "s1", "r1")
            .unwrap_err()
            .starts_with("UNDO_RUN_ACTIVE:")
    );
    assert!(undo_run_edits_checked(
        &conn,
        &running,
        &team_running,
        "s1",
        "r1",
        Vec::new(),
        Vec::new(),
    )
    .unwrap_err()
    .starts_with("UNDO_RUN_ACTIVE:"));

    running.0.lock().unwrap().remove("s1");
    team_running.register(&member_runner::MemberKey::new("s1", "r1", "a1"), 43);
    assert!(
        list_run_undo_entries_checked(&conn, &running, &team_running, "s1", "r1")
            .unwrap_err()
            .starts_with("UNDO_RUN_ACTIVE:")
    );
    assert!(undo_run_edits_checked(
        &conn,
        &running,
        &team_running,
        "s1",
        "r1",
        Vec::new(),
        Vec::new(),
    )
    .unwrap_err()
    .starts_with("UNDO_RUN_ACTIVE:"));
}

#[test]
fn locale_whitelist_and_backend_messages_are_bilingual() {
    assert_eq!(Locale::parse("zh"), Some(Locale::Zh));
    assert_eq!(Locale::parse("en"), Some(Locale::En));
    assert_eq!(Locale::parse("zh-CN"), None);
    assert_eq!(local_repo_label(Locale::Zh), "本地");
    assert_eq!(local_repo_label(Locale::En), "Local");
    assert_eq!(
        session_continued_readonly_message(Locale::Zh),
        "会话已交接到新会话·只读·请到新会话继续"
    );
    assert_eq!(
        session_continued_readonly_message(Locale::En),
        "Session handed off to a new session · read-only · continue in the new session"
    );
    assert_eq!(
        handoff_truncation_warning(Locale::Zh),
        "已截断旧消息（仅取最近 40 条）"
    );
    assert_eq!(
        handoff_truncation_warning(Locale::En),
        "Older messages were truncated (only the latest 40 were included)"
    );
    assert_eq!(
        continuation_child_title(Locale::Zh, "父会话"),
        "接续: 父会话"
    );
    assert_eq!(
        continuation_child_title(Locale::En, "Parent session"),
        "Continuation: Parent session"
    );

    let mut truncation = "…[已截断 42 字节]\ntail".to_string();
    localize_truncation_marker(Locale::Zh, &mut truncation);
    assert_eq!(truncation, "…[已截断 42 字节]\ntail");
    localize_truncation_marker(Locale::En, &mut truncation);
    assert_eq!(truncation, "…[truncated 42 bytes]\ntail");

    assert_eq!(
        cli_exit_failure_message(Locale::Zh, "队长", None, ""),
        "队长 进程失败（退出状态未知），没有 stderr 输出。请检查 CLI 登录、额度、模型和网络。"
    );
    assert_eq!(
            cli_exit_failure_message(Locale::En, "lead", None, ""),
            "lead process failed (exit status unknown) with no stderr output. Check CLI login, quota, model, and network."
        );
    assert_eq!(
        cli_exit_failure_message(Locale::Zh, "队长", None, "quota exhausted"),
        "队长 进程失败（退出状态未知）：quota exhausted"
    );

    let exit_status = std::process::Command::new("sh")
        .args(["-c", "exit 3"])
        .status()
        .expect("sh should provide a real nonzero ExitStatus");
    assert!(!exit_status.success());
    assert_eq!(
        cli_exit_failure_message(Locale::Zh, "队长", Some(&exit_status), ""),
        "队长 进程失败（exit status: 3），没有 stderr 输出。请检查 CLI 登录、额度、模型和网络。"
    );
    assert_eq!(
            cli_exit_failure_message(Locale::En, "lead", Some(&exit_status), ""),
            "lead process failed (exit status: 3) with no stderr output. Check CLI login, quota, model, and network."
        );
    assert_eq!(
        cli_exit_failure_message(Locale::Zh, "队长", Some(&exit_status), "quota exhausted"),
        "队长 进程失败（exit status: 3）：quota exhausted"
    );
    assert_eq!(
        cli_exit_failure_message(Locale::En, "lead", Some(&exit_status), "quota exhausted"),
        "lead process failed (exit status: 3): quota exhausted"
    );

    assert_eq!(
        lead_runtime_failure_message(Locale::Zh, LeadRuntimeFailure::McpStart("boom")),
        "MCP 服务启动失败：boom"
    );
    assert_eq!(
        lead_runtime_failure_message(Locale::En, LeadRuntimeFailure::McpStart("boom")),
        "MCP server failed to start: boom"
    );
    assert_eq!(
        lead_runtime_failure_message(Locale::En, LeadRuntimeFailure::CommandBuild("boom")),
        "Failed to construct lead command: boom"
    );
    assert_eq!(
        lead_runtime_failure_message(
            Locale::En,
            LeadRuntimeFailure::CommandBuild(
                r#"AL_ERR:run.workspaceCanonicalizeFailed:{"detail":"boom"}"#
            )
        ),
        r#"AL_ERR:run.workspaceCanonicalizeFailed:{"detail":"boom"}"#
    );
    assert_eq!(
        lead_runtime_failure_message(Locale::En, LeadRuntimeFailure::ProcessStart("boom")),
        "Lead failed to start: boom"
    );
}

/// P1-2（opus 对抗审）：`member_stall_failure_message` 之前只在 zh 侧被间接测过
/// （member_runner.rs 的 run_member_reader 系列测试固定走 zh 包装）——en 分支零覆盖。
/// 直接钉住 zh/en 两侧、blocked/needs_decision 两个分支的精确文案，并且都得含
/// 「不是环境故障 / not an environment failure」这句锚点（历史上 memberFailure.ts 曾靠
/// 这句字面匹配分类——现已改结构化 failure_kind 判据，但锚点句子本身仍是给人看的诚实
/// 措辞，得留着，也得测双语）。
#[test]
fn member_stall_failure_message_is_bilingual_and_distinguishes_reason() {
    let exit_status = std::process::Command::new("sh")
        .args(["-c", "exit 3"])
        .status()
        .expect("sh should provide a real nonzero ExitStatus");
    assert!(!exit_status.success());

    // saw_needs_decision 优先于 saw_blocked（两者都真时，需要决策的措辞更具体）。
    assert_eq!(
            member_stall_failure_message(Locale::Zh, false, true, Some(&exit_status)),
            Some(
                "工人停在需要决策（exit status: 3）。这不是环境故障——看它最后的输出，回答它的问题或调整任务范围。"
                    .to_string()
            )
        );
    assert_eq!(
            member_stall_failure_message(Locale::En, false, true, Some(&exit_status)),
            Some(
                "Worker stopped needing a decision (exit status: 3). This is not an environment failure — see its last output, answer its question or adjust the task scope."
                    .to_string()
            )
        );
    assert_eq!(
            member_stall_failure_message(Locale::Zh, true, false, Some(&exit_status)),
            Some(
                "工人停摆：有问题在等回答，或执行被阻塞（exit status: 3）。这不是环境故障——看它最后的输出。"
                    .to_string()
            )
        );
    assert_eq!(
            member_stall_failure_message(Locale::En, true, false, Some(&exit_status)),
            Some(
                "Worker stalled: it has a question pending or execution got blocked (exit status: 3). This is not an environment failure — see its last output."
                    .to_string()
            )
        );
    // 都为假 → None（调用方只在其一为真时才调这个函数）。
    assert_eq!(
        member_stall_failure_message(Locale::Zh, false, false, Some(&exit_status)),
        None
    );
    assert_eq!(
        member_stall_failure_message(Locale::En, false, false, None),
        None
    );

    // 双语都得含诚实锚点短语（memberFailure.ts 的 spoof-resistance 讨论里提过的那句）。
    for locale in [Locale::Zh, Locale::En] {
        let needle = match locale {
            Locale::Zh => "不是环境故障",
            Locale::En => "not an environment failure",
        };
        let blocked = member_stall_failure_message(locale, true, false, Some(&exit_status))
            .expect("blocked message");
        let needs_decision = member_stall_failure_message(locale, false, true, Some(&exit_status))
            .expect("needs_decision message");
        assert!(blocked.contains(needle), "{blocked}");
        assert!(needs_decision.contains(needle), "{needs_decision}");
        // 也不该出现 cli_exit_failure_message 那句「检查 CLI 登录」——两条文案互斥。
        assert!(!blocked.contains("CLI"), "{blocked}");
        assert!(!needs_decision.contains("CLI"), "{needs_decision}");
    }
}
