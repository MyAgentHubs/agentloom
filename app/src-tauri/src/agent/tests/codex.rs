#![cfg(test)]

use super::*;

#[cfg(not(windows))]
#[test]
fn resolve_codex_bin_keeps_non_windows_bare_name() {
    assert_eq!(resolve_codex_bin().unwrap(), OsString::from("codex"));
}

#[test]
fn resolve_codex_bin_reads_the_spawn_override_cache() {
    let _guard = crate::detect::CliPathOverrideTestGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("codex");
    std::fs::write(&cli, "test cli").unwrap();
    crate::detect::set_cached_cli_path("codex", cli.to_str()).unwrap();

    let resolved = resolve_codex_bin().unwrap();
    crate::detect::set_cached_cli_path("codex", None).unwrap();

    assert_eq!(resolved, cli.into_os_string());
}

#[test]
fn resolve_codex_bin_ignores_the_global_override_cache_without_explicit_test_opt_in() {
    let _lock = crate::detect::CLI_PATH_OVERRIDE_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    crate::detect::replace_cached_cli_paths([]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("codex");
    std::fs::write(&cli, "test cli").unwrap();
    crate::detect::set_cached_cli_path("codex", cli.to_str()).unwrap();

    let resolved = resolve_codex_bin().unwrap();
    crate::detect::replace_cached_cli_paths([]).unwrap();

    assert_eq!(resolved, OsString::from("codex"));
}

#[test]
fn solo_codex_rebuild_preserves_shim_script_once_before_injected_config() {
    let script = OsString::from(r"C:\npm\node_modules\@openai\codex\bin\codex.js");
    let mut command = crate::proc::command(r"C:\npm\node.exe");
    command.args([
        script.as_os_str(),
        OsStr::new("-a"),
        OsStr::new("never"),
        OsStr::new("exec"),
        OsStr::new("--json"),
        OsStr::new("prompt"),
    ]);
    let mut profile = borrow_profile();
    profile.provider = "codex".to_string();
    profile.access = "native".to_string();

    attach_solo_commit_mcp_argv(&mut command, &profile, 4321).unwrap();

    let rebuilt_args = command
        .get_args()
        .map(OsStr::to_os_string)
        .collect::<Vec<_>>();
    assert_eq!(command.get_program(), r"C:\npm\node.exe");
    assert_eq!(rebuilt_args.first(), Some(&script));
    assert_eq!(rebuilt_args.iter().filter(|arg| *arg == &script).count(), 1);
    let exec_index = rebuilt_args
        .windows(2)
        .position(|pair| pair[0] == "exec" && pair[1] == "--json")
        .expect("exec --json should survive rebuilding");
    let injected_config_index = rebuilt_args
        .iter()
        .position(|arg| arg == r#"mcp_servers.agentloom.url="http://127.0.0.1:4321/mcp""#)
        .expect("agentloom MCP config should be injected");
    assert_eq!(rebuilt_args[injected_config_index - 1], "-c");
    assert!(injected_config_index < exec_index);
}

#[test]
fn native_codex_receives_inline_hook_config_and_hidden_token() {
    let test = setup_context();
    let ctx = build_context(&test, "hi");
    let cmd = NativeBackend {
        provider: "codex".to_string(),
        primary_model: Some("gpt-test".to_string()),
    }
    .build_command(&ctx)
    .unwrap();
    let args = command_args(&cmd);
    let exec_index = args.iter().position(|arg| arg == "exec").unwrap();
    let bypass_index = args
        .iter()
        .position(|arg| arg == "--dangerously-bypass-hook-trust")
        .unwrap();

    assert!(contains_adjacent_pair(&args, "-c", "features.hooks=true"));
    assert!(args.iter().any(|arg| {
        arg.starts_with("hooks.PreToolUse=[")
            && arg.contains("matcher = \"^apply_patch$\"")
            && arg.contains("127.0.0.1:9/checkpoint")
    }));
    assert!(!args
        .iter()
        .any(|arg| arg.starts_with("hooks.PostToolUse=[")));
    assert!(
        bypass_index < exec_index,
        "hook trust flag must precede exec"
    );
    let token = env_value(&cmd, crate::checkpoint_hook::TOKEN_ENV)
        .flatten()
        .expect("checkpoint token should be injected through the environment");
    assert_eq!(token.len(), 64);
    assert!(!args.iter().any(|arg| arg.contains(&token)));
}

#[test]
fn native_codex_parsefn_codex() {
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };

    assert_eq!(backend.parse_fn(), ParseFn::Codex);
}

#[test]
fn native_codex_args_has_exec_json() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        contains_adjacent_pair(&args, "exec", "--json"),
        "expected exec --json in args: {args:?}"
    );
    assert_codex_approval_never_before_exec(&args);
    let exec_idx = args
        .iter()
        .position(|arg| arg == "exec")
        .expect("expected exec in args");
    let ignore_user_config_idx = args
        .iter()
        .position(|arg| arg == "--ignore-user-config")
        .expect("expected --ignore-user-config in args");
    assert!(
        exec_idx < ignore_user_config_idx,
        "expected --ignore-user-config after exec in args: {args:?}"
    );
}

#[test]
fn native_codex_write_modes_instruct_image_outputs_to_persist_in_workspace() {
    const IMAGE_OUTPUT_INSTRUCTION: &str =
        "save or copy them into the current workspace and state each image's absolute path";

    let test = setup_context();
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };

    for mode in [BuildMode::Normal, BuildMode::Worker] {
        let ctx = build_context_for_mode(&test, "create an image", mode);
        let cmd = backend.build_command(&ctx).unwrap();
        let args = command_args(&cmd);
        assert_eq!(
            args.last().map(String::as_str),
            Some("-"),
            "prompt 正文不再进 argv，位置参数应是 \"-\"：{args:?}"
        );
        let stdin_prompt = backend
            .stdin_prompt(&ctx)
            .expect("codex stdin prompt should exist");

        assert!(
            stdin_prompt.contains(IMAGE_OUTPUT_INSTRUCTION),
            "expected image persistence instruction for {mode:?}: {stdin_prompt:?}"
        );
    }

    for mode in [
        BuildMode::LeadDraft,
        BuildMode::LeadAction,
        BuildMode::Summarize,
    ] {
        let ctx = build_context_for_mode(&test, "review an image request", mode);
        let cmd = backend.build_command(&ctx).unwrap();
        let args = command_args(&cmd);
        assert_eq!(args.last().map(String::as_str), Some("-"), "{args:?}");
        let stdin_prompt = backend
            .stdin_prompt(&ctx)
            .expect("codex stdin prompt should exist");

        assert!(
            !stdin_prompt.contains(IMAGE_OUTPUT_INSTRUCTION),
            "did not expect image persistence instruction for {mode:?}: {stdin_prompt:?}"
        );
    }
}

#[test]
fn native_codex_write_modes_teach_markdown_inline_image_syntax() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };

    for mode in [BuildMode::Normal, BuildMode::Worker] {
        let ctx = build_context_for_mode(&test, "create an image", mode);
        let cmd = backend.build_command(&ctx).unwrap();
        let args = command_args(&cmd);
        assert_eq!(args.last().map(String::as_str), Some("-"), "{args:?}");
        let stdin_prompt = backend
            .stdin_prompt(&ctx)
            .expect("codex stdin prompt should exist");

        assert!(
            stdin_prompt.contains("![]("),
            "expected Markdown inline image syntax guidance for {mode:?}: {stdin_prompt:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn native_codex_command_uses_augmented_path() {
    const CHILD_ENV: &str = "AGENTLOOM_TEST_CODEX_AUGMENTED_PATH_CHILD";

    if std::env::var_os(CHILD_ENV).is_some() {
        let test = setup_context();
        let expected_dir = test.home.join(".local/bin");
        std::fs::create_dir_all(&expected_dir).unwrap();
        let ctx = build_context(&test, "hi");
        let cmd = NativeBackend {
            provider: "codex".to_string(),
            primary_model: Some("gpt-test".to_string()),
        }
        .build_command(&ctx)
        .unwrap();

        let path = env_value(&cmd, "PATH")
            .flatten()
            .expect("codex command should receive the augmented PATH");
        let path_entries: Vec<PathBuf> = std::env::split_paths(OsStr::new(&path)).collect();
        assert!(
            path_entries.contains(&expected_dir),
            "expected {} in codex PATH: {path:?}",
            expected_dir.display()
        );
        return;
    }

    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "agent::tests::codex::native_codex_command_uses_augmented_path",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env("PATH", "/usr/bin:/bin")
        .env("AGENTLOOM_SKIP_SHELL_PATH", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "controlled child test failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("test agent::tests::codex::native_codex_command_uses_augmented_path ... ok"),
        "controlled child must execute the exact test, not silently run zero tests:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn native_codex_stale_primary_model_uses_config_model_before_exec() {
    let test = setup_context();
    write_codex_config(&test.home, r#"model = "gpt-5.5""#);
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: Some("gpt-5".to_string()),
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert_codex_model_before_exec(&args, "gpt-5.5");
}

#[test]
fn native_codex_stale_gpt53_primary_model_uses_config_model_before_exec() {
    let test = setup_context();
    write_codex_config(&test.home, r#"model = "gpt-5.5""#);
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: Some("gpt-5.3-codex".to_string()),
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert_codex_model_before_exec(&args, "gpt-5.5");
    assert!(
        !contains_adjacent_pair(&args, "-m", "gpt-5.3-codex"),
        "did not expect stale primary model in args: {args:?}"
    );
}

#[test]
fn native_codex_custom_primary_model_wins_over_config_model() {
    let test = setup_context();
    write_codex_config(&test.home, r#"model = "gpt-5.5""#);
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: Some("custom-codex-model".to_string()),
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert_codex_model_before_exec(&args, "custom-codex-model");
    assert!(
        !contains_adjacent_pair(&args, "-m", "gpt-5.5"),
        "did not expect config fallback to replace custom model: {args:?}"
    );
}

#[test]
fn native_codex_missing_primary_model_uses_config_model() {
    let test = setup_context();
    write_codex_config(&test.home, r#"model = "gpt-5.5""#);
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert_codex_model_before_exec(&args, "gpt-5.5");
}

#[test]
fn native_codex_ignores_profile_section_model_for_config_fallback() {
    let test = setup_context();
    write_codex_config(
        &test.home,
        r#"
[profiles.some]
model = "nested-model"
"#,
    );
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        !contains_adjacent_pair(&args, "-m", "nested-model"),
        "did not expect nested profile model in args: {args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg == "-m"),
        "did not expect any model fallback from profile section: {args:?}"
    );
}

#[test]
fn native_codex_lead_action_prefixes_lead_system_prompt_into_prompt_arg() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };
    let ctx = BuildContext {
        prompt: "decide next",
        session_id: &test.session_id,
        run_id: "test-run",
        wt: &test.home,
        conn: &test.conn,
        mode: BuildMode::LeadAction,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);
    assert_eq!(args.last().map(String::as_str), Some("-"), "{args:?}");
    let stdin_prompt = backend
        .stdin_prompt(&ctx)
        .expect("codex stdin prompt should exist");

    assert!(
        stdin_prompt.contains(crate::lead_step::LEAD_DECISION_SYS_PROMPT),
        "expected lead action system prompt in Codex stdin prompt: {stdin_prompt:?}"
    );
    assert!(
        stdin_prompt.ends_with("decide next"),
        "expected user prompt after system prompt: {stdin_prompt:?}"
    );
    assert!(
        contains_adjacent_pair(&args, "--sandbox", "read-only"),
        "lead action must use Codex's read-only sandbox: {args:?}"
    );
    assert_eq!(
        env_value(&cmd, crate::checkpoint_hook::TOKEN_ENV),
        Some(None),
        "lead action should not receive a checkpoint token"
    );
}

#[test]
fn native_codex_reasoning_uses_model_reasoning_effort_config() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: &test.session_id,
        run_id: "test-run",
        wt: &test.home,
        conn: &test.conn,
        mode: BuildMode::Normal,
        locale: crate::Locale::Zh,
        reasoning_tier: Some("xhigh"),
        criteria: &[],
    };

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        contains_adjacent_pair(&args, "-c", "model_reasoning_effort=\"xhigh\""),
        "expected Codex reasoning config in args: {args:?}"
    );
}
