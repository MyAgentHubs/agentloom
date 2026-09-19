#![cfg(test)]

use super::*;

#[test]
fn claude_agent_argv_loads_user_project_and_local_settings() {
    let argv = claude_agent_argv();
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--setting-sources" && w[1] == "user,project,local"),
        "{argv:?}"
    );
}

/// D5 续刀防回归：argv 不再带 prompt 正文（超长 prompt 撞 ARG_MAX 会报
/// `Argument list too long (os error 7)`）——`-p` 是布尔开关，不带位置参数，
/// prompt 改走 stdin（见 `agent::spawn_with_stdin_prompt`）。
#[test]
fn claude_agent_argv_never_carries_prompt_body_as_positional_arg() {
    let argv = claude_agent_argv();
    assert_eq!(argv.first().map(String::as_str), Some("-p"), "{argv:?}");
    assert_eq!(
        argv.get(1).map(String::as_str),
        Some("--output-format"),
        "-p 后紧跟下一个 flag，说明 argv 里没有插入 prompt 位置参数：{argv:?}"
    );
}

/// 正文走 stdin 后 `--disable-slash-commands` 下沉进基础 argv（全线共用），防止同一 flag
/// 被调用方再 ad-hoc 加一遍出现两次。
#[test]
fn claude_agent_argv_carries_exactly_one_disable_slash_commands() {
    let argv = claude_agent_argv();
    let count = argv
        .iter()
        .filter(|a| a.as_str() == "--disable-slash-commands")
        .count();
    assert_eq!(count, 1, "{argv:?}");
}

/// HOME 是进程级全局，下面几条用例要临时把它改成 temp dir / 非法值，
/// 与其它同样构造沙箱命令、以及全仓其它改 HOME 的用例并行会互相打架 ——
/// 复用 `worktree::test_home_lock()` 这**唯一**一把全仓 HOME 锁（91+ 处
/// 在用），不要另起一把锁：两把锁互不排斥会导致本模块用例与
/// `worktree`/`lib.rs` 其它改 HOME 的用例并发时相互踩踏（实测 ~21% 失败率）。
fn sandbox_home_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::worktree::test_home_lock()
}

/// 从 `sandbox-exec -p <profile>` 命令里把 profile 取出来。
#[cfg(target_os = "macos")]
fn seatbelt_profile_of(cmd: &Command) -> String {
    assert_eq!(
        cmd.get_program().to_string_lossy(),
        "/usr/bin/sandbox-exec",
        "mac 上必须包 sandbox-exec"
    );
    let args = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    args.windows(2)
        .find(|pair| pair[0] == "-p")
        .map(|pair| pair[1].clone())
        .expect("sandbox-exec 必须带 -p profile")
}

#[test]
fn claude_sandboxed_cmd_in_uses_explicit_worktree() {
    // 不依赖 DB：直接验「显式 wt 变体把 current_dir 设为给定 wt」
    let _serial = sandbox_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("member-wt");
    std::fs::create_dir_all(&wt).unwrap();
    let (cmd, _) = claude_sandboxed_cmd_in(&wt, &[]).expect("build cmd");
    let got = cmd.get_current_dir().map(|p| p.to_path_buf());
    assert_eq!(
        got,
        Some(wt.clone()),
        "命令 workdir 必须是显式传入的 member worktree"
    );
}

#[cfg(unix)]
#[test]
fn claude_sandboxed_cmd_in_sets_augmented_path() {
    let _serial = sandbox_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("member-wt");
    std::fs::create_dir_all(&wt).unwrap();
    let expected =
        crate::agent::augmented_path_for_spawn().map(|path| path.to_string_lossy().into_owned());

    let (cmd, _) = claude_sandboxed_cmd_in(&wt, &[]).expect("build cmd");

    assert_eq!(env_value(&cmd, "PATH").flatten(), expected);
}

#[test]
fn claude_lead_cmd_in_sets_mcp_timeouts() {
    let _serial = sandbox_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("lead-wt");
    std::fs::create_dir_all(&wt).unwrap();
    let expected = mcp_server::CLAUDE_MCP_TIMEOUT_MS.to_string();

    let (cmd, _) = claude_lead_cmd_in(&wt, "hi", &["--mcp-config", "{}"]).expect("build lead cmd");

    assert_eq!(
        env_value(&cmd, "MCP_TOOL_TIMEOUT"),
        Some(Some(expected.clone()))
    );
    assert_eq!(env_value(&cmd, "MCP_TIMEOUT"), Some(Some(expected)));
}

#[test]
fn borrow_lead_cmd_in_sets_borrow_env_and_merged_system_prompt() {
    // L1 回归：borrow lead spawn 的 env（同源 BorrowClaudeBackend）+ argv 形态。
    let _serial = sandbox_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("lead-wt");
    std::fs::create_dir_all(&wt).unwrap();

    let mut profile = agent_profile("borrow-lead-deepseek", true, false);
    profile.provider = "deepseek".to_string();
    profile.access = "borrow".to_string();
    profile.cap_lead = Some("borrow_claude".to_string());
    profile.endpoint = Some("https://api.deepseek.test/anthropic".to_string());

    let identity = agent::borrow_claude_identity_prompt(&profile);
    let system_prompt = format!("{identity}\n\n{LEAD_SYS_V2}");
    let mcp_cfg = "{\"mcpServers\":{}}";

    let (cmd, _) = borrow_lead_cmd_in(
        &profile,
        "borrow-test-key",
        &wt,
        "hi",
        mcp_cfg,
        &system_prompt,
    )
    .expect("build borrow lead cmd");

    // env：与 BorrowClaudeBackend 同源（CLAUDE_CONFIG_DIR / ANTHROPIC_BASE_URL / AUTH_TOKEN）。
    assert_eq!(
        env_value(&cmd, "ANTHROPIC_BASE_URL"),
        Some(Some("https://api.deepseek.test/anthropic".to_string()))
    );
    assert_eq!(
        env_value(&cmd, "ANTHROPIC_AUTH_TOKEN"),
        Some(Some("borrow-test-key".to_string()))
    );
    assert!(
        env_value(&cmd, "CLAUDE_CONFIG_DIR").flatten().is_some(),
        "borrow lead 必须设 CLAUDE_CONFIG_DIR 隔离配置目录"
    );
    // apply_clean_env 必须已跑过、又被 borrow env 覆盖回来——不能被 clean 蒸发掉。
    assert!(
        env_value(&cmd, "ANTHROPIC_BASE_URL").flatten().is_some(),
        "borrow env 不能被后续 apply_clean_env 冲掉"
    );

    let args = command_args(&cmd);
    // MCP timeout env 同 native lead。
    let expected_timeout = mcp_server::CLAUDE_MCP_TIMEOUT_MS.to_string();
    assert_eq!(
        env_value(&cmd, "MCP_TOOL_TIMEOUT"),
        Some(Some(expected_timeout.clone()))
    );
    assert_eq!(env_value(&cmd, "MCP_TIMEOUT"), Some(Some(expected_timeout)));

    // --disable-slash-commands 来自 claude_agent_argv() 基础项（与 BorrowClaudeBackend
    // 一致），必须恰好一条——不能被 borrow_lead_cmd_in 再 ad-hoc 加一遍。
    assert_eq!(
        args.iter()
            .filter(|a| a.as_str() == "--disable-slash-commands")
            .count(),
        1,
        "borrow lead 必须恰好带 1 条 --disable-slash-commands: {args:?}"
    );

    // 恰好一条 --append-system-prompt，且同时含身份片段 + LEAD_SYS_V2 片段（合并、不覆盖）。
    let append_positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, s)| s.as_str() == "--append-system-prompt")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        append_positions.len(),
        1,
        "borrow lead 必须恰好一条 --append-system-prompt: {args:?}"
    );
    let prompt_val = &args[append_positions[0] + 1];
    assert!(
        prompt_val.contains("绝不能自称 Claude"),
        "身份提示片段丢失: {prompt_val}"
    );
    assert!(
        prompt_val.contains("call the dispatch_worker tool"),
        "LEAD_SYS_V2 片段丢失: {prompt_val}"
    );

    // lead 的 MCP/allowedTools/disallowedTools 序列与 native lead 同源（复用 lead_claude_argv_extra）。
    assert!(args.contains(&"--mcp-config".to_string()));
    assert!(args.contains(&"--strict-mcp-config".to_string()));
    let allowed_idx = args
        .iter()
        .position(|s| s == "--allowedTools")
        .expect("--allowedTools missing");
    assert!(args[allowed_idx + 1].contains("mcp__agentloom__dispatch_worker"));
    let disallowed_idx = args
        .iter()
        .position(|s| s == "--disallowedTools")
        .expect("--disallowedTools missing");
    assert_eq!(
        args[disallowed_idx + 1],
        "Write,Edit,MultiEdit,NotebookEdit,Bash"
    );
}

#[test]
fn harness_lead_cmd_in_builds_run_argv_with_mcp_and_disallow_tools() {
    // L3 A1：myagent 队长 spawn argv 断言——纯函数，不真 spawn（软链的旧 sidecar 不含新 flag）。
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(tmp.path());
    let wt = tmp.path().join("lead-wt");
    std::fs::create_dir_all(&wt).unwrap();

    let mut profile = harness_lead_capable_profile("harness-lead-glm");
    profile.provider = "glm".to_string();
    profile.endpoint = Some("https://glm.example.test/v1".to_string());
    profile.primary_model = Some("glm-4.5".to_string());

    let (cmd, _) = harness_lead_cmd_in(
        &profile,
        Some("glm-test-key"),
        Some("search-key"),
        Some("brave"),
        &wt,
        "session-harness-lead",
        "hi",
        "http://127.0.0.1:4321/mcp",
    )
    .expect("build harness lead cmd");

    let args = command_args(&cmd);
    assert_eq!(args.first().map(String::as_str), Some("run"));
    let prompt_path = std::path::PathBuf::from(&args[1]);
    assert!(prompt_path.is_file(), "prompt positional: {args:?}");
    assert_eq!(std::fs::read_to_string(prompt_path).unwrap(), "hi");
    assert!(args.iter().any(|a| a == "--jsonl"), "{args:?}");
    assert!(
        contains_adjacent_pair(&args, "--provider", "glm"),
        "{args:?}"
    );
    assert!(
        contains_adjacent_pair(&args, "--permission", "allow"),
        "{args:?}"
    );
    assert!(
        contains_adjacent_pair(&args, "--disallow-tools", "fs_edit,fs_write,shell_exec"),
        "{args:?}"
    );
    // T4：lead 回合预算放宽到 120（引擎默认 40 轮对「读码+派单+问人」结构性太紧）。
    assert!(
        contains_adjacent_pair(&args, "--max-turns", "120"),
        "{args:?}"
    );
    assert!(
        contains_adjacent_pair(&args, "--mcp-server", "agentloom=http://127.0.0.1:4321/mcp"),
        "{args:?}"
    );
    let append_idx = args
        .iter()
        .position(|s| s == "--append-system-prompt")
        .expect("--append-system-prompt missing");
    assert_eq!(
        args[append_idx + 1],
        LEAD_SYS_V2,
        "harness lead 不合并身份提示，直发 LEAD_SYS_V2 本体"
    );
    assert!(
        args[append_idx + 1].contains("call the dispatch_worker tool"),
        "LEAD_SYS_V2 特征串丢失: {}",
        args[append_idx + 1]
    );
    assert!(args.iter().any(|a| a == "--client-session-id"), "{args:?}");
    assert!(args.iter().any(|a| a == "--workspace"), "{args:?}");
    assert!(args.iter().any(|a| a == "--journal-dir"), "{args:?}");
    // claude 专属参数不应出现在 harness 队长 argv 里。
    assert!(
        !args.iter().any(|a| a == "--mcp-config"),
        "harness lead 不该带 claude 的 --mcp-config: {args:?}"
    );
    assert!(!args.iter().any(|a| a == "--strict-mcp-config"), "{args:?}");

    // env：与 HarnessBackend 同源（apply_harness_provider_env）。
    assert_eq!(
        env_value(&cmd, "GLM_API_KEY"),
        Some(Some("glm-test-key".to_string()))
    );
    assert_eq!(
        env_value(&cmd, "MYAGENT_API_KEY"),
        Some(Some("glm-test-key".to_string()))
    );
    assert_eq!(
        env_value(&cmd, "MYAGENT_SEARCH_API_KEY"),
        Some(Some("search-key".to_string()))
    );
    assert_eq!(
        env_value(&cmd, "MYAGENT_SEARCH_BACKEND"),
        Some(Some("brave".to_string()))
    );
}

#[test]
fn harness_lead_cmd_in_writes_large_prompt_to_app_domain_file() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(tmp.path());
    let wt = tmp.path().join("user-workspace");
    std::fs::create_dir_all(&wt).unwrap();
    let profile = harness_lead_capable_profile("harness-lead-large-prompt");
    let prompt = "x".repeat(2 * 1024 * 1024 + 1);

    let (cmd, _) = harness_lead_cmd_in(
        &profile,
        None,
        None,
        None,
        &wt,
        "session-harness-lead-large-prompt",
        &prompt,
        "http://127.0.0.1:4321/mcp",
    )
    .expect("build harness lead cmd");

    let args = command_args(&cmd);
    assert!(!args.iter().any(|arg| arg == &prompt));
    let prompt_path = std::path::PathBuf::from(&args[1]);
    assert!(prompt_path.is_file(), "prompt positional: {args:?}");
    assert!(prompt_path.starts_with(crate::worktree::journals_dir()));
    assert!(!prompt_path.starts_with(&wt));
    assert_eq!(std::fs::read(&prompt_path).unwrap(), prompt.as_bytes());
}

/// T3：lead spawn 路径（`harness_lead_cmd_in`）与 `HarnessBackend` 共用
/// `apply_harness_provider_env`——`agents.api_timeout_ms` 同样要能到达 lead 子进程 env。
#[test]
fn harness_lead_cmd_in_maps_api_timeout_ms_to_myagent_timeout_secs() {
    let _home_lock = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(tmp.path());
    let wt = tmp.path().join("lead-wt");
    std::fs::create_dir_all(&wt).unwrap();

    // a) 600000ms → 600s。
    let mut profile_a = harness_lead_capable_profile("harness-lead-timeout-a");
    profile_a.api_timeout_ms = Some(600_000);
    let (cmd_a, _) = harness_lead_cmd_in(
        &profile_a,
        Some("k"),
        None,
        None,
        &wt,
        "session-timeout-a",
        "hi",
        "http://127.0.0.1:4321/mcp",
    )
    .expect("build harness lead cmd");
    assert_eq!(
        env_value(&cmd_a, "MYAGENT_TIMEOUT_SECS"),
        Some(Some("600".to_string()))
    );

    // b) None → 不设该变量。
    let mut profile_b = harness_lead_capable_profile("harness-lead-timeout-b");
    profile_b.api_timeout_ms = None;
    let (cmd_b, _) = harness_lead_cmd_in(
        &profile_b,
        Some("k"),
        None,
        None,
        &wt,
        "session-timeout-b",
        "hi",
        "http://127.0.0.1:4321/mcp",
    )
    .expect("build harness lead cmd");
    assert_eq!(env_value(&cmd_b, "MYAGENT_TIMEOUT_SECS"), None);

    // c) 边界 500ms → 向上取整 + 下限 1 → 1s。
    let mut profile_c = harness_lead_capable_profile("harness-lead-timeout-c");
    profile_c.api_timeout_ms = Some(500);
    let (cmd_c, _) = harness_lead_cmd_in(
        &profile_c,
        Some("k"),
        None,
        None,
        &wt,
        "session-timeout-c",
        "hi",
        "http://127.0.0.1:4321/mcp",
    )
    .expect("build harness lead cmd");
    assert_eq!(
        env_value(&cmd_c, "MYAGENT_TIMEOUT_SECS"),
        Some(Some("1".to_string()))
    );
}

/// 接线锁 ①：mac 上 HOME 缺失 / 相对路径必须 fail-closed。
/// 拆掉 `sandbox::canonicalize_sandbox_home` 这层接线、或把平台守卫写反，这条就红。
#[cfg(target_os = "macos")]
#[test]
fn claude_sandboxed_cmd_in_fails_closed_on_unusable_home() {
    let _serial = sandbox_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();

    for bad_home in ["", "relative/home"] {
        let _home_guard = TestHomeGuard::set(std::path::Path::new(bad_home));
        assert!(
            claude_sandboxed_cmd_in(&wt, &[]).is_err(),
            "HOME={bad_home:?} 时 Seatbelt app 域 deny 会退化成相对 subpath，必须 fail-closed"
        );
    }
}

/// 接线锁 ②（守卫另一侧）：非 mac 不构造 profile，HOME 与沙箱无关，不得因它报错。
/// 平台守卫写反 → 非 mac 上也去校验 HOME，这条就红。
#[cfg(not(target_os = "macos"))]
#[test]
fn claude_sandboxed_cmd_in_ignores_home_off_macos() {
    let _serial = sandbox_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();

    let _home_guard = TestHomeGuard::set(std::path::Path::new(""));
    assert!(
        claude_sandboxed_cmd_in(&wt, &[]).is_ok(),
        "非 mac 走 wrap 的 None 降级分支，不该被 HOME 卡住"
    );
}

/// 接线锁 ③：真正落到 `-p` 里的 profile 必须以 canonical HOME 拒写 `~/.agentloom`。
#[cfg(target_os = "macos")]
#[test]
fn claude_sandboxed_cmd_in_profile_denies_canonical_agentloom_dir() {
    let _serial = sandbox_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("plain-project");
    std::fs::create_dir_all(&wt).unwrap();

    let (cmd, _) = claude_sandboxed_cmd_in(&wt, &[]).expect("build cmd");
    let profile = seatbelt_profile_of(&cmd);

    let canonical_home = std::fs::canonicalize(home.path()).unwrap();
    let expected = format!(
        "(deny file-write* (subpath \"{}/.agentloom\"))",
        canonical_home.display()
    );
    assert!(
        profile.contains(&expected),
        "app 域写拒绝必须用 canonical HOME：{expected}\nprofile：{profile}"
    );
    assert!(
        !profile.contains("(allow file-write* (subpath"),
        "普通项目不在 deny 域内，不该发逐目录 allow：{profile}"
    );
}

/// P0 回归锁：开箱即用的默认项目 `~/.agentloom/local/default` 就落在 deny 域里，
/// profile 必须在所有 deny 之后补一条 canonical 工作区 allow，否则 agent 能读不能写。
#[cfg(target_os = "macos")]
#[test]
fn claude_sandboxed_cmd_in_reallows_workspace_inside_agentloom_dir() {
    let _serial = sandbox_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let wt = home.path().join(".agentloom/local/default");
    std::fs::create_dir_all(&wt).unwrap();

    let (cmd, _) = claude_sandboxed_cmd_in(&wt, &[]).expect("build cmd");
    let profile = seatbelt_profile_of(&cmd);

    let canonical_wt = std::fs::canonicalize(&wt).unwrap();
    let expected = format!(
        "(allow file-write* (subpath \"{}\"))",
        canonical_wt.display()
    );
    assert!(
        profile.contains(&expected),
        "默认项目落在 deny 域内，必须被尾部精确放行：{expected}\nprofile：{profile}"
    );
    let last_deny = profile
        .rfind("(deny file-write*")
        .expect("app 域写拒绝规则必须还在");
    assert!(
        last_deny < profile.find(&expected).unwrap(),
        "Seatbelt 末匹配语义要求工作区 allow 排在所有 deny 之后：{profile}"
    );
}

#[test]
fn apply_augmented_spawn_path_sets_command_path_when_available() {
    let mut cmd = Command::new("claude");

    apply_augmented_spawn_path(&mut cmd, Some(std::ffi::OsString::from("/enhanced/bin")));

    assert_eq!(
        env_value(&cmd, "PATH"),
        Some(Some("/enhanced/bin".to_string()))
    );
}

#[test]
fn log_claude_bin_appends_resolved_path_to_session_log() {
    let home = TestHome::new();

    log_claude_bin("resolved-bin-session", "/enhanced/bin/claude");

    let logged =
        std::fs::read_to_string(home.path.join(".agentloom/logs/resolved-bin-session.log"))
            .unwrap();
    assert_eq!(logged, "claude-bin: /enhanced/bin/claude\n");
}
