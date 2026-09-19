#![cfg(test)]

use super::*;

#[test]
fn borrow_sets_four_mappings() {
    let cmd = borrow_command(borrow_profile());

    assert_eq!(
        env_value(&cmd, "ANTHROPIC_DEFAULT_OPUS_MODEL"),
        Some(Some("m".to_string()))
    );
    assert_eq!(
        env_value(&cmd, "ANTHROPIC_DEFAULT_SONNET_MODEL"),
        Some(Some("m".to_string()))
    );
    assert_eq!(
        env_value(&cmd, "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
        Some(Some("m".to_string()))
    );
    assert_eq!(
        env_value(&cmd, "CLAUDE_CODE_SUBAGENT_MODEL"),
        Some(Some("m".to_string()))
    );
}

#[test]
fn borrow_explicit_mappings_win() {
    let mut profile = borrow_profile();
    profile.model_haiku = Some("h".to_string());
    let cmd = borrow_command(profile);

    assert_eq!(
        env_value(&cmd, "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
        Some(Some("h".to_string()))
    );
}

#[test]
fn auth_bearer_sets_auth_token() {
    let mut profile = borrow_profile();
    profile.auth_mode = Some("bearer".to_string());
    let cmd = borrow_command(profile);

    assert_eq!(
        env_value(&cmd, "ANTHROPIC_AUTH_TOKEN"),
        Some(Some("test-key".to_string()))
    );
    assert!(
        !matches!(env_value(&cmd, "ANTHROPIC_API_KEY"), Some(Some(_))),
        "x-api-key env should not be set for bearer auth"
    );
}

#[test]
fn auth_xapikey_sets_api_key() {
    let mut profile = borrow_profile();
    profile.auth_mode = Some("x_api_key".to_string());
    let cmd = borrow_command(profile);

    assert_eq!(
        env_value(&cmd, "ANTHROPIC_API_KEY"),
        Some(Some("test-key".to_string()))
    );
    assert!(
        !matches!(env_value(&cmd, "ANTHROPIC_AUTH_TOKEN"), Some(Some(_))),
        "auth token env should not be set for x-api-key auth"
    );
}

#[test]
fn effort_from_reasoning_default() {
    let mut profile = borrow_profile();
    profile.reasoning_default = "high".to_string();
    let cmd = borrow_command(profile);

    assert_eq!(
        env_value(&cmd, "CLAUDE_CODE_EFFORT_LEVEL"),
        Some(Some("high".to_string()))
    );
}

#[test]
fn runtime_reasoning_override_wins_over_profile_default() {
    let test = setup_context();
    let mut profile = borrow_profile();
    profile.reasoning_default = "high".to_string();
    let backend = BorrowClaudeBackend {
        profile,
        api_key: "test-key".to_string(),
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: &test.session_id,
        run_id: "test-run",
        wt: &test.home,
        conn: &test.conn,
        mode: BuildMode::Normal,
        locale: crate::Locale::Zh,
        reasoning_tier: Some("low"),
        criteria: &[],
    };

    let cmd = backend.build_command(&ctx).unwrap();

    assert_eq!(
        env_value(&cmd, "CLAUDE_CODE_EFFORT_LEVEL"),
        Some(Some("low".to_string()))
    );
}

#[test]
fn borrow_normal_en_appends_identity_and_language_directive() {
    let test = setup_context();
    let backend = BorrowClaudeBackend {
        profile: borrow_profile(),
        api_key: "test-key".to_string(),
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: &test.session_id,
        run_id: "test-run",
        wt: &test.home,
        conn: &test.conn,
        mode: BuildMode::Normal,
        locale: crate::Locale::En,
        reasoning_tier: None,
        criteria: &[],
    };

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);
    let system_prompt = args
        .windows(2)
        .find(|window| window[0] == "--append-system-prompt")
        .map(|window| window[1].as_str())
        .expect("expected --append-system-prompt value");

    assert!(system_prompt.contains("绝不能自称 Claude"), "{args:?}");
    assert!(
        system_prompt.contains("reply in the SAME language"),
        "{args:?}"
    );
}

#[test]
fn borrow_normal_also_appends_solo_image_output_guidance() {
    // 实勘：BorrowClaudeBackend（deepseek 等借壳会话）同样经 system_prompt_for_mode
    // 消费 Normal 的产图引导——这是预期的可接受行为，不为它加特判。
    let test = setup_context();
    let backend = BorrowClaudeBackend {
        profile: borrow_profile(),
        api_key: "test-key".to_string(),
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: &test.session_id,
        run_id: "test-run",
        wt: &test.home,
        conn: &test.conn,
        mode: BuildMode::Normal,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);
    let system_prompt = args
        .windows(2)
        .find(|window| window[0] == "--append-system-prompt")
        .map(|window| window[1].as_str())
        .expect("expected --append-system-prompt value");

    assert!(
        system_prompt.contains(SOLO_IMAGE_OUTPUT_GUIDANCE),
        "expected solo image output guidance appended after the borrow identity prompt: {args:?}"
    );
    assert!(
        system_prompt.contains("绝不能自称 Claude"),
        "identity prompt must still precede the image guidance: {args:?}"
    );
}

#[test]
fn borrow_worker_does_not_append_language_directive() {
    let test = setup_context();
    let backend = BorrowClaudeBackend {
        profile: borrow_profile(),
        api_key: "test-key".to_string(),
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: &test.session_id,
        run_id: "test-run",
        wt: &test.home,
        conn: &test.conn,
        mode: BuildMode::Worker,
        locale: crate::Locale::En,
        reasoning_tier: None,
        criteria: &[],
    };

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);
    let system_prompt = args
        .windows(2)
        .find(|window| window[0] == "--append-system-prompt")
        .map(|window| window[1].as_str())
        .expect("expected --append-system-prompt value");

    assert!(system_prompt.contains("绝不能自称 Claude"), "{args:?}");
    assert!(
        !system_prompt.contains("reply in the SAME language"),
        "{args:?}"
    );
    assert!(!system_prompt.contains("语言要求"), "{args:?}");
}

#[test]
fn config_dir_within_temp_root() {
    let mut profile = borrow_profile();
    profile.id = "a/../b".to_string();
    let cmd = borrow_command(profile);
    let config_dir = env_value(&cmd, "CLAUDE_CONFIG_DIR")
        .and_then(|v| v)
        .expect("CLAUDE_CONFIG_DIR should be set");
    let config_dir = std::path::PathBuf::from(config_dir);
    let config_dir = std::fs::canonicalize(&config_dir).unwrap_or(config_dir);
    let tmp = std::env::temp_dir();
    let tmp = std::fs::canonicalize(&tmp).unwrap_or(tmp);

    assert!(
        config_dir.starts_with(&tmp),
        "config dir should stay inside temp root: config={config_dir:?} tmp={tmp:?}"
    );
}

#[test]
fn clean_env_removes_keys_value_none() {
    let cmd = borrow_command(borrow_profile());

    assert_eq!(env_value(&cmd, "CLAUDE_CODE_DISABLE_THINKING"), Some(None));
}

#[test]
fn native_claude_args_has_model_when_set() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "claude".to_string(),
        primary_model: Some("opus-x".to_string()),
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        contains_adjacent_pair(&args, "--model", "opus-x"),
        "expected --model opus-x in args: {args:?}"
    );
}

#[test]
fn native_claude_model_omits_blank_and_keeps_non_empty() {
    let test = setup_context();

    for model in ["", "   "] {
        let backend = NativeBackend {
            provider: "claude".to_string(),
            primary_model: Some(model.to_string()),
        };
        let ctx = build_context(&test, "hi");
        let args = command_args(&backend.build_command(&ctx).unwrap());

        assert!(
            !args.iter().any(|arg| arg == "--model"),
            "did not expect --model for blank model {model:?}: {args:?}"
        );
    }

    let backend = NativeBackend {
        provider: "claude".to_string(),
        primary_model: Some("sonnet".to_string()),
    };
    let ctx = build_context(&test, "hi");
    let args = command_args(&backend.build_command(&ctx).unwrap());

    assert!(
        contains_adjacent_pair(&args, "--model", "sonnet"),
        "expected adjacent --model sonnet in args: {args:?}"
    );
}

#[test]
fn native_claude_no_model_when_none() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "claude".to_string(),
        primary_model: None,
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        !args.iter().any(|arg| arg == "--model"),
        "did not expect --model in args: {args:?}"
    );
}

/// solo / native lead 都经 `NativeBackend` → `claude_sandboxed_cmd_in` →
/// `claude_agent_argv()`；正文走 stdin 后该基础项必须恰好出现 1 次，不能被
/// 上层再 ad-hoc 加一遍出现两次。
#[test]
fn native_claude_argv_has_exactly_one_disable_slash_commands() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "claude".to_string(),
        primary_model: None,
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert_eq!(
        args.iter()
            .filter(|arg| arg.as_str() == "--disable-slash-commands")
            .count(),
        1,
        "native claude（solo / native lead 共用）必须恰好带 1 条 --disable-slash-commands: {args:?}"
    );
}

#[test]
fn native_claude_lead_draft_appends_lead_system_prompt() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "claude".to_string(),
        primary_model: None,
    };
    let ctx = BuildContext {
        prompt: "draft this",
        session_id: &test.session_id,
        run_id: "test-run",
        wt: &test.home,
        conn: &test.conn,
        mode: BuildMode::LeadDraft,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        contains_adjacent_pair(
            &args,
            "--append-system-prompt",
            crate::lead_draft::LEAD_DRAFT_SYS_PROMPT
        ),
        "expected lead draft system prompt in Claude args: {args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg == "draft this"),
        "prompt 正文不再进 argv，应改走 stdin: {args:?}"
    );
    assert_eq!(
        backend.stdin_prompt(&ctx).as_deref(),
        Some("draft this"),
        "expected original prompt via stdin_prompt"
    );
    assert!(
        contains_adjacent_pair(
            &args,
            "--disallowedTools",
            "Write,Edit,MultiEdit,NotebookEdit,Bash"
        ),
        "lead draft must not expose untracked write tools: {args:?}"
    );
    assert_eq!(
        env_value(&cmd, crate::checkpoint_hook::TOKEN_ENV),
        Some(None),
        "lead draft should not receive a checkpoint token"
    );
}

#[test]
fn native_claude_normal_appends_solo_image_output_guidance() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "claude".to_string(),
        primary_model: None,
    };
    let ctx = build_context(&test, "hi");

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        contains_adjacent_pair(
            &args,
            "--append-system-prompt",
            SOLO_IMAGE_OUTPUT_GUIDANCE
        ),
        "expected solo image output guidance via --append-system-prompt for Normal claude: {args:?}"
    );
    assert!(
        SOLO_IMAGE_OUTPUT_GUIDANCE.contains("![]("),
        "solo image output guidance should teach the Markdown inline image syntax: {SOLO_IMAGE_OUTPUT_GUIDANCE}"
    );
}

#[test]
fn native_claude_lead_action_system_prompt_stays_pure() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "claude".to_string(),
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

    assert!(
        contains_adjacent_pair(
            &args,
            "--append-system-prompt",
            crate::lead_step::LEAD_DECISION_SYS_PROMPT
        ),
        "LeadAction's Claude system prompt must remain exactly LEAD_DECISION_SYS_PROMPT, unpolluted by the solo image guidance: {args:?}"
    );
}

#[test]
fn claude_effort_clamps_reasoning_tiers_to_supported_values() {
    assert_eq!(claude_effort_for_reasoning_tier("auto"), Some("medium"));
    assert_eq!(claude_effort_for_reasoning_tier("none"), Some("low"));
    assert_eq!(claude_effort_for_reasoning_tier("minimal"), Some("low"));
    assert_eq!(claude_effort_for_reasoning_tier("high"), Some("high"));
    assert_eq!(claude_effort_for_reasoning_tier("max"), Some("max"));
    assert_eq!(claude_effort_for_reasoning_tier(""), None);
    assert_eq!(claude_effort_for_reasoning_tier("turbo"), None);
    assert_eq!(claude_effort_for_reasoning_tier("  HIGH  "), Some("high"));
}

#[test]
fn native_claude_reasoning_uses_effort_arg_and_auto_medium() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "claude".to_string(),
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
        reasoning_tier: Some("auto"),
        criteria: &[],
    };

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        contains_adjacent_pair(&args, "--effort", "medium"),
        "expected --effort medium in args: {args:?}"
    );
    assert_eq!(env_value(&cmd, "CLAUDE_CODE_EFFORT_LEVEL"), None);
}

#[test]
fn native_claude_effort_clamps_minimal_and_omits_unknown() {
    let test = setup_context();
    let backend = NativeBackend {
        provider: "claude".to_string(),
        primary_model: None,
    };

    for (tier, expected) in [("minimal", Some("low")), ("turbo", None)] {
        let ctx = BuildContext {
            prompt: "hi",
            session_id: &test.session_id,
            run_id: "test-run",
            wt: &test.home,
            conn: &test.conn,
            mode: BuildMode::Normal,
            locale: crate::Locale::Zh,
            reasoning_tier: Some(tier),
            criteria: &[],
        };

        let cmd = backend.build_command(&ctx).unwrap();
        let args = command_args(&cmd);
        match expected {
            Some(effort) => assert!(
                contains_adjacent_pair(&args, "--effort", effort),
                "expected --effort {effort} in args: {args:?}"
            ),
            None => assert!(
                !args.iter().any(|arg| arg == "--effort"),
                "unknown tier should omit --effort: {args:?}"
            ),
        }
    }
}
