    use super::*;
    use crate::events::EventRecorder;
    use crate::provider::{FunctionCall, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "shell_call".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "shell_exec".into(),
                arguments: args.to_string(),
            },
        }
    }

    #[test]
    fn cap_for_wire_leaves_small_output_untouched() {
        let input = "short output\n";
        let (out, truncated) = cap_for_wire(input, WIRE_OUTPUT_CAP_BYTES);

        assert_eq!(out.as_bytes(), input.as_bytes());
        assert!(!truncated);
    }

    #[test]
    fn cap_for_wire_keeps_head_and_tail_and_marks_middle() {
        let input = "A".repeat(8_000) + &"B".repeat(8_000) + &"C".repeat(8_000);
        let (out, truncated) = cap_for_wire(&input, WIRE_OUTPUT_CAP_BYTES);

        assert!(out.len() <= WIRE_OUTPUT_CAP_BYTES);
        assert!(truncated);
        assert!(out.starts_with('A'));
        assert!(out.ends_with('C'));
        assert!(out.contains("bytes elided from the middle"));
    }

    #[test]
    fn cap_for_wire_is_utf8_safe() {
        let input = "中文测试abc".repeat(3_000);

        for max_bytes in [WIRE_OUTPUT_CAP_BYTES, 512] {
            let (out, truncated) = cap_for_wire(&input, max_bytes);
            assert!(truncated);
            assert!(out.len() <= max_bytes);
            assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        }
    }

    #[test]
    fn cap_for_wire_is_deterministic() {
        let input = "deterministic-output-中文".repeat(2_000);

        assert_eq!(
            cap_for_wire(&input, WIRE_OUTPUT_CAP_BYTES),
            cap_for_wire(&input, WIRE_OUTPUT_CAP_BYTES)
        );
    }

    #[test]
    fn cap_for_wire_边界() {
        let input = "中文测试abc".repeat(100);
        let (out, truncated) = cap_for_wire(&input, 32);

        assert!(truncated);
        assert!(out.len() <= 32);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn shell_exec_emits_full_stdout_but_caps_wire_output() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({
                    "command": "awk 'BEGIN { for (i = 0; i < 20000; i++) printf \"A\" }'"
                })),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::Success);
        let wire: Value = serde_json::from_str(&out.content).unwrap();
        assert!(wire["stdout"].as_str().unwrap().len() <= WIRE_OUTPUT_CAP_BYTES);
        assert_eq!(wire["wire_truncated"], true);

        let events: Vec<Value> = std::fs::read_to_string(&journal)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let stdout_event = events
            .iter()
            .find(|event| event["type"] == "tool.stdout.delta")
            .expect("tool.stdout.delta event");
        assert!(stdout_event["payload"]["text"].as_str().unwrap().len() > WIRE_OUTPUT_CAP_BYTES);
    }

    #[tokio::test]
    async fn shell_exec_caps_stderr_into_the_wire_too() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({
                    "command": "awk 'BEGIN { for (i = 0; i < 20000; i++) printf \"E\" }' >&2"
                })),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::Success);
        let wire: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(wire["stdout"], "");
        let wire_stderr = wire["stderr"].as_str().unwrap();
        assert!(wire_stderr.len() <= WIRE_OUTPUT_CAP_BYTES);
        assert!(wire_stderr.contains("bytes elided from the middle"));
        assert_eq!(wire["wire_truncated"], true);

        let events: Vec<Value> = std::fs::read_to_string(&journal)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let stderr_event = events
            .iter()
            .find(|event| event["type"] == "tool.stderr.delta")
            .expect("tool.stderr.delta event");
        assert!(stderr_event["payload"]["text"].as_str().unwrap().len() > WIRE_OUTPUT_CAP_BYTES);
    }

    #[test]
    fn shell_tool_description_explains_posix_dialect() {
        let definition =
            shell_tool_definition_for_dialect(crate::exec::controlled::ShellDialect::Posix);
        let description = definition["function"]["description"].as_str().unwrap();
        assert!(description.contains("commands run via a POSIX shell (sh); use POSIX syntax"));
    }

    #[test]
    fn shell_tool_description_explains_cmd_dialect() {
        let definition =
            shell_tool_definition_for_dialect(crate::exec::controlled::ShellDialect::Cmd);
        let description = definition["function"]["description"].as_str().unwrap();
        assert!(description.contains(
            "commands run via Windows cmd.exe; use Windows command syntax (dir, del, && is supported)"
        ));
        assert!(description.contains(
            "cmd variable expansion (%VAR%, %X, %%X, !VAR!) is rejected by the safety scanner"
        ));
        assert!(description.contains("do NOT use POSIX-only constructs (~, $VAR, single quotes)"));
    }

    fn context<'a>(
        workspace: &'a std::path::Path,
        recorder: &'a mut EventRecorder,
        file_ledger: &'a mut crate::file_ledger::FileLedger,
    ) -> ToolContext<'a> {
        ToolContext {
            workspace,
            recorder,
            file_ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        }
    }

    #[tokio::test]
    async fn explicit_missing_cwd_is_recoverable_and_not_spawned() {
        let workspace = tempfile::tempdir().unwrap();
        let missing_cwd = "missing-cwd";
        let spawned = workspace.path().join("spawned");
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({"command": "touch spawned", "cwd": missing_cwd})),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains(missing_cwd));
        assert!(!spawned.exists());
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn explicit_outside_cwd_is_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let spawned = outside.join("spawned");
        let outside_arg = outside.to_string_lossy().into_owned();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(&workspace, &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({"command": "touch spawned", "cwd": outside_arg})),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains(&outside_arg));
        assert!(!spawned.exists());
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn default_cwd_resolution_failure_stays_fatal() {
        let root = tempfile::tempdir().unwrap();
        let missing_workspace = root.path().join("missing-workspace");
        let journal = root.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(&missing_workspace, &mut recorder, &mut ledger);

        let err = ShellExecTool
            .execute(&mut ctx, &call(json!({"command": "true"})))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            HarnessError::Io(ref source) if source.kind() == std::io::ErrorKind::NotFound
        ));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn explicit_cwd_with_missing_workspace_stays_fatal() {
        let root = tempfile::tempdir().unwrap();
        let missing_workspace = root.path().join("missing-workspace");
        let spawned = root.path().join("spawned");
        let journal = root.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(&missing_workspace, &mut recorder, &mut ledger);

        let err = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({"command": "touch ../spawned", "cwd": "."})),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            HarnessError::Io(ref source) if source.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(!spawned.exists());
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(!events.contains("\"type\":\"tool.started\""));
    }

    #[tokio::test]
    async fn tool_outcome_shell_exec_timeout_is_recoverable_and_emits_failed() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({"command": "sleep 1", "timeout_ms": 1})),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("timed out"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(events.contains("\"tool\":\"shell_exec\""));
        assert!(events.contains("\"tool_call_id\":\"shell_call\""));
    }

    #[tokio::test]
    async fn shell_exec_timeout_returns_partial_output_and_emits_deltas() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({
                    "command": "printf 'hello from stdout\\n'; printf 'hello from stderr\\n' >&2; sleep 5",
                    "timeout_ms": 1500
                })),
            )
            .await
            .unwrap();

        let wire: Value = serde_json::from_str(&out.content).unwrap();
        assert!(wire["stdout"]
            .as_str()
            .unwrap()
            .contains("hello from stdout"));
        assert!(wire["stderr"]
            .as_str()
            .unwrap()
            .contains("hello from stderr"));
        assert_eq!(wire["timed_out"], true);

        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.stdout.delta\""));
        assert!(events.contains("\"type\":\"tool.stderr.delta\""));
        assert!(events.contains("hello from stdout"));
        assert!(events.contains("hello from stderr"));
    }

    #[tokio::test]
    async fn shell_exec_timeout_caps_wire_output_and_preserves_full_delta() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({
                    "command": "awk 'BEGIN { for (i = 0; i < 20000; i++) printf \"A\" }'; awk 'BEGIN { for (i = 0; i < 20000; i++) printf \"E\" }' >&2; sleep 5",
                    "timeout_ms": 1500
                })),
            )
            .await
            .unwrap();

        let wire: Value = serde_json::from_str(&out.content).unwrap();
        let wire_stdout = wire["stdout"].as_str().unwrap();
        assert!(wire_stdout.len() <= WIRE_OUTPUT_CAP_BYTES);
        assert!(wire_stdout.contains("bytes elided from the middle"));
        let wire_stderr = wire["stderr"].as_str().unwrap();
        assert!(wire_stderr.len() <= WIRE_OUTPUT_CAP_BYTES);
        assert!(wire_stderr.contains("bytes elided from the middle"));
        assert_eq!(wire["wire_truncated"], true);

        let events: Vec<Value> = std::fs::read_to_string(&journal)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let stdout_event = events
            .iter()
            .find(|event| event["type"] == "tool.stdout.delta")
            .expect("tool.stdout.delta event");
        assert!(stdout_event["payload"]["text"].as_str().unwrap().len() > WIRE_OUTPUT_CAP_BYTES);
        let stderr_event = events
            .iter()
            .find(|event| event["type"] == "tool.stderr.delta")
            .expect("tool.stderr.delta event");
        assert!(stderr_event["payload"]["text"].as_str().unwrap().len() > WIRE_OUTPUT_CAP_BYTES);
    }

    #[tokio::test]
    async fn shell_exec_timeout_stays_recoverable_with_actionable_error() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(
                &mut ctx,
                &call(json!({"command": "sleep 5", "timeout_ms": 1500})),
            )
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        let wire: Value = serde_json::from_str(&out.content).unwrap();
        let error = wire["error"].as_str().unwrap();
        assert!(error.contains("command timed out after 2s"));
        assert!(
            error.contains("Running the same command again unchanged will likely time out again")
        );
        assert!(error.contains("faster source or mirror"));
        assert!(error.contains("split the command into smaller steps"));
        assert!(error.contains("larger timeout_ms"));
        assert_eq!(wire["timed_out"], true);
    }

    #[tokio::test]
    async fn tool_outcome_shell_exec_bad_args_is_recoverable_and_emits_failed() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(&mut ctx, &call(json!({})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::FailedRecoverable);
        assert!(!out.invalidates_verification);
        assert!(out.content.contains("bad arguments"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
        assert!(events.contains("\"tool\":\"shell_exec\""));
        assert!(events.contains("\"tool_call_id\":\"shell_call\""));
    }

    #[test]
    fn only_shell_unavailable_errors_are_recoverable_spawn_failures() {
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let message = "POSIX shell not found".to_string();
        let outcome = recover_shell_unavailable(
            &mut recorder,
            "shell_exec",
            "shell_call",
            &HarnessError::ShellUnavailable(message.clone()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.status, crate::tools::ToolStatus::FailedRecoverable);
        assert_eq!(outcome.content, message);
        assert!(!outcome.invalidates_verification);

        assert_eq!(
            recover_shell_unavailable(
                &mut recorder,
                "shell_exec",
                "shell_call",
                &HarnessError::Io(std::io::Error::from(std::io::ErrorKind::NotFound)),
            )
            .unwrap(),
            None
        );
        assert_eq!(
            recover_shell_unavailable(
                &mut recorder,
                "shell_exec",
                "shell_call",
                &HarnessError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            )
            .unwrap(),
            None
        );
        let events = std::fs::read_to_string(&journal).unwrap();
        assert_eq!(events.matches("\"type\":\"tool.failed\"").count(), 1);
        assert!(events.contains("POSIX shell not found"));
    }

    #[tokio::test]
    async fn tool_outcome_shell_exec_blocks_dangerous_command() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        let out = ShellExecTool
            .execute(&mut ctx, &call(json!({"command": "rm -rf /etc"})))
            .await
            .unwrap();

        assert_eq!(out.status, crate::tools::ToolStatus::Rejected);
        assert!(out.content.contains("blocked"));
        assert!(out.content.contains("rm_system_path"));
        let events = std::fs::read_to_string(&journal).unwrap();
        assert!(events.contains("\"type\":\"tool.failed\""));
    }

    #[tokio::test]
    async fn tool_outcome_shell_exec_allows_in_workspace_relative_write() {
        let workspace = tempfile::tempdir().unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal = journal_dir.path().join("events.jsonl");
        let mut recorder =
            EventRecorder::new("r", None, None, &journal, crate::events::OutputMode::Silent)
                .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = context(workspace.path(), &mut recorder, &mut ledger);

        // 工作区内相对重定向目标必须放行（守住 canonical workspace 修复·否则会被误判越界）
        let out = ShellExecTool
            .execute(&mut ctx, &call(json!({"command": "echo hi > out.txt"})))
            .await
            .unwrap();
        assert_eq!(out.status, crate::tools::ToolStatus::Success);
    }
