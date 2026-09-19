#![cfg(test)]

use super::*;
use crate::{db, test_support};
use std::ffi::{OsStr, OsString};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

static HARNESS_MODE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static CHECKPOINT_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct HarnessModeGuard {
    old_mode: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for HarnessModeGuard {
    fn drop(&mut self) {
        match &self.old_mode {
            Some(mode) => std::env::set_var("MYAGENT_APP_HARNESS_MODE", mode),
            None => std::env::remove_var("MYAGENT_APP_HARNESS_MODE"),
        }
    }
}

fn set_harness_mode_for_test(mode: Option<&str>) -> HarnessModeGuard {
    let lock = HARNESS_MODE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap();
    let old_mode = std::env::var_os("MYAGENT_APP_HARNESS_MODE");
    match mode {
        Some(mode) => std::env::set_var("MYAGENT_APP_HARNESS_MODE", mode),
        None => std::env::remove_var("MYAGENT_APP_HARNESS_MODE"),
    }
    HarnessModeGuard {
        old_mode,
        _lock: lock,
    }
}

struct CheckpointEnvGuard {
    old_endpoint: Option<OsString>,
    old_token: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for CheckpointEnvGuard {
    fn drop(&mut self) {
        match &self.old_endpoint {
            Some(value) => std::env::set_var(crate::checkpoint_hook::ENDPOINT_ENV, value),
            None => std::env::remove_var(crate::checkpoint_hook::ENDPOINT_ENV),
        }
        match &self.old_token {
            Some(value) => std::env::set_var(crate::checkpoint_hook::TOKEN_ENV, value),
            None => std::env::remove_var(crate::checkpoint_hook::TOKEN_ENV),
        }
    }
}

fn set_checkpoint_envs_for_test(endpoint: Option<&str>, token: Option<&str>) -> CheckpointEnvGuard {
    let lock = CHECKPOINT_ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap();
    let old_endpoint = std::env::var_os(crate::checkpoint_hook::ENDPOINT_ENV);
    let old_token = std::env::var_os(crate::checkpoint_hook::TOKEN_ENV);
    match endpoint {
        Some(value) => std::env::set_var(crate::checkpoint_hook::ENDPOINT_ENV, value),
        None => std::env::remove_var(crate::checkpoint_hook::ENDPOINT_ENV),
    }
    match token {
        Some(value) => std::env::set_var(crate::checkpoint_hook::TOKEN_ENV, value),
        None => std::env::remove_var(crate::checkpoint_hook::TOKEN_ENV),
    }
    CheckpointEnvGuard {
        old_endpoint,
        old_token,
        _lock: lock,
    }
}

/// 从 `idx` 往前找最近一个 `fn ` 关键字，取其后的标识符——用来给一处裸 `.spawn()` 定位
/// 「它在哪个函数体里」。这是文本启发式，不是 AST 作用域分析：足够给白名单当身份锚点，
/// 不足以当安全边界（详见 `agent_backend_commands_spawn_via_stdin_prompt_helper` 的白名单
/// 注释，每条例外都已人工核实附近没有会误命中的嵌套 `fn `/注释）。
fn enclosing_fn_name(text: &str, idx: usize) -> Option<String> {
    let prefix = &text[..idx];
    let fn_idx = prefix.rfind("fn ")?;
    let after = &prefix[fn_idx + 3..];
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// stdin 刀 P1-1：钉住「claude/codex/myagent 子进程一律走 `spawn_with_stdin_prompt`，不得裸
/// `Command::spawn()`」——这是本刀新设的统一口（见本文件顶部 `spawn_with_stdin_prompt` 文档
/// 注释），谁绕过它谁就悄悄把 argv/stdin 决策权收窄回旧路径，可能重新踩回 ARG_MAX 或 stdin
/// 语义不一致的坑。用源码文本扫描而非运行时断言：新增的裸 spawn 调用点在改动当下就会让这条
/// 测试变红，而不是等到真的撞见超长 prompt 才发现。
///
/// 用精确字面量 `.spawn()`（零参数、紧跟右括号）而不是宽泛的 `.spawn(` 子串：`Command::spawn`
/// 恰好零参，而 `thread::spawn(closure)` / `Builder::spawn(closure)` / `scope.spawn(closure)`
/// 都带闭包实参，字面量层面天然区分，不需要额外排除线程 spawn 的分支。
///
/// 白名单——已逐条核实身份、不是 agent CLI 子进程，允许留在统一口之外：
/// - `agent.rs::spawn_with_stdin_prompt_ack` —— 本身就是统一口的实现，不能自己调自己。
/// - `agent.rs::path_from_login_shell` —— 探测用户 login shell 的真实 PATH，不是 agent 进程。
/// - `detect.rs::query_registry_value` —— Windows `reg query` 注册表探针。
/// - `github.rs::command_output_with_timeout` —— git/gh 命令的通用超时执行器。
/// - `lib.rs::windows_taskkill_tree` —— Windows `taskkill` 树杀探针。
/// - `worktree.rs::reject_ignored_exact_paths` —— `git check-ignore` 校验。
///
/// 若未来新增一处裸 spawn 且确认不是 agent CLI 子进程，往这张表加一行并写清身份；
/// 若是 agent CLI 子进程，改走 `spawn_with_stdin_prompt`，不要加白名单。
#[test]
fn agent_backend_commands_spawn_via_stdin_prompt_helper() {
    const ALLOWLIST: &[(&str, &str)] = &[
        ("agent.rs", "spawn_with_stdin_prompt_ack"),
        ("agent.rs", "path_from_login_shell"),
        ("detect.rs", "query_registry_value"),
        ("github.rs", "command_output_with_timeout"),
        ("lib.rs", "windows_taskkill_tree"),
        ("worktree.rs", "reject_ignored_exact_paths"),
    ];

    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&src_dir)
        .expect("read src dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("rs"))
        .collect();
    entries.sort();

    for path in entries {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("utf8 file name")
            .to_string();
        let source = std::fs::read_to_string(&path).expect("read source file");
        // 内联 `mod tests {` 之后是测试模块（含 fixture 用的裸 spawn），不受本规则约束。
        // 外提后 src/*.rs 只留下 `mod tests;` 声明，测试 fixture 位于子目录；
        // 未匹配到内联测试模块时，仍完整扫描当前文件。
        let production = match source.find("\nmod tests {") {
            Some(idx) => &source[..idx],
            None => source.as_str(),
        };

        let mut search_from = 0;
        while let Some(rel_idx) = production[search_from..].find(".spawn()") {
            let idx = search_from + rel_idx;
            search_from = idx + ".spawn()".len();
            let enclosing_fn = enclosing_fn_name(production, idx);
            let allowed = ALLOWLIST.iter().any(|(allowed_file, allowed_fn)| {
                *allowed_file == file_name.as_str() && enclosing_fn.as_deref() == Some(*allowed_fn)
            });
            if allowed {
                continue;
            }
            let line_no = production[..idx].matches('\n').count() + 1;
            violations.push(format!(
                "{file_name}:{line_no}（函数 {enclosing_fn:?}）出现裸 .spawn()，须改走 \
                 agent::spawn_with_stdin_prompt；若确认不是 agent CLI 子进程，把它加进本测试的\
                 白名单并写明身份"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "发现未走 spawn_with_stdin_prompt 的裸 Command spawn：\n{}",
        violations.join("\n")
    );
}

#[test]
fn sidecar_exit_error_truth_table() {
    assert!(sidecar_exit_error(false, false, false, false, false));
    assert!(!sidecar_exit_error(true, false, false, false, false));
    assert!(!sidecar_exit_error(false, false, false, true, false));
    assert!(!sidecar_exit_error(false, false, false, false, true));

    // 已发 Blocked / NeedsDecision 后，非零退出不再叠加通用 Error（诚实终态优先）。
    assert!(!sidecar_exit_error(false, true, false, false, false));
    assert!(!sidecar_exit_error(false, false, true, false, false));
}

struct TestContext {
    conn: rusqlite::Connection,
    session_id: String,
    _home_guard: tempfile::TempDir,
    home: std::path::PathBuf,
    old_home: Option<OsString>,
    _home_lock: MutexGuard<'static, ()>,
}

impl Drop for TestContext {
    fn drop(&mut self) {
        match &self.old_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn build_context<'a>(ctx: &'a TestContext, prompt: &'a str) -> BuildContext<'a> {
    build_context_for_mode(ctx, prompt, BuildMode::Normal)
}

fn build_context_for_mode<'a>(
    ctx: &'a TestContext,
    prompt: &'a str,
    mode: BuildMode,
) -> BuildContext<'a> {
    BuildContext {
        prompt,
        session_id: &ctx.session_id,
        run_id: "test-run",
        wt: &ctx.home,
        conn: &ctx.conn,
        mode,
        locale: crate::Locale::Zh,
        reasoning_tier: None,
        criteria: &[],
    }
}

fn setup_context() -> TestContext {
    let home_lock = crate::worktree::test_home_lock();
    let old_home = std::env::var_os("HOME");
    let (home_guard, home) = test_support::tmp_root();
    std::env::set_var("HOME", &home);
    let conn = test_support::mem_db();
    let session_id = format!("s-agent-{}", std::process::id());
    db::create_session(&conn, &session_id, "agent", "local-default", "local").unwrap();
    TestContext {
        conn,
        session_id,
        _home_guard: home_guard,
        home,
        old_home,
        _home_lock: home_lock,
    }
}

fn command_args(cmd: &Command) -> Vec<String> {
    cmd.get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn harness_prompt_path(args: &[String]) -> PathBuf {
    let path = PathBuf::from(args.get(1).expect("harness prompt path positional missing"));
    assert!(
        path.is_file(),
        "harness prompt positional must be a file: {args:?}"
    );
    path
}

fn contains_adjacent_pair(args: &[String], left: &str, right: &str) -> bool {
    args.windows(2)
        .any(|window| window[0] == left && window[1] == right)
}

fn write_codex_config(home: &std::path::Path, contents: &str) {
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(codex_dir.join("config.toml"), contents).unwrap();
}

fn assert_codex_model_before_exec(args: &[String], model: &str) {
    assert!(
        contains_adjacent_pair(args, "-m", model),
        "expected -m {model} in args: {args:?}"
    );
    let model_idx = args
        .windows(2)
        .position(|window| window[0] == "-m" && window[1] == model)
        .expect("model pair should exist");
    let exec_idx = args
        .iter()
        .position(|arg| arg == "exec")
        .expect("expected exec in args");
    assert!(
        model_idx < exec_idx,
        "expected -m {model} before exec in args: {args:?}"
    );
}

fn assert_codex_approval_never_before_exec(args: &[String]) {
    assert!(
        contains_adjacent_pair(args, "-a", "never"),
        "expected -a never in args: {args:?}"
    );
    let approval_idx = args
        .windows(2)
        .position(|window| window[0] == "-a" && window[1] == "never")
        .expect("approval pair should exist");
    let exec_idx = args
        .iter()
        .position(|arg| arg == "exec")
        .expect("expected exec in args");
    assert!(
        approval_idx < exec_idx,
        "expected -a never before exec in args: {args:?}"
    );
}

fn borrow_profile() -> db::AgentProfile {
    db::AgentProfile {
        id: "borrow-agent".to_string(),
        name: "Borrow Agent".to_string(),
        access: "borrow".to_string(),
        provider: "compat".to_string(),
        primary_model: Some("m".to_string()),
        endpoint: Some("https://example.test/anthropic".to_string()),
        auth_mode: Some("bearer".to_string()),
        model_opus: None,
        model_sonnet: None,
        model_haiku: None,
        model_subagent: None,
        reasoning_default: "auto".to_string(),
        max_output_tokens: None,
        api_timeout_ms: None,
        compat_disable_betas: false,
        compat_disable_nonessential: false,
        compat_disable_thinking: false,
        compat_proxy: None,
        custom_headers: None,
        extra_body: None,
        cap_reasoning: None,
        cap_computer_use: None,
        cap_lead: None,
        has_key: true,
        is_builtin: false,
        enabled: true,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    }
}

fn harness_profile() -> db::AgentProfile {
    let mut p = borrow_profile();
    p.id = "harness-deepseek".to_string();
    p.access = "harness".to_string();
    p.provider = "deepseek".to_string();
    p.endpoint = Some("https://api.deepseek.com/v1".to_string());
    p.primary_model = Some("deepseek-chat".to_string());
    p
}

fn borrow_command(profile: db::AgentProfile) -> Command {
    let test = setup_context();
    let backend = BorrowClaudeBackend {
        profile,
        api_key: "test-key".to_string(),
    };
    let ctx = build_context(&test, "hi");

    backend.build_command(&ctx).unwrap()
}

fn env_value(cmd: &Command, key: &str) -> Option<Option<String>> {
    cmd.get_envs()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.map(|value| value.to_string_lossy().into_owned()))
}

#[test]
fn all_agent_build_commands_disable_optional_git_locks() {
    let test = setup_context();
    let ctx = build_context(&test, "hi");
    let commands = [
        NativeBackend {
            provider: "claude".to_string(),
            primary_model: None,
        }
        .build_command(&ctx)
        .unwrap(),
        NativeBackend {
            provider: "codex".to_string(),
            primary_model: Some("gpt-test".to_string()),
        }
        .build_command(&ctx)
        .unwrap(),
        BorrowClaudeBackend {
            profile: borrow_profile(),
            api_key: "test-key".to_string(),
        }
        .build_command(&ctx)
        .unwrap(),
        HarnessBackend {
            profile: harness_profile(),
            api_key: None,
            search_api_key: None,
            search_backend: None,
        }
        .build_command(&ctx)
        .unwrap(),
    ];

    for cmd in commands {
        assert_eq!(
            env_value(&cmd, "GIT_OPTIONAL_LOCKS"),
            Some(Some("0".to_string()))
        );
    }
}

#[test]
fn claude_backends_receive_scoped_settings_and_hidden_token() {
    let test = setup_context();
    let ctx = build_context(&test, "hi");
    let commands = [
        NativeBackend {
            provider: "claude".to_string(),
            primary_model: None,
        }
        .build_command(&ctx)
        .unwrap(),
        BorrowClaudeBackend {
            profile: borrow_profile(),
            api_key: "test-key".to_string(),
        }
        .build_command(&ctx)
        .unwrap(),
    ];

    for cmd in commands {
        let args = command_args(&cmd);
        let settings_index = args
            .iter()
            .position(|arg| arg == "--settings")
            .expect("Claude command should include --settings");
        let settings = args
            .get(settings_index + 1)
            .expect("--settings should have a path");
        assert!(settings.contains("/.agentloom/hooks/claude-"));
        let token = env_value(&cmd, crate::checkpoint_hook::TOKEN_ENV)
            .flatten()
            .expect("checkpoint token should be injected through the environment");
        assert_eq!(token.len(), 64);
        assert!(!args.iter().any(|arg| arg.contains(&token)));
    }
}

#[test]
fn write_modes_scrub_ambient_checkpoint_env_and_only_inject_fresh_backend_values() {
    let _mode = set_harness_mode_for_test(None);
    let _checkpoint_env = set_checkpoint_envs_for_test(
        Some("http://127.0.0.1:65535/checkpoint"),
        Some("stale-parent-token"),
    );
    let test = setup_context();

    for mode in [BuildMode::Normal, BuildMode::Worker] {
        let ctx = build_context_for_mode(&test, "hi", mode);
        let native_claude = NativeBackend {
            provider: "claude".to_string(),
            primary_model: None,
        }
        .build_command(&ctx)
        .unwrap();
        let native_codex = NativeBackend {
            provider: "codex".to_string(),
            primary_model: Some("gpt-test".to_string()),
        }
        .build_command(&ctx)
        .unwrap();
        let borrow = BorrowClaudeBackend {
            profile: borrow_profile(),
            api_key: "test-key".to_string(),
        }
        .build_command(&ctx)
        .unwrap();
        let harness = HarnessBackend {
            profile: harness_profile(),
            api_key: Some("k".to_string()),
            search_api_key: None,
            search_backend: None,
        }
        .build_command(&ctx)
        .unwrap();

        for cmd in [native_claude, native_codex, borrow] {
            let token = env_value(&cmd, crate::checkpoint_hook::TOKEN_ENV)
                .flatten()
                .expect("write-mode native/borrow backend should inject a fresh token");
            assert_eq!(token.len(), 64);
            assert_ne!(token, "stale-parent-token");
            assert_eq!(
                env_value(&cmd, crate::checkpoint_hook::ENDPOINT_ENV),
                Some(None),
                "native/borrow backends must scrub inherited checkpoint endpoint env"
            );
        }

        let harness_token = env_value(&harness, crate::checkpoint_hook::TOKEN_ENV)
            .flatten()
            .expect("write-mode harness backend should inject a fresh token");
        assert_eq!(harness_token.len(), 64);
        assert_ne!(harness_token, "stale-parent-token");
        assert_eq!(
            env_value(&harness, crate::checkpoint_hook::ENDPOINT_ENV),
            Some(Some("http://127.0.0.1:9/checkpoint".into())),
            "harness backend should replace the ambient endpoint with the per-run hook endpoint"
        );
    }
}

#[test]
fn read_only_modes_scrub_ambient_checkpoint_env_for_all_backends() {
    let _mode = set_harness_mode_for_test(None);
    let _checkpoint_env = set_checkpoint_envs_for_test(
        Some("http://127.0.0.1:65535/checkpoint"),
        Some("stale-parent-token"),
    );
    let test = setup_context();

    for mode in [
        BuildMode::LeadDraft,
        BuildMode::LeadAction,
        BuildMode::Summarize,
    ] {
        let ctx = build_context_for_mode(&test, "hi", mode);
        let commands = [
            NativeBackend {
                provider: "claude".to_string(),
                primary_model: None,
            }
            .build_command(&ctx)
            .unwrap(),
            NativeBackend {
                provider: "codex".to_string(),
                primary_model: Some("gpt-test".to_string()),
            }
            .build_command(&ctx)
            .unwrap(),
            BorrowClaudeBackend {
                profile: borrow_profile(),
                api_key: "test-key".to_string(),
            }
            .build_command(&ctx)
            .unwrap(),
            HarnessBackend {
                profile: harness_profile(),
                api_key: Some("k".to_string()),
                search_api_key: None,
                search_backend: None,
            }
            .build_command(&ctx)
            .unwrap(),
        ];

        for cmd in commands {
            assert_eq!(
                env_value(&cmd, crate::checkpoint_hook::TOKEN_ENV),
                Some(None),
                "read-only backends must scrub ambient checkpoint token env"
            );
            assert_eq!(
                env_value(&cmd, crate::checkpoint_hook::ENDPOINT_ENV),
                Some(None),
                "read-only backends must scrub ambient checkpoint endpoint env"
            );
        }
    }
}

#[test]
fn safe_id_strips_dotdot() {
    let id = safe_id("a/../b").unwrap();

    assert!(!id.contains('/'), "safe id should strip slash: {id}");
    assert!(!id.contains('.'), "safe id should strip dots: {id}");
}

#[test]
fn safe_id_empty_errs() {
    assert_eq!(
        safe_id("...///").unwrap_err(),
        "AL_ERR:agent.emptyFilteredId"
    );
}

#[test]
fn resolve_base_url_proxy_localhost() {
    assert_eq!(
        resolve_base_url(Some("thinking_passback"), "https://x", Some(8080)),
        "http://127.0.0.1:8080"
    );
}

#[test]
fn resolve_base_url_direct() {
    assert_eq!(resolve_base_url(None, "https://x", Some(8080)), "https://x");
}

/// 造一根管道、写入 `payload` 后立刻关写端（数据留在缓冲区，读端仍能读到，随后遇 EOF——
/// 不需要一直吊着写端）；返回读端包成 `Stdio`，给 `Command::stdin` 直接用。
#[cfg(unix)]
fn probe_pipe_with_payload(payload: &[u8]) -> std::process::Stdio {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "libc::pipe 建管道失败");
    let mut write_end = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    write_end.write_all(payload).expect("写探针管道 payload");
    drop(write_end);
    unsafe { std::process::Stdio::from_raw_fd(fds[0]) }
}

/// unix-only：这些测试针对 shell PATH 解析链路，Windows 下相关函数不存在
/// （Windows 从注册表继承完整 PATH，无需修复，见 augmented_path_for_spawn 的 windows 分支）。
#[cfg(unix)]
mod unix_path_resolution_tests;

mod claude;
mod codex;
mod harness;
mod path_resolution;
mod stdin;
