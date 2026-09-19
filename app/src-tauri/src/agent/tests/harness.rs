#![cfg(test)]

use super::*;

#[test]
fn harness_build_command_has_run_jsonl_provider_permission() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let ctx = build_context(&test, "fix the bug");
    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);
    assert_eq!(args.first().map(String::as_str), Some("run"));
    let prompt_path = harness_prompt_path(&args);
    assert_eq!(std::fs::read_to_string(prompt_path).unwrap(), "fix the bug");
    assert!(args.iter().any(|a| a == "--jsonl"), "{args:?}");
    assert!(
        contains_adjacent_pair(&args, "--provider", "deepseek"),
        "{args:?}"
    );
    assert!(
        contains_adjacent_pair(&args, "--permission", "allow"),
        "{args:?}"
    );
    assert!(args.iter().any(|a| a == "--client-session-id"), "{args:?}");
    assert!(args.iter().any(|a| a == "--workspace"), "{args:?}");
    assert!(args.iter().any(|a| a == "--journal-dir"), "{args:?}");
    assert!(args.windows(2).any(|w| w[0] == "--journal-dir"), "{args:?}");
}

#[test]
fn harness_build_command_uses_plan_when_env_enabled() {
    let _mode = set_harness_mode_for_test(Some("plan"));
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let ctx = build_context(&test, "fix the bug");
    let cmd = backend.build_command(&ctx).unwrap();

    let args = command_args(&cmd);
    assert_eq!(args.first().map(String::as_str), Some("plan"));
    let prompt_path = harness_prompt_path(&args);
    assert_eq!(std::fs::read_to_string(prompt_path).unwrap(), "fix the bug");
    assert!(args.iter().any(|a| a == "--jsonl"), "{args:?}");
    assert!(
        contains_adjacent_pair(&args, "--provider", "deepseek"),
        "{args:?}"
    );
    assert!(
        contains_adjacent_pair(&args, "--permission", "allow"),
        "{args:?}"
    );
    assert!(args.iter().any(|a| a == "--workspace"), "{args:?}");
    assert!(args.iter().any(|a| a == "--journal-dir"), "{args:?}");
    assert!(
        !args.iter().any(|a| a == "--client-session-id"),
        "plan mode must not pass --client-session-id because myagent plan does not accept it: {args:?}"
    );
}

#[test]
fn harness_build_command_writes_large_prompt_to_app_domain_file() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let workspace = test.home.join("user-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let first_prompt = "x".repeat(2 * 1024 * 1024 + 1);
    let second_prompt = "second prompt";
    let mut ctx = BuildContext {
        prompt: &first_prompt,
        session_id: &test.session_id,
        run_id: "large-prompt-run",
        wt: &workspace,
        conn: &test.conn,
        mode: BuildMode::Normal,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };

    let first_args = command_args(&backend.build_command(&ctx).unwrap());
    assert!(!first_args.iter().any(|arg| arg == &first_prompt));
    let first_path = harness_prompt_path(&first_args);
    assert!(first_path.starts_with(crate::worktree::journals_dir()));
    assert!(!first_path.starts_with(&workspace));
    assert_eq!(std::fs::read(&first_path).unwrap(), first_prompt.as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&first_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    ctx.prompt = second_prompt;
    let second_args = command_args(&backend.build_command(&ctx).unwrap());
    let second_path = harness_prompt_path(&second_args);
    assert_ne!(first_path, second_path);
    assert!(
        first_path.exists(),
        "首个在途 prompt 文件不应在构造期被清理"
    );
    assert_eq!(std::fs::read(&first_path).unwrap(), first_prompt.as_bytes());
    assert_eq!(
        std::fs::read(&second_path).unwrap(),
        second_prompt.as_bytes()
    );
}

#[test]
fn harness_prompt_cleanup_removes_only_expired_files() {
    let test = setup_context();
    let prompts_dir = crate::worktree::journals_dir()
        .join(&test.session_id)
        .join("prompts");
    std::fs::create_dir_all(&prompts_dir).unwrap();
    let old_path = prompts_dir.join("old.txt");
    std::fs::write(&old_path, b"old").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let new_path = prompts_dir.join("new.txt");
    std::fs::write(&new_path, b"new").unwrap();
    let old_modified = std::fs::metadata(&old_path).unwrap().modified().unwrap();
    let new_modified = std::fs::metadata(&new_path).unwrap().modified().unwrap();
    assert!(
        old_modified < new_modified,
        "测试前置要求新旧文件的 mtime 可区分"
    );

    cleanup_expired_harness_prompt_files(
        &prompts_dir,
        HARNESS_PROMPT_FILE_MAX_AGE,
        new_modified + HARNESS_PROMPT_FILE_MAX_AGE,
    );

    assert!(!old_path.exists(), "mtime 早于一小时阈值的旧文件应被清理");
    assert!(new_path.exists(), "mtime 位于一小时阈值的新文件应保留");
    assert_eq!(std::fs::read_to_string(new_path).unwrap(), "new");
}

#[test]
fn harness_build_command_passes_criteria_as_repeated_args() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let criteria = vec!["cmd: cargo test".to_string(), "judge: inspect".to_string()];
    let ctx = BuildContext {
        prompt: "fix the bug",
        session_id: &test.session_id,
        run_id: "test-run",
        wt: &test.home,
        conn: &test.conn,
        mode: BuildMode::Normal,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &criteria,
    };

    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        args.windows(2)
            .any(|w| w[0] == "--criteria" && w[1] == "cmd: cargo test"),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--criteria" && w[1] == "judge: inspect"),
        "{args:?}"
    );
}

#[test]
fn harness_injects_env_from_profile_and_key() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let mut profile = harness_profile();
    profile.endpoint = Some("https://example.test/v1".to_string());
    profile.primary_model = Some("deepseek-chat".to_string());
    let backend = HarnessBackend {
        profile,
        api_key: Some("k".to_string()),
        search_api_key: Some("search-k".to_string()),
        search_backend: Some("exa".to_string()),
    };
    let ctx = build_context(&test, "fix the bug");

    let cmd = backend.build_command(&ctx).unwrap();

    assert_eq!(env_value(&cmd, "MYAGENT_API_KEY"), Some(Some("k".into())));
    assert_eq!(
        env_value(&cmd, "MYAGENT_SEARCH_API_KEY"),
        Some(Some("search-k".into()))
    );
    assert_eq!(
        env_value(&cmd, "MYAGENT_SEARCH_BACKEND"),
        Some(Some("exa".into()))
    );
    assert_eq!(
        env_value(&cmd, "MYAGENT_BASE_URL"),
        Some(Some("https://example.test/v1".into()))
    );
    assert_eq!(
        env_value(&cmd, "MYAGENT_MODEL"),
        Some(Some("deepseek-chat".into()))
    );
}

#[test]
fn harness_injects_brave_search_backend_env_explicitly() {
    // brave 曾靠「有 key 无名→引擎兜底当 brave」隐式规则；改为不管哪个 backend 都显式传名。
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let mut profile = harness_profile();
    profile.endpoint = Some("https://example.test/v1".to_string());
    profile.primary_model = Some("deepseek-chat".to_string());
    let backend = HarnessBackend {
        profile,
        api_key: Some("k".to_string()),
        search_api_key: Some("search-k".to_string()),
        search_backend: Some("brave".to_string()),
    };
    let ctx = build_context(&test, "fix the bug");

    let cmd = backend.build_command(&ctx).unwrap();

    assert_eq!(
        env_value(&cmd, "MYAGENT_SEARCH_BACKEND"),
        Some(Some("brave".into()))
    );
}

#[test]
fn harness_injects_provider_specific_env_alongside_myagent_env() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let mut profile = harness_profile();
    profile.provider = "glm".to_string();
    profile.endpoint = Some("https://glm.example.test/v1".to_string());
    profile.primary_model = Some("glm-4.5".to_string());
    let backend = HarnessBackend {
        profile,
        api_key: Some("glm-key".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let ctx = build_context(&test, "fix the bug");

    let cmd = backend.build_command(&ctx).unwrap();

    assert_eq!(env_value(&cmd, "GLM_API_KEY"), Some(Some("glm-key".into())));
    assert_eq!(
        env_value(&cmd, "GLM_BASE_URL"),
        Some(Some("https://glm.example.test/v1".into()))
    );
    assert_eq!(env_value(&cmd, "GLM_MODEL"), Some(Some("glm-4.5".into())));
    assert_eq!(
        env_value(&cmd, "MYAGENT_API_KEY"),
        Some(Some("glm-key".into()))
    );
    assert_eq!(
        env_value(&cmd, "MYAGENT_BASE_URL"),
        Some(Some("https://glm.example.test/v1".into()))
    );
    assert_eq!(
        env_value(&cmd, "MYAGENT_MODEL"),
        Some(Some("glm-4.5".into()))
    );
}

/// T3：`agents.api_timeout_ms`（毫秒）→ `MYAGENT_TIMEOUT_SECS`（秒，向上取整，下限 1）。
/// Normal（solo）与 Worker（member）两条模式共用 `HarnessBackend::build_command_inner`，
/// 循环两种 mode 确认 env 装配不依赖 ctx.mode——一并盖住 solo/member 两条 spawn 路径。
#[test]
fn harness_maps_api_timeout_ms_to_myagent_timeout_secs() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();

    for mode in [BuildMode::Normal, BuildMode::Worker] {
        let ctx = BuildContext {
            prompt: "fix the bug",
            session_id: &test.session_id,
            run_id: "test-run",
            wt: &test.home,
            conn: &test.conn,
            mode,
            locale: crate::Locale::Zh,
            reasoning_tier: None,
            criteria: &[],
        };

        // a) 600000ms → 600s（整除）。
        let mut profile_a = harness_profile();
        profile_a.api_timeout_ms = Some(600_000);
        let backend_a = HarnessBackend {
            profile: profile_a,
            api_key: Some("k".to_string()),
            search_api_key: None,
            search_backend: None,
        };
        let cmd_a = backend_a.build_command(&ctx).unwrap();
        assert_eq!(
            env_value(&cmd_a, "MYAGENT_TIMEOUT_SECS"),
            Some(Some("600".into())),
            "mode={mode:?}"
        );

        // b) None → 不设该变量（引擎默认 120 生效）。
        let mut profile_b = harness_profile();
        profile_b.api_timeout_ms = None;
        let backend_b = HarnessBackend {
            profile: profile_b,
            api_key: Some("k".to_string()),
            search_api_key: None,
            search_backend: None,
        };
        let cmd_b = backend_b.build_command(&ctx).unwrap();
        assert_eq!(
            env_value(&cmd_b, "MYAGENT_TIMEOUT_SECS"),
            None,
            "mode={mode:?}"
        );

        // c) 边界 500ms → 向上取整 + 下限 1 → 1s（不是 0，避免触发引擎硬报错）。
        let mut profile_c = harness_profile();
        profile_c.api_timeout_ms = Some(500);
        let backend_c = HarnessBackend {
            profile: profile_c,
            api_key: Some("k".to_string()),
            search_api_key: None,
            search_backend: None,
        };
        let cmd_c = backend_c.build_command(&ctx).unwrap();
        assert_eq!(
            env_value(&cmd_c, "MYAGENT_TIMEOUT_SECS"),
            Some(Some("1".into())),
            "mode={mode:?}"
        );
    }
}

/// 边界：`api_timeout_ms` ≤ 0（脏数据/未来 UI 允许输入 0）不该注入 `MYAGENT_TIMEOUT_SECS=0`——
/// 引擎侧把非法值/0 当硬报错（`ea9ac648`/`bfc4b210`），必须完全不设该变量、让引擎默认值生效。
#[test]
fn harness_omits_timeout_env_when_api_timeout_ms_non_positive() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let ctx = build_context(&test, "fix the bug");

    for bad in [0_i64, -1_i64] {
        let mut profile = harness_profile();
        profile.api_timeout_ms = Some(bad);
        let backend = HarnessBackend {
            profile,
            api_key: Some("k".to_string()),
            search_api_key: None,
            search_backend: None,
        };
        let cmd = backend.build_command(&ctx).unwrap();
        assert_eq!(
            env_value(&cmd, "MYAGENT_TIMEOUT_SECS"),
            None,
            "api_timeout_ms={bad}"
        );
    }
}

#[test]
fn harness_normal_and_worker_inject_checkpoint_token_and_endpoint_env() {
    let _mode = set_harness_mode_for_test(None);
    let _checkpoint_env = set_checkpoint_envs_for_test(
        Some("http://127.0.0.1:65535/checkpoint"),
        Some("stale-parent-token"),
    );
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };

    for mode in [BuildMode::Normal, BuildMode::Worker] {
        let ctx = BuildContext {
            prompt: "fix the bug",
            session_id: &test.session_id,
            run_id: "test-run",
            wt: &test.home,
            conn: &test.conn,
            mode,
            locale: crate::Locale::Zh,
            reasoning_tier: None,
            criteria: &[],
        };

        let cmd = backend.build_command(&ctx).unwrap();

        let token = env_value(&cmd, crate::checkpoint_hook::TOKEN_ENV)
            .flatten()
            .expect("checkpoint token should be injected for myagent write modes");
        assert_eq!(token.len(), 64);
        assert_ne!(token, "stale-parent-token");
        assert_eq!(
            env_value(&cmd, crate::checkpoint_hook::ENDPOINT_ENV),
            Some(Some("http://127.0.0.1:9/checkpoint".into()))
        );
    }
}

#[test]
fn harness_non_write_modes_omit_checkpoint_env() {
    let _mode = set_harness_mode_for_test(None);
    let _checkpoint_env = set_checkpoint_envs_for_test(
        Some("http://127.0.0.1:65535/checkpoint"),
        Some("stale-parent-token"),
    );
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };

    for mode in [
        BuildMode::LeadDraft,
        BuildMode::LeadAction,
        BuildMode::Summarize,
    ] {
        let ctx = BuildContext {
            prompt: "fix the bug",
            session_id: &test.session_id,
            run_id: "test-run",
            wt: &test.home,
            conn: &test.conn,
            mode,
            locale: crate::Locale::Zh,
            reasoning_tier: None,
            criteria: &[],
        };

        let cmd = backend.build_command(&ctx).unwrap();

        assert_eq!(
            env_value(&cmd, crate::checkpoint_hook::TOKEN_ENV),
            Some(None)
        );
        assert_eq!(
            env_value(&cmd, crate::checkpoint_hook::ENDPOINT_ENV),
            Some(None)
        );
    }
}

#[test]
fn harness_read_only_modes_use_permission_deny_in_run_and_plan() {
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };

    for harness_mode in [None, Some("plan")] {
        let _mode = set_harness_mode_for_test(harness_mode);
        let test = setup_context();
        for mode in [
            BuildMode::LeadDraft,
            BuildMode::LeadAction,
            BuildMode::Summarize,
        ] {
            let ctx = BuildContext {
                prompt: "fix the bug",
                session_id: &test.session_id,
                run_id: "test-run",
                wt: &test.home,
                conn: &test.conn,
                mode,
                locale: crate::Locale::Zh,
                reasoning_tier: None,
                criteria: &[],
            };

            let cmd = backend.build_command(&ctx).unwrap();
            let args = command_args(&cmd);

            assert!(
                contains_adjacent_pair(&args, "--permission", "deny"),
                "read-only harness mode must deny writes in {:?}: {args:?}",
                harness_mode
            );
        }
    }
}

#[test]
fn harness_read_only_modes_disallow_mutating_tools() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };

    for mode in [
        BuildMode::LeadDraft,
        BuildMode::LeadAction,
        BuildMode::Summarize,
    ] {
        let ctx = BuildContext {
            prompt: "fix the bug",
            session_id: &test.session_id,
            run_id: "test-run",
            wt: &test.home,
            conn: &test.conn,
            mode,
            locale: crate::Locale::Zh,
            reasoning_tier: None,
            criteria: &[],
        };

        let cmd = backend.build_command(&ctx).unwrap();
        let args = command_args(&cmd);

        assert!(
            contains_adjacent_pair(&args, "--disallow-tools", "fs_edit,fs_write,shell_exec"),
            "read-only harness mode must block mutating tools: {args:?}"
        );
    }
}

#[test]
fn harness_plan_read_only_modes_skip_unsupported_disallow_tools() {
    let _mode = set_harness_mode_for_test(Some("plan"));
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };

    for mode in [
        BuildMode::LeadDraft,
        BuildMode::LeadAction,
        BuildMode::Summarize,
    ] {
        let ctx = BuildContext {
            prompt: "fix the bug",
            session_id: &test.session_id,
            run_id: "test-run",
            wt: &test.home,
            conn: &test.conn,
            mode,
            locale: crate::Locale::Zh,
            reasoning_tier: None,
            criteria: &[],
        };

        let cmd = backend.build_command(&ctx).unwrap();
        let args = command_args(&cmd);

        assert!(
            !args.iter().any(|arg| arg == "--disallow-tools"),
            "plan mode must not pass unsupported --disallow-tools: {args:?}"
        );
    }
}

#[test]
fn harness_normal_and_worker_keep_permission_allow_without_disallow_tools() {
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };

    for harness_mode in [None, Some("plan")] {
        let _mode = set_harness_mode_for_test(harness_mode);
        let test = setup_context();
        for mode in [BuildMode::Normal, BuildMode::Worker] {
            let ctx = BuildContext {
                prompt: "fix the bug",
                session_id: &test.session_id,
                run_id: "test-run",
                wt: &test.home,
                conn: &test.conn,
                mode,
                locale: crate::Locale::Zh,
                reasoning_tier: None,
                criteria: &[],
            };

            let cmd = backend.build_command(&ctx).unwrap();
            let args = command_args(&cmd);

            assert!(
                contains_adjacent_pair(&args, "--permission", "allow"),
                "write-capable harness modes must keep permission allow in {:?}: {args:?}",
                harness_mode
            );
            assert!(
                !args.iter().any(|arg| arg == "--disallow-tools"),
                "write-capable harness modes must keep full tool access in {:?}: {args:?}",
                harness_mode
            );
        }
    }
}

#[test]
fn harness_worker_mode_passes_max_turns_120_normal_does_not() {
    // member worker 的回合预算需与 lead 对齐放宽到 120（引擎默认 40 轮结构性偏小，
    // 详见 HARNESS_MEMBER_MAX_TURNS 注释）；Normal 模式不应被这条改动波及。
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();

    let worker_ctx = BuildContext {
        prompt: "fix the bug",
        session_id: &test.session_id,
        run_id: "test-run",
        wt: &test.home,
        conn: &test.conn,
        mode: BuildMode::Worker,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };
    let worker_args = command_args(&backend.build_command(&worker_ctx).unwrap());
    assert!(
        contains_adjacent_pair(&worker_args, "--max-turns", "120"),
        "worker mode must pass --max-turns 120: {worker_args:?}"
    );

    let normal_ctx = BuildContext {
        mode: BuildMode::Normal,
        ..worker_ctx
    };
    let normal_args = command_args(&backend.build_command(&normal_ctx).unwrap());
    assert!(
        !normal_args.iter().any(|a| a == "--max-turns"),
        "normal mode must not be affected by member max-turns change: {normal_args:?}"
    );
}

#[test]
fn harness_reserved_myagent_prefix_skips_provider_specific_env() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let mut profile = harness_profile();
    // 恶性形态：算出的前缀 MYAGENT_SEARCH 会撞搜索 key 保留名——必须只走 MYAGENT_* 通用注入
    profile.provider = "myagent-search".to_string();
    profile.endpoint = Some("https://evil.example.test/v1".to_string());
    profile.primary_model = Some("m1".to_string());
    let backend = HarnessBackend {
        profile,
        api_key: Some("llm-key".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let ctx = build_context(&test, "fix the bug");

    let cmd = backend.build_command(&ctx).unwrap();

    assert_eq!(
        env_value(&cmd, "MYAGENT_API_KEY"),
        Some(Some("llm-key".into()))
    );
    assert!(env_value(&cmd, "MYAGENT_SEARCH_API_KEY").is_none());
    assert!(env_value(&cmd, "MYAGENT_SEARCH_BASE_URL").is_none());
    assert!(env_value(&cmd, "MYAGENT_SEARCH_MODEL").is_none());
}

#[test]
fn harness_provider_env_prefix_replaces_hyphens_with_underscores() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let mut profile = harness_profile();
    profile.provider = "glm-x".to_string();
    let backend = HarnessBackend {
        profile,
        api_key: Some("glm-x-key".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let ctx = build_context(&test, "fix the bug");

    let cmd = backend.build_command(&ctx).unwrap();

    assert_eq!(
        env_value(&cmd, "GLM_X_API_KEY"),
        Some(Some("glm-x-key".into()))
    );
}

#[test]
fn harness_no_key_omits_api_key_env() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let profile = harness_profile();
    let env_prefix = profile.provider.to_ascii_uppercase().replace('-', "_");
    let backend = HarnessBackend {
        profile,
        api_key: None,
        search_api_key: None,
        search_backend: None,
    };
    let ctx = build_context(&test, "fix the bug");

    let cmd = backend.build_command(&ctx).unwrap();

    assert_eq!(env_value(&cmd, "MYAGENT_API_KEY"), None);
    assert_eq!(env_value(&cmd, &format!("{env_prefix}_API_KEY")), None);
}

#[test]
fn harness_omits_provider_endpoint_and_model_env_when_unconfigured() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();

    let mut none_profile = harness_profile();
    none_profile.provider = "glm".to_string();
    none_profile.endpoint = None;
    none_profile.primary_model = None;
    let none_backend = HarnessBackend {
        profile: none_profile,
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let none_ctx = build_context(&test, "fix the bug");

    let none_cmd = none_backend.build_command(&none_ctx).unwrap();

    assert_eq!(env_value(&none_cmd, "GLM_BASE_URL"), None);
    assert_eq!(env_value(&none_cmd, "GLM_MODEL"), None);
    assert_eq!(env_value(&none_cmd, "MYAGENT_BASE_URL"), None);
    assert_eq!(env_value(&none_cmd, "MYAGENT_MODEL"), None);

    let mut empty_profile = harness_profile();
    empty_profile.provider = "glm-x".to_string();
    empty_profile.endpoint = Some(String::new());
    empty_profile.primary_model = Some(String::new());
    let empty_backend = HarnessBackend {
        profile: empty_profile,
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let empty_ctx = build_context(&test, "fix the bug");

    let empty_cmd = empty_backend.build_command(&empty_ctx).unwrap();

    assert_eq!(env_value(&empty_cmd, "GLM_X_BASE_URL"), None);
    assert_eq!(env_value(&empty_cmd, "GLM_X_MODEL"), None);
    assert_eq!(env_value(&empty_cmd, "MYAGENT_BASE_URL"), None);
    assert_eq!(env_value(&empty_cmd, "MYAGENT_MODEL"), None);
}

#[test]
fn harness_parsefn_is_harness() {
    let _mode = set_harness_mode_for_test(None);
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: None,
        search_api_key: None,
        search_backend: None,
    };

    assert_eq!(backend.parse_fn(), ParseFn::Harness);
}

#[test]
fn harness_parsefn_is_plan_when_plan_mode_enabled() {
    let _mode = set_harness_mode_for_test(Some("plan"));
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: None,
        search_api_key: None,
        search_backend: None,
    };

    assert_eq!(backend.parse_fn(), ParseFn::HarnessPlan);
}

#[test]
fn harness_build_command_always_passes_read_root_for_pasted_dir() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let ctx = build_context(&test, "no images here");
    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    let pasted_dir = crate::agent::harness_attachments::pasted_dir();
    assert!(pasted_dir.is_dir(), "pasted dir must be created eagerly");
    assert!(
        contains_adjacent_pair(&args, "--read-root", &pasted_dir.to_string_lossy()),
        "{args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--image"),
        "no images in prompt: {args:?}"
    );
}

#[test]
fn harness_build_command_does_not_eagerly_create_or_read_root_workspace_attachments_dir() {
    // 工作区附件目录本就在 `--workspace` 底下可读，第二个 `--read-root` 是冗余
    // 参数——去掉之后也不该再提前创建目录（没粘贴过附件的项目不该平白多出 `.agentloom/`）。
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let ctx = build_context(&test, "no images here");
    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    let workspace_attachments_dir = test.home.join(".agentloom").join("attachments");
    assert!(
        !workspace_attachments_dir.exists(),
        "workspace attachments dir must stay lazily created, not eager"
    );
    assert!(
        !contains_adjacent_pair(
            &args,
            "--read-root",
            &workspace_attachments_dir.to_string_lossy()
        ),
        "second --read-root for the workspace attachments dir is redundant now: {args:?}"
    );
}

#[test]
fn harness_build_command_passes_image_from_workspace_attachments_dir() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let workspace_attachments_dir =
        crate::attachments::dir::attachments_dir_for_workspace(&test.home).unwrap();
    let image = workspace_attachments_dir.join("paste-1-0.png");
    let prompt = format!("看看这张图\n\n![粘贴图片](<{}>)", image.display());
    let ctx = build_context(&test, &prompt);
    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        contains_adjacent_pair(&args, "--image", &image.to_string_lossy()),
        "{args:?}"
    );
}

#[test]
fn harness_build_command_passes_image_for_each_pasted_markdown_image_in_order() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let pasted_dir = crate::agent::harness_attachments::pasted_dir();
    std::fs::create_dir_all(&pasted_dir).unwrap();
    let first = pasted_dir.join("paste-1-0.png");
    let second = pasted_dir.join("paste-2-0.jpg");
    let prompt = format!(
        "看看这两张图\n\n![粘贴图片](<{}>)\n\n![粘贴图片]({})",
        first.display(),
        second.display()
    );
    let ctx = build_context(&test, &prompt);
    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    let image_positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--image")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(image_positions.len(), 2, "{args:?}");
    assert_eq!(args[image_positions[0] + 1], first.to_string_lossy());
    assert_eq!(args[image_positions[1] + 1], second.to_string_lossy());
    assert!(
        image_positions[0] < image_positions[1],
        "images must appear in prompt order: {args:?}"
    );
}

#[test]
fn harness_build_command_skips_non_pasted_and_non_image_and_relative_refs() {
    let _mode = set_harness_mode_for_test(None);
    let test = setup_context();
    let backend = HarnessBackend {
        profile: harness_profile(),
        api_key: Some("k".to_string()),
        search_api_key: None,
        search_backend: None,
    };
    let pasted_dir = crate::agent::harness_attachments::pasted_dir();
    std::fs::create_dir_all(&pasted_dir).unwrap();
    let prompt = format!(
        "Attached file: {}/notes.txt\n```text\nhello\n```\n\n![alt](/elsewhere/x.png)\n![alt]({}/doc.pdf)\n![alt](relative/x.png)",
        pasted_dir.display(),
        pasted_dir.display(),
    );
    let ctx = build_context(&test, &prompt);
    let cmd = backend.build_command(&ctx).unwrap();
    let args = command_args(&cmd);

    assert!(
        !args.iter().any(|a| a == "--image"),
        "no eligible pasted-dir image refs: {args:?}"
    );
}
