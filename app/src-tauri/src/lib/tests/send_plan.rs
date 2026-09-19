#![cfg(test)]

use super::*;

struct EnvOverride {
    key: &'static str,
    old: Option<std::ffi::OsString>,
}

impl EnvOverride {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        match &self.old {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn build_command_with_home_retry<B: AgentBackend>(
    backend: &B,
    ctx: &BuildContext<'_>,
    home: &TestHome,
) -> Command {
    let mut last_err = None;
    for _ in 0..10 {
        home.apply();
        match backend.build_command(ctx) {
            Ok(cmd) => return cmd,
            Err(err) if err.contains("Operation not permitted") => {
                last_err = Some(err);
                std::thread::yield_now();
            }
            Err(err) => panic!("backend build_command 失败：{err}"),
        }
    }
    panic!(
        "backend build_command 多次遇到 HOME 并发覆盖：{}",
        last_err.unwrap_or_else(|| "unknown".to_string())
    );
}

fn build_send_plan_with_home_retry(
    conn: &rusqlite::Connection,
    session_id: &str,
    agent_id: &str,
    message: &str,
    reasoning_tier: Option<&str>,
    key_store: &dyn crate::keychain::KeyStore,
    home: &TestHome,
) -> SendPlan {
    let mut last_err = None;
    for _ in 0..10 {
        home.apply();
        match build_send_plan(
            conn,
            session_id,
            "test-run",
            agent_id,
            message,
            reasoning_tier,
            &[],
            key_store,
            Locale::Zh,
        ) {
            Ok(plan) => return plan,
            Err(err) if err.contains("Operation not permitted") => {
                last_err = Some(err);
                std::thread::yield_now();
            }
            Err(err) => panic!("build_send_plan 失败：{err}"),
        }
    }
    panic!(
        "build_send_plan 多次遇到 HOME 并发覆盖：{}",
        last_err.unwrap_or_else(|| "unknown".to_string())
    );
}

#[test]
fn a1a_normal_native_claude_has_no_tools_baseline() {
    let home = TestHome::new();
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-a1a", "x", "local-default", "local").unwrap();
    let backend = NativeBackend {
        provider: "claude".into(),
        primary_model: None,
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: "s-a1a",
        run_id: "test-run",
        wt: &home.path,
        conn: &c,
        mode: agent::BuildMode::Normal,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };
    let cmd = build_command_with_home_retry(&backend, &ctx, &home);
    let args = command_args(&cmd);
    assert!(
        !args.iter().any(|a| a == "--tools"),
        "A1a：plumbing 不该引入任何 --tools：{args:?}"
    );
    home.apply();
}

#[test]
fn a1b_worker_native_claude_gets_allowlist_and_oneshot() {
    let home = TestHome::new();
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-a1b", "x", "local-default", "local").unwrap();
    let backend = NativeBackend {
        provider: "claude".into(),
        primary_model: None,
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: "s-a1b",
        run_id: "test-run",
        wt: &home.path,
        conn: &c,
        mode: agent::BuildMode::Worker,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };
    let cmd = build_command_with_home_retry(&backend, &ctx, &home);
    let args = command_args(&cmd);
    assert!(
        args.iter().any(|a| a == "--tools"),
        "worker 必须带 --tools：{args:?}"
    );
    for t in ["Read", "Edit", "Write", "Bash", "Glob", "Grep"] {
        assert!(args.iter().any(|a| a == t), "白名单漏 {t}：{args:?}");
    }
    for esc in [
        "ScheduleWakeup",
        "CronCreate",
        "PushNotification",
        "RemoteTrigger",
    ] {
        assert!(
            !args.iter().any(|a| a == esc),
            "逃逸工具 {esc} 不该进白名单：{args:?}"
        );
    }
    assert!(
        contains_adjacent_pair(
            &args,
            "--append-system-prompt",
            crate::WORKER_ONESHOT_PROMPT
        ) || args.iter().any(|a| a.contains("一次性执行器")),
        "worker 必须带一次性软框：{args:?}"
    );
    home.apply();
}

#[test]
fn native_codex_backend_uses_workspace_write_and_explicit_workdir() {
    let home = TestHome::new();
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-codex-plan", "x", "local-default", "local").unwrap();
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: "s-codex-plan",
        run_id: "test-run",
        wt: &home.path,
        conn: &c,
        mode: agent::BuildMode::Normal,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };

    let cmd = build_command_with_home_retry(&backend, &ctx, &home);
    let args = command_args(&cmd);

    assert_eq!(backend.parse_fn(), ParseFn::Codex);
    assert!(
        contains_adjacent_pair(&args, "exec", "--json"),
        "Codex backend 应使用 exec --json：{args:?}"
    );
    assert!(
        args.iter().any(|arg| arg == "--skip-git-repo-check"),
        "Codex backend 应跳过 git repo 检查：{args:?}"
    );
    assert!(
        args.iter().any(|arg| arg == "workspace-write"),
        "Codex backend 必须用 --sandbox workspace-write：{args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg == "read-only"),
        "Codex backend 不应使用 read-only sandbox：{args:?}"
    );
    // -a never 必须在 exec 前（本机 codex-cli 0.135.0：codex exec -a never 报 unexpected argument）
    let a_pos = args.iter().position(|arg| arg == "-a").expect("应有 -a");
    let never_pos = args
        .iter()
        .position(|arg| arg == "never")
        .expect("应有 never");
    let exec_pos = args
        .iter()
        .position(|arg| arg == "exec")
        .expect("应有 exec");
    assert_eq!(never_pos, a_pos + 1, "-a 后应紧跟 never：{args:?}");
    assert!(
        a_pos < exec_pos,
        "Codex backend -a 必须在 exec 之前：{args:?}"
    );

    home.apply();
    assert_eq!(
        cmd.get_current_dir(),
        Some(home.path.as_path()),
        "Codex backend workdir 必须是显式 ctx.wt"
    );
}

#[test]
fn summarizer_codex_command_uses_read_only_sandbox() {
    let home = TestHome::new();
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-summarizer-codex", "x", "local-default", "local").unwrap();
    let backend = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };
    let ctx = BuildContext {
        prompt: "summarize this session",
        session_id: "s-summarizer-codex",
        run_id: "test-run",
        wt: &home.path,
        conn: &c,
        mode: agent::BuildMode::Summarize,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };

    let cmd = build_command_with_home_retry(&backend, &ctx, &home);
    let args = command_args(&cmd);

    assert!(
        args.iter().any(|arg| arg == "read-only"),
        "Summarize mode codex 必须用 --sandbox read-only：{args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg == "workspace-write"),
        "Summarize mode codex 不应用 workspace-write：{args:?}"
    );
    home.apply();
}

#[test]
fn summarizer_claude_command_is_read_only_allowlist() {
    let home = TestHome::new();
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-summarizer-claude", "x", "local-default", "local").unwrap();
    let backend = NativeBackend {
        provider: "claude".into(),
        primary_model: None,
    };
    let ctx = BuildContext {
        prompt: "summarize this session",
        session_id: "s-summarizer-claude",
        run_id: "test-run",
        wt: &home.path,
        conn: &c,
        mode: agent::BuildMode::Summarize,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };
    let cmd = build_command_with_home_retry(&backend, &ctx, &home);
    let args = command_args(&cmd);
    let tools_idx = args
        .iter()
        .position(|a| a == "--tools")
        .unwrap_or_else(|| panic!("Summarize mode claude 必须含 --tools：{args:?}"));
    let tools_val = &args[tools_idx + 1];
    assert!(
        tools_val.contains("Read"),
        "Summarize --tools 必须含 Read：{tools_val}"
    );
    assert!(
        !tools_val.contains("Write"),
        "Summarize --tools 不应含 Write：{tools_val}"
    );
    assert!(
        !tools_val.contains("Bash"),
        "Summarize --tools 不应含 Bash：{tools_val}"
    );
    assert!(
        !tools_val.contains("Agent"),
        "Summarize --tools 不应含 Agent：{tools_val}"
    );
    assert!(
        !tools_val.contains("Task"),
        "Summarize --tools 不应含 Task：{tools_val}"
    );
    home.apply();
}

#[test]
fn summarizer_borrow_command_is_read_only_allowlist() {
    let home = TestHome::new();
    let _proxy_off = EnvOverride::set("AGENTLOOM_DEEPSEEK_PROXY", "0");
    let c = crate::test_support::mem_db();
    db::seed_builtin_agents(&c).unwrap();
    db::create_session(&c, "s-summarizer-borrow", "x", "local-default", "local").unwrap();
    let profile = agent_profile("deepseek", true, true);
    let backend = BorrowClaudeBackend {
        profile: profile.clone(),
        api_key: "sk-test".to_string(),
    };
    let ctx = BuildContext {
        prompt: "summarize this session",
        session_id: "s-summarizer-borrow",
        run_id: "test-run",
        wt: &home.path,
        conn: &c,
        mode: agent::BuildMode::Summarize,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };
    let cmd = build_command_with_home_retry(&backend, &ctx, &home);
    let args = command_args(&cmd);
    let tools_idx = args
        .iter()
        .position(|a| a == "--tools")
        .unwrap_or_else(|| panic!("Summarize borrow 必须含 --tools：{args:?}"));
    let tools_val = &args[tools_idx + 1];
    assert!(
        tools_val.contains("Read"),
        "Summarize borrow --tools 必须含 Read：{tools_val}"
    );
    assert!(
        !tools_val.contains("Write"),
        "Summarize borrow --tools 不应含 Write：{tools_val}"
    );
    assert!(
        !tools_val.contains("Bash"),
        "Summarize borrow --tools 不应含 Bash：{tools_val}"
    );
    assert!(
        !tools_val.contains("Agent"),
        "Summarize borrow --tools 不应含 Agent：{tools_val}"
    );
    assert!(
        !tools_val.contains("Task"),
        "Summarize borrow --tools 不应含 Task：{tools_val}"
    );
    home.apply();
}

#[test]
fn borrow_claude_backend_uses_sandboxed_cmd_disables_skills_and_clean_env() {
    let home = TestHome::new();
    let _proxy_off = EnvOverride::set("AGENTLOOM_DEEPSEEK_PROXY", "0");
    let c = crate::test_support::mem_db();
    db::seed_builtin_agents(&c).unwrap();
    db::create_session(&c, "s-borrow-plan", "x", "local-default", "local").unwrap();
    let profile = agent_profile("deepseek", true, true);
    let backend = BorrowClaudeBackend {
        profile: profile.clone(),
        api_key: "sk-test".to_string(),
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: "s-borrow-plan",
        run_id: "test-run",
        wt: &home.path,
        conn: &c,
        mode: agent::BuildMode::Normal,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };

    let cmd = build_command_with_home_retry(&backend, &ctx, &home);
    let args = command_args(&cmd);

    assert_eq!(backend.parse_fn(), ParseFn::Claude);
    assert!(
        contains_adjacent_pair(&args, "--output-format", "stream-json"),
        "Borrow backend 应复用 claude_sandboxed_cmd 的 stream-json argv：{args:?}"
    );
    assert_eq!(
            args.iter()
                .filter(|arg| arg.as_str() == "--disable-slash-commands")
                .count(),
            1,
            "Borrow backend 必须恰好带 1 条 --disable-slash-commands（禁 slash commands / skills 注入，不能重复）：{args:?}"
        );
    assert!(
        env_value(&cmd, "CLAUDE_CONFIG_DIR")
            .and_then(|v| v)
            .is_some(),
        "Borrow backend 必须设置独立 CLAUDE_CONFIG_DIR"
    );
    assert_eq!(
        env_value(&cmd, "ANTHROPIC_BASE_URL"),
        Some(profile.endpoint.clone())
    );
    assert_eq!(
        env_value(&cmd, "ANTHROPIC_AUTH_TOKEN"),
        Some(Some("sk-test".to_string()))
    );
    assert!(
        !matches!(env_value(&cmd, "ANTHROPIC_API_KEY"), Some(Some(_))),
        "bearer 借壳不应保留 ANTHROPIC_API_KEY"
    );
    assert_eq!(
        env_value(&cmd, "CLAUDE_CODE_DISABLE_THINKING"),
        Some(None),
        "Borrow backend 应清掉宿主 thinking env，除非 profile 显式重设"
    );
}

#[test]
fn a1c_worker_borrow_gets_allowlist_and_keeps_identity() {
    let home = TestHome::new();
    let _proxy_off = EnvOverride::set("AGENTLOOM_DEEPSEEK_PROXY", "0");
    let c = crate::test_support::mem_db();
    db::seed_builtin_agents(&c).unwrap();
    db::create_session(&c, "s-wbor", "x", "local-default", "local").unwrap();
    let profile = agent_profile("deepseek", true, true);
    let backend = BorrowClaudeBackend {
        profile,
        api_key: "sk-test".into(),
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: "s-wbor",
        run_id: "test-run",
        wt: &home.path,
        conn: &c,
        mode: agent::BuildMode::Worker,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };
    let cmd = build_command_with_home_retry(&backend, &ctx, &home);
    let args = command_args(&cmd);
    assert!(
        args.iter().any(|a| a == "--tools"),
        "borrow worker 必须带白名单：{args:?}"
    );
    assert!(
        args.iter()
            .any(|a| a.contains("绝不能自称 Claude") && a.contains("一次性执行器")),
        "借壳 identity 与一次性软框应拼在同一 system prompt（identity 不被覆盖）：{args:?}"
    );
    home.apply();
}

#[test]
fn a1c_normal_borrow_has_no_tools_and_no_oneshot() {
    let home = TestHome::new();
    let _proxy_off = EnvOverride::set("AGENTLOOM_DEEPSEEK_PROXY", "0");
    let c = crate::test_support::mem_db();
    db::seed_builtin_agents(&c).unwrap();
    db::create_session(&c, "s-nbor", "x", "local-default", "local").unwrap();
    let profile = agent_profile("deepseek", true, true);
    let backend = BorrowClaudeBackend {
        profile,
        api_key: "sk-test".into(),
    };
    let ctx = BuildContext {
        prompt: "hi",
        session_id: "s-nbor",
        run_id: "test-run",
        wt: &home.path,
        conn: &c,
        mode: agent::BuildMode::Normal,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    };
    let cmd = build_command_with_home_retry(&backend, &ctx, &home);
    let args = command_args(&cmd);
    assert!(
        !args.iter().any(|a| a == "--tools"),
        "Normal borrow 绝不能被 --tools 削：{args:?}"
    );
    assert!(
        !args.iter().any(|a| a.contains("一次性执行器")),
        "Normal borrow 不该带一次性软框：{args:?}"
    );
    assert!(
        args.iter().any(|a| a.contains("绝不能自称 Claude")),
        "Normal borrow 仍应保留 identity：{args:?}"
    );
    home.apply();
}

#[test]
fn send_plan_missing_key_errs_no_ledger_no_message() {
    let c = crate::test_support::mem_db();
    db::seed_builtin_agents(&c).unwrap();
    let profile = agent_profile("deepseek", false, false);
    db::upsert_agent(&c, &profile).unwrap();
    db::create_session(&c, "s-missing-key", "x", "local-default", "local").unwrap();
    let store = FakeKeyStore::default();

    let err = match build_send_plan(
        &c,
        "s-missing-key",
        "test-run",
        "deepseek",
        "hi",
        None,
        &[],
        &store,
        Locale::Zh,
    ) {
        Ok(_) => panic!("缺 key 的 borrow agent 不应生成 send plan"),
        Err(err) => err,
    };

    assert_eq!(err, "AL_ERR:agent.missingApiKey");
    let message_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 's-missing-key'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(message_count, 0, "前置失败不应写 user message");
    let run_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM run_commits WHERE session_id = 's-missing-key'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(run_count, 0, "前置失败不应写 run ledger");
    assert_eq!(
        db::get_git_state(&c, "s-missing-key").unwrap(),
        "clean",
        "前置失败不应置 git_state=running"
    );
}

#[test]
fn send_plan_routes_native_claude() {
    let home = TestHome::new();
    let c = crate::test_support::mem_db();
    db::seed_builtin_agents(&c).unwrap();
    db::create_session(&c, "s-native-claude", "x", "local-default", "local").unwrap();
    let store = FakeKeyStore::default();

    let plan = build_send_plan_with_home_retry(
        &c,
        "s-native-claude",
        "claude",
        "hello",
        None,
        &store,
        &home,
    );

    assert_eq!(plan.parse_fn, ParseFn::Claude);
    assert_eq!(plan.agent_id, "claude");
    assert_eq!(plan.name_snapshot, "Claude");
    assert_eq!(plan.profile.provider, "claude");
    assert_eq!(plan.prompt, "hello");
    assert!(
        plan.wt.exists(),
        "send plan 应提前解析并确保 session workspace"
    );
}

#[test]
fn send_plan_reasoning_override_reaches_borrow_runtime_command() {
    let home = TestHome::new();
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-borrow-reasoning", "x", "local-default", "local").unwrap();
    let store = FakeKeyStore::default();
    let mut profile = agent_profile("reasoner", true, false);
    profile.reasoning_default = "high".into();
    db::upsert_agent(&c, &profile).unwrap();
    store.set(&profile.id, "sk-test").unwrap();

    let plan = build_send_plan_with_home_retry(
        &c,
        "s-borrow-reasoning",
        "reasoner",
        "hello",
        Some("low"),
        &store,
        &home,
    );

    assert_eq!(
        env_value(&plan.command, "CLAUDE_CODE_EFFORT_LEVEL"),
        Some(Some("low".to_string()))
    );
}

#[test]
fn send_message_requires_agent_id() {
    let err = require_agent_id(String::new()).unwrap_err();

    assert_eq!(err, "AL_ERR:agent.missingId");
}
