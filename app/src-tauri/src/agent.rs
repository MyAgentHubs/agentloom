pub use crate::db::AgentProfile;

mod harness_attachments;

use std::ffi::{OsStr, OsString};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct BuildContext<'a> {
    pub prompt: &'a str,
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub wt: &'a Path,
    pub conn: &'a rusqlite::Connection,
    pub mode: BuildMode,
    pub locale: crate::Locale,
    pub reasoning_tier: Option<&'a str>,
    pub criteria: &'a [String],
}

/// worker / lead one-shot / Normal 区分注入点·守 DNA「Normal=原生」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildMode {
    Normal,
    Worker,
    LeadDraft,
    LeadAction,
    Summarize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseFn {
    Claude,
    Codex,
    Harness,
    HarnessPlan,
}

/// claude / codex 的 prompt 正文改走子进程 stdin（argv 不再带正文，超长 prompt 撞 ARG_MAX
/// 会报 `Argument list too long (os error 7)`，见 `spawn_with_stdin_prompt`）。`Arc<str>` 而非
/// `String`：team member 的 auth-retry 会对同一份 payload 重复 spawn+写，克隆 `Arc` 是 O(1)，
/// 不会每次重复拷贝整段正文。
pub(crate) type StdinPrompt = std::sync::Arc<str>;

pub trait AgentBackend {
    fn build_command(&self, ctx: &BuildContext) -> Result<Command, String> {
        let mut cmd = self.build_command_inner(ctx)?;
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        Ok(cmd)
    }
    fn build_command_inner(&self, ctx: &BuildContext) -> Result<Command, String>;
    fn parse_fn(&self) -> ParseFn;
    /// argv 不带 prompt 正文的 backend（claude / codex）在这里返回 `Some(payload)`：调用方
    /// spawn 前必须把它写进子进程 stdin 再关闭（EOF）——见 `spawn_with_stdin_prompt`。
    /// 默认 `None`（如 harness：prompt 走 app 域临时文件传路径，不用 stdin）。
    fn stdin_prompt(&self, _ctx: &BuildContext) -> Option<StdinPrompt> {
        None
    }
}

pub(crate) struct SpawnedWithStdinPrompt {
    pub child: std::process::Child,
    /// `Some` 仅表示本次有 prompt：receiver 在 `write_all + flush + drop(stdin)` 后收到 I/O
    /// 结果。无 prompt 时 stdin 直接接空设备，因此为 `None`，没有需要等待的 writer ack。
    pub stdin_ack: Option<std::sync::mpsc::Receiver<std::io::Result<()>>>,
}

/// 带显式 stdin writer ack 的 spawn 入口。writer 线程只负责 I/O，不持 DB 状态或业务回调；
/// `write_all + flush + drop(stdin)` 完成后发送其 `io::Result<()>`。线程创建失败也会立即预置为
/// `Err`，不会留下永不返回的 receiver。
///
/// `stdin_prompt` 为 `None` 时显式 `Stdio::null()`，返回的 `stdin_ack` 为 `None`。调用前不能已经
/// 对 `command` 调过 `.stdin(..)`——本函数是唯一决定 stdin 去向的地方。
pub(crate) fn spawn_with_stdin_prompt_ack(
    command: &mut Command,
    stdin_prompt: Option<&StdinPrompt>,
) -> std::io::Result<SpawnedWithStdinPrompt> {
    match stdin_prompt {
        Some(_) => command.stdin(std::process::Stdio::piped()),
        None => command.stdin(std::process::Stdio::null()),
    };
    let mut child = command.spawn()?;
    let stdin_ack = if let Some(prompt) = stdin_prompt {
        let mut stdin = child
            .stdin
            .take()
            .expect("stdin piped when stdin_prompt is Some");
        let prompt = prompt.clone();
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        let spawn_error_tx = ack_tx.clone();
        let spawn_result = std::thread::Builder::new()
            .name("agent-stdin-writer".into())
            .spawn(move || {
                let write_result = stdin.write_all(prompt.as_bytes());
                let flush_result = stdin.flush();
                let result = write_result.and(flush_result);
                drop(stdin);
                let _ = ack_tx.send(result);
            });
        match spawn_result {
            Ok(_) => drop(spawn_error_tx),
            Err(error) => {
                // spawn 失败会 drop 掉闭包（连同其中的 stdin/ack_tx）；用保留 sender 预置
                // 明确错误，保证调用方不会面对一个永远收不到结果的 receiver。
                let _ = spawn_error_tx.send(Err(error));
            }
        }
        Some(ack_rx)
    } else {
        None
    };
    Ok(SpawnedWithStdinPrompt { child, stdin_ack })
}

/// 全仓 claude/codex/borrow spawn 的兼容入口：`stdin_prompt` 非空时设 `Stdio::piped()`、spawn
/// 后起独立线程写完整段正文并关闭 fd（EOF）。同步写可能超过管道缓冲（64KB）把调用线程堵死，
/// 故必须用独立线程写，不能就地写。现有调用方无需消费 ack；需要按 I5 等待交付结果的新调用方
/// 使用 [`spawn_with_stdin_prompt_ack`]。
pub(crate) fn spawn_with_stdin_prompt(
    command: &mut Command,
    stdin_prompt: Option<&StdinPrompt>,
) -> std::io::Result<std::process::Child> {
    spawn_with_stdin_prompt_ack(command, stdin_prompt).map(|spawned| {
        let SpawnedWithStdinPrompt { child, stdin_ack } = spawned;
        drop(stdin_ack);
        child
    })
}

pub fn safe_id(id: &str) -> Result<String, String> {
    let id: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if id.is_empty() {
        Err(crate::ui_msg::al_err("agent.emptyFilteredId", &[]))
    } else {
        Ok(id)
    }
}

/// sidecar 进程退出后是否叠加一条通用 Error 事件（桥收尾判定·M2 评测缝）。
/// 已发过任一诚实终态（Error / Blocked / NeedsDecision）时，非零退出码只是该终态的
/// 携带信号（如 blocked=exit 3、needs_decision=exit 4），绝不再叠加一条通用 Error 把
/// 用户可见的真实原因顶成「进程失败」。仅在**没有**任何诚实终态、又非用户主动中断、
/// 且退出非零时，才补一条通用 Error（评测场景 02/05/06/10）。
pub fn sidecar_exit_error(
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    exit_success: bool,
    interrupted: bool,
) -> bool {
    !interrupted && !exit_success && !saw_error && !saw_blocked && !saw_needs_decision
}

fn resolve_base_url(compat_proxy: Option<&str>, endpoint: &str, proxy_port: Option<u16>) -> String {
    match (compat_proxy, proxy_port) {
        (Some("thinking_passback"), Some(port)) => format!("http://127.0.0.1:{port}"),
        _ => endpoint.to_string(),
    }
}

pub struct NativeBackend {
    pub provider: String,
    pub primary_model: Option<String>,
}

/// Windows 上 npm 装的 codex 只落 `codex.cmd`（没有 `codex.exe`），而 `Command::new("codex")`
/// 只会补 `.exe`、不查 PATHEXT —— 裸名永远找不到它。这里改走 detect 那套 PATHEXT 感知的解析
/// 拿绝对路径（再由 proc::command 把外壳换成真实解释器）。
/// 非 Windows 保持裸名 "codex"（走 PATH + augmented PATH），行为逐字节不变。
pub(crate) fn resolve_codex_bin() -> Result<OsString, String> {
    let windows = cfg!(target_os = "windows");
    let override_path = crate::cli_path_override_for_spawn("codex");
    crate::detect::resolve_cli_path_with_override(override_path.as_deref(), windows, || {
        windows
            .then(|| crate::detect::which_or_fallback("codex", &[]))
            .flatten()
    })
    .map(|path| {
        path.map(OsString::from)
            .unwrap_or_else(|| OsString::from("codex"))
    })
}

pub(crate) fn supports_solo_commit_mcp(profile: &AgentProfile) -> bool {
    matches!(profile.provider.as_str(), "claude" | "codex") && profile.access == "native"
}

pub(crate) const SOLO_MCP_DELIVERY_GUIDANCE: &str = "\
Record changes with mcp__agentloom__commit. For delivery, use mcp__agentloom__push, \
mcp__agentloom__create_pr, or mcp__agentloom__publish; these tools ask the user for confirmation \
before running. Do not deliver with raw git push, gh pr create, or similar shell commands, because \
that bypasses user confirmation.";

pub(crate) fn solo_commit_mcp_argv_extra(profile: &AgentProfile, port: u16) -> Vec<String> {
    if !supports_solo_commit_mcp(profile) {
        return Vec::new();
    }

    match profile.provider.as_str() {
        "claude" => vec![
            "--mcp-config".to_string(),
            crate::mcp_server::mcp_config_json(port),
            "--strict-mcp-config".to_string(),
            "--allowedTools".to_string(),
            "mcp__agentloom__commit,mcp__agentloom__push,mcp__agentloom__create_pr,mcp__agentloom__publish"
                .to_string(),
            "--append-system-prompt".to_string(),
            SOLO_MCP_DELIVERY_GUIDANCE.to_string(),
        ],
        "codex" => vec![
            "-c".to_string(),
            format!("mcp_servers.agentloom.url=\"http://127.0.0.1:{port}/mcp\""),
            "-c".to_string(),
            "mcp_servers.agentloom.default_tools_approval_mode=\"approve\"".to_string(),
            "-c".to_string(),
            "mcp_servers.agentloom.tool_timeout_sec=86400".to_string(),
            "-c".to_string(),
            "mcp_servers.agentloom.startup_timeout_sec=60".to_string(),
            "-c".to_string(),
            format!("developer_instructions={SOLO_MCP_DELIVERY_GUIDANCE:?}"),
        ],
        _ => Vec::new(),
    }
}

/// 给 solo 命令接入进程内 MCP。Claude 的 MCP 参数可放在命令尾；Codex 的 `-c`
/// 是全局参数，必须与已有 `-a` / `-m` / `-c` 同处于 `exec` 子命令之前。
pub(crate) fn attach_solo_commit_mcp_argv(
    command: &mut Command,
    profile: &AgentProfile,
    port: u16,
) -> Result<(), String> {
    let extra = solo_commit_mcp_argv_extra(profile, port);
    if extra.is_empty() {
        return Ok(());
    }
    if profile.provider != "codex" {
        command.args(extra);
        return Ok(());
    }

    let program = command.get_program().to_os_string();
    let mut args = command
        .get_args()
        .map(OsStr::to_os_string)
        .collect::<Vec<_>>();
    let exec_index = args
        .windows(2)
        .position(|pair| pair[0] == "exec" && pair[1] == "--json")
        .ok_or_else(|| "codex command is missing the exec subcommand".to_string())?;
    args.splice(
        exec_index..exec_index,
        extra.into_iter().map(OsString::from),
    );

    let current_dir = command.get_current_dir().map(Path::to_path_buf);
    let envs = command
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(OsStr::to_os_string)))
        .collect::<Vec<_>>();
    let mut rebuilt = crate::proc::command(program);
    rebuilt.args(args);
    if let Some(current_dir) = current_dir {
        rebuilt.current_dir(current_dir);
    }
    for (key, value) in envs {
        if let Some(value) = value {
            rebuilt.env(key, value);
        } else {
            rebuilt.env_remove(key);
        }
    }
    *command = rebuilt;
    Ok(())
}

pub(crate) fn effective_reasoning_tier(tier: &str) -> &str {
    if tier == "auto" {
        "medium"
    } else {
        tier
    }
}

pub(crate) fn claude_effort_for_reasoning_tier(tier: &str) -> Option<&'static str> {
    match tier.trim().to_ascii_lowercase().as_str() {
        "auto" => Some("medium"),
        "none" | "minimal" => Some("low"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

/// solo/Normal 会话产图引导：产出图片时用 Markdown 内联图语法引用，裸路径不会内联显示。
/// 语义与 `member_runner.rs` 任务包工程纪律段的英文版一致（同一条产品行为、两处措辞同源）。
pub(crate) const SOLO_IMAGE_OUTPUT_GUIDANCE: &str = "\
If you produce or generate an image file (such as a screenshot or chart) that you want the user \
to see directly in chat, reference it in your reply with the Markdown inline image syntax \
`![](absolute image path)`; a bare path will not display inline. If the path contains spaces, \
wrap it in angle brackets: `![](</path/with space.png>)`.";

fn system_prompt_for_mode(mode: BuildMode) -> Option<&'static str> {
    match mode {
        BuildMode::Normal => Some(SOLO_IMAGE_OUTPUT_GUIDANCE),
        BuildMode::Summarize => None,
        BuildMode::Worker => Some(crate::WORKER_ONESHOT_PROMPT),
        BuildMode::LeadDraft => Some(crate::lead_draft::LEAD_DRAFT_SYS_PROMPT),
        BuildMode::LeadAction => Some(crate::lead_step::LEAD_DECISION_SYS_PROMPT),
    }
}

const CODEX_IMAGE_OUTPUT_INSTRUCTION: &str = "If you generate any image files, save or copy them into the current workspace and state each image's absolute path in your final reply. Do not leave generated images only under $CODEX_HOME/generated_images. Also reference each image in your final reply with Markdown inline image syntax `![](absolute path)`; a bare path will not display inline. If the path contains spaces, wrap it in angle brackets: `![](</path/with space.png>)`.";

fn prompt_for_mode(mode: BuildMode, prompt: &str) -> String {
    let prompt = match system_prompt_for_mode(mode) {
        Some(system) if matches!(mode, BuildMode::LeadDraft | BuildMode::LeadAction) => {
            format!("{system}\n\n{prompt}")
        }
        _ => prompt.to_string(),
    };

    if matches!(mode, BuildMode::Normal | BuildMode::Worker) {
        format!("{prompt}\n\n{CODEX_IMAGE_OUTPUT_INSTRUCTION}")
    } else {
        prompt
    }
}

fn checkpoint_hook_for_mode(
    ctx: &BuildContext,
) -> Result<Option<crate::checkpoint_hook::HookConfig>, String> {
    matches!(ctx.mode, BuildMode::Normal | BuildMode::Worker)
        .then(|| crate::checkpoint_hook::install(ctx.conn, ctx.session_id, ctx.run_id, ctx.wt))
        .transpose()
}

fn scrub_checkpoint_env(command: &mut Command) {
    command.env_remove(crate::checkpoint_hook::TOKEN_ENV);
    command.env_remove(crate::checkpoint_hook::ENDPOINT_ENV);
}

fn configure_harness_checkpoint_env(
    command: &mut Command,
    hook: Option<&crate::checkpoint_hook::HookConfig>,
) {
    scrub_checkpoint_env(command);
    if let Some(hook) = hook {
        crate::checkpoint_hook::configure_harness_command(command, hook);
    }
}

fn harness_read_only_mode(mode: BuildMode) -> bool {
    matches!(
        mode,
        BuildMode::LeadDraft | BuildMode::LeadAction | BuildMode::Summarize
    )
}

fn harness_permission_for_mode(mode: BuildMode) -> &'static str {
    if harness_read_only_mode(mode) {
        "deny"
    } else {
        "allow"
    }
}

fn harness_read_only_disallowed_tools(mode: BuildMode) -> Option<&'static str> {
    harness_read_only_mode(mode).then_some("fs_edit,fs_write,shell_exec")
}

fn append_lead_read_only_tools(mode: BuildMode, extra: &mut Vec<String>) {
    if matches!(mode, BuildMode::LeadDraft | BuildMode::LeadAction) {
        extra.extend([
            "--disallowedTools".to_string(),
            "Write,Edit,MultiEdit,NotebookEdit,Bash".to_string(),
        ]);
    }
}

fn is_stale_native_codex_model(model: &str) -> bool {
    matches!(model.trim(), "gpt-5" | "gpt-5.3-codex")
}

fn effective_native_codex_model(primary_model: Option<&str>) -> Option<String> {
    let primary_model = primary_model
        .map(str::trim)
        .filter(|model| !model.is_empty());
    if let Some(model) = primary_model.filter(|model| !is_stale_native_codex_model(model)) {
        return Some(model.to_string());
    }
    read_user_codex_config_model()
}

fn read_user_codex_config_model() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".codex").join("config.toml");
    let contents = std::fs::read_to_string(path).ok()?;
    parse_top_level_codex_model(&contents)
}

fn parse_top_level_codex_model(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            break;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "model" {
            continue;
        }
        let value = value.trim();
        let Some(value) = value.strip_prefix('"') else {
            continue;
        };
        let Some(end) = value.find('"') else {
            continue;
        };
        let model = value[..end].trim();
        if !model.is_empty() {
            return Some(model.to_string());
        }
    }
    None
}

impl AgentBackend for NativeBackend {
    fn build_command_inner(&self, ctx: &BuildContext) -> Result<Command, String> {
        let _ = (ctx.session_id, ctx.conn);
        match self.provider.as_str() {
            "claude" => {
                let mut extra: Vec<String> = self
                    .primary_model
                    .as_deref()
                    .filter(|model| !model.trim().is_empty())
                    .map(|model| vec!["--model".to_string(), model.to_string()])
                    .unwrap_or_default();
                let hook = checkpoint_hook_for_mode(ctx)?;
                if let Some(hook) = &hook {
                    extra.push("--settings".to_string());
                    extra.push(hook.settings_path.to_string_lossy().into_owned());
                }
                if let Some(system_prompt) = system_prompt_for_mode(ctx.mode) {
                    extra.push("--append-system-prompt".to_string());
                    extra.push(system_prompt.to_string());
                }
                append_lead_read_only_tools(ctx.mode, &mut extra);
                if ctx.mode == BuildMode::Worker {
                    extra.extend(crate::worker_tools_allowlist());
                }
                if ctx.mode == BuildMode::Summarize {
                    extra.extend(crate::summarize_tools_allowlist());
                }
                if let Some(effort) = ctx
                    .reasoning_tier
                    .and_then(claude_effort_for_reasoning_tier)
                {
                    extra.push("--effort".to_string());
                    extra.push(effort.to_string());
                }
                let extra_ref: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();
                let (mut cmd, claude_bin) = crate::claude_sandboxed_cmd_in(ctx.wt, &extra_ref)?;
                crate::log_claude_bin(ctx.session_id, &claude_bin);
                scrub_checkpoint_env(&mut cmd);
                crate::apply_clean_env(&mut cmd);
                if let Some(hook) = hook {
                    cmd.env(crate::checkpoint_hook::TOKEN_ENV, hook.token);
                }
                Ok(cmd)
            }
            "codex" => {
                let mut cmd = crate::proc::command(resolve_codex_bin()?);
                if let Some(path) = augmented_path_for_spawn() {
                    cmd.env("PATH", path);
                }
                scrub_checkpoint_env(&mut cmd);
                cmd.args(["-a", "never"]);
                let hook = checkpoint_hook_for_mode(ctx)?;
                if let Some(model) = effective_native_codex_model(self.primary_model.as_deref()) {
                    cmd.args(["-m", model.as_str()]);
                }
                if let Some(tier) = ctx.reasoning_tier {
                    let tier = effective_reasoning_tier(tier);
                    let config = format!("model_reasoning_effort=\"{tier}\"");
                    cmd.args(["-c", config.as_str()]);
                }
                if let Some(hook) = &hook {
                    crate::checkpoint_hook::configure_codex_command(&mut cmd, hook);
                }
                let sandbox = if matches!(
                    ctx.mode,
                    BuildMode::LeadDraft | BuildMode::LeadAction | BuildMode::Summarize
                ) {
                    "read-only"
                } else {
                    "workspace-write"
                };
                // prompt 正文不再进 argv（超长 prompt 会撞 ARG_MAX）：位置参数传 "-"，
                // 实测确认 `codex exec ... -` 从 stdin 读正文；真正的正文由 `stdin_prompt()`
                // 提供，调用方经 `spawn_with_stdin_prompt` 写入子进程 stdin。
                cmd.args([
                    "exec",
                    "--json",
                    "--ignore-user-config",
                    "--skip-git-repo-check",
                    "--sandbox",
                    sandbox,
                    "-",
                ]);
                crate::apply_workdir(&mut cmd, ctx.wt);
                Ok(cmd)
            }
            other => Err(crate::ui_msg::al_err(
                "agent.unknownEngine",
                &[("engine", other.to_string())],
            )),
        }
    }

    fn parse_fn(&self) -> ParseFn {
        match self.provider.as_str() {
            "codex" => ParseFn::Codex,
            _ => ParseFn::Claude,
        }
    }

    fn stdin_prompt(&self, ctx: &BuildContext) -> Option<StdinPrompt> {
        match self.provider.as_str() {
            "claude" => Some(StdinPrompt::from(ctx.prompt)),
            // codex 的 argv 已经不带正文（build_command_inner 传 "-" 代替 prompt 位置参数），
            // stdin 必须写 prompt_for_mode 处理过的同一份文本（含 mode 相关的 system 前缀/
            // 图片输出指引），不能直接写 ctx.prompt 原文，否则跟 argv 版本语义对不上。
            "codex" => Some(StdinPrompt::from(prompt_for_mode(ctx.mode, ctx.prompt))),
            _ => None,
        }
    }
}

/// borrow-claude 身份提示：告诉模型它实际是谁（不是 Claude），防止误自称。
/// 供 `BorrowClaudeBackend` 与 lead borrow spawn（`lib.rs::start_lead_session` 的
/// `borrow_lead_cmd_in` 分支）共用——两处身份提示措辞必须同源，不能各写一份走样。
pub(crate) fn borrow_claude_identity_prompt(profile: &AgentProfile) -> String {
    format!(
        "重要身份说明：你实际运行在 {}（provider={}）模型上（经兼容接口接入）。被问到你是谁/什么模型时，必须如实回答你是 {}，绝不能自称 Claude 或 Anthropic。",
        profile.name, profile.provider, profile.name
    )
}

/// borrow-claude env 装配：CLAUDE_CONFIG_DIR 隔离配置目录 + settings.json 清理 +
/// ANTHROPIC_BASE_URL/AUTH_TOKEN(或 API_KEY) + 模型 env + 推理档位 + timeout/compat 开关。
/// 供 `BorrowClaudeBackend`（Normal/Worker/Summarize/LeadDraft/LeadAction 模式）与
/// lead borrow spawn（`borrow_lead_cmd_in`）共用——两处 env 必须同源，行为一致由测试钉住。
///
/// 调用前调用方必须已经 `crate::apply_clean_env(cmd)`：本函数只叠加 borrow 专属 env，
/// 不重复做全局 clean；顺序反了（先设 borrow env 再 clean）会被 clean 冲掉。
///
/// `reasoning_tier_override` 为 `None` 时退回 `profile.reasoning_default`
/// （与 `BorrowClaudeBackend` 传 `ctx.reasoning_tier` 的语义一致）。
pub(crate) fn apply_borrow_claude_env(
    cmd: &mut Command,
    profile: &AgentProfile,
    api_key: &str,
    reasoning_tier_override: Option<&str>,
) -> Result<(), String> {
    let safe = safe_id(&profile.id)?;
    let config_dir = std::env::temp_dir().join(format!("agentloom-claude-{safe}"));
    std::fs::create_dir_all(&config_dir).map_err(|e| {
        crate::ui_msg::al_err("agent.configDirCreateFailed", &[("detail", e.to_string())])
    })?;
    let config_dir = std::fs::canonicalize(&config_dir).unwrap_or(config_dir);
    let tmp = std::env::temp_dir();
    let tmp = std::fs::canonicalize(&tmp).unwrap_or(tmp);
    if !config_dir.starts_with(&tmp) {
        return Err(format!(
            "CLAUDE_CONFIG_DIR 不在临时目录下：config={config_dir:?} tmp={tmp:?}"
        ));
    }
    let _ = std::fs::remove_file(config_dir.join("settings.json"));

    for k in [
        "CLAUDE_CODE_DISABLE_THINKING",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
        "CLAUDE_CODE_EFFORT_LEVEL",
    ] {
        cmd.env_remove(k);
    }

    let endpoint = profile
        .endpoint
        .as_deref()
        .filter(|endpoint| !endpoint.is_empty())
        .ok_or_else(|| {
            crate::ui_msg::al_err("agent.missingEndpoint", &[("id", profile.id.clone())])
        })?;
    let proxy_port = if profile.compat_proxy.as_deref() == Some("thinking_passback") {
        crate::deepseek_proxy::ensure_proxy(endpoint)
    } else {
        None
    };
    let base_url = resolve_base_url(profile.compat_proxy.as_deref(), endpoint, proxy_port);
    cmd.env("CLAUDE_CONFIG_DIR", &config_dir)
        .env("ANTHROPIC_BASE_URL", base_url);

    if profile.auth_mode.as_deref() == Some("x_api_key") {
        cmd.env("ANTHROPIC_API_KEY", api_key);
    } else {
        cmd.env("ANTHROPIC_AUTH_TOKEN", api_key);
    }

    let primary_model = profile
        .primary_model
        .as_deref()
        .filter(|model| !model.is_empty());
    if let Some(model) = primary_model {
        cmd.env("ANTHROPIC_MODEL", model);
    }
    set_model_env(
        cmd,
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        profile.model_opus.as_deref(),
        primary_model,
    );
    set_model_env(
        cmd,
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        profile.model_sonnet.as_deref(),
        primary_model,
    );
    set_model_env(
        cmd,
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        profile.model_haiku.as_deref(),
        primary_model,
    );
    set_model_env(
        cmd,
        "CLAUDE_CODE_SUBAGENT_MODEL",
        profile.model_subagent.as_deref(),
        primary_model,
    );

    let reasoning_tier =
        effective_reasoning_tier(reasoning_tier_override.unwrap_or(&profile.reasoning_default));
    cmd.env("CLAUDE_CODE_EFFORT_LEVEL", reasoning_tier).env(
        "API_TIMEOUT_MS",
        profile.api_timeout_ms.unwrap_or(600000).to_string(),
    );
    if let Some(tokens) = profile.max_output_tokens {
        cmd.env("CLAUDE_CODE_MAX_OUTPUT_TOKENS", tokens.to_string());
    }
    if profile.compat_disable_nonessential {
        cmd.env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    }
    if profile.compat_disable_betas {
        cmd.env("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1");
    }
    if profile.compat_disable_thinking {
        cmd.env("CLAUDE_CODE_DISABLE_THINKING", "1");
    }

    Ok(())
}

pub struct BorrowClaudeBackend {
    pub profile: AgentProfile,
    pub api_key: String,
}

impl AgentBackend for BorrowClaudeBackend {
    fn build_command_inner(&self, ctx: &BuildContext) -> Result<Command, String> {
        let profile = &self.profile;

        let mut identity_prompt = borrow_claude_identity_prompt(profile);
        if ctx.mode == BuildMode::Normal {
            identity_prompt.push_str(crate::language_directive(ctx.locale));
        }
        let system_prompt = match system_prompt_for_mode(ctx.mode) {
            Some(mode_prompt) => format!("{identity_prompt}\n\n{mode_prompt}"),
            None => identity_prompt.clone(),
        };
        // --disable-slash-commands 已下沉进 claude_agent_argv()（全线共用基础项），
        // 这里不再 ad-hoc 加，避免同一 flag 出现两次。
        let mut extra: Vec<String> = vec!["--append-system-prompt".to_string(), system_prompt];
        let hook = checkpoint_hook_for_mode(ctx)?;
        if let Some(hook) = &hook {
            extra.push("--settings".to_string());
            extra.push(hook.settings_path.to_string_lossy().into_owned());
        }
        if matches!(ctx.mode, BuildMode::Worker) {
            extra.extend(crate::worker_tools_allowlist());
        }
        if matches!(ctx.mode, BuildMode::Summarize) {
            extra.extend(crate::summarize_tools_allowlist());
        }
        append_lead_read_only_tools(ctx.mode, &mut extra);
        let extra_ref: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();
        let (mut cmd, claude_bin) = crate::claude_sandboxed_cmd_in(ctx.wt, &extra_ref)?;
        crate::log_claude_bin(ctx.session_id, &claude_bin);

        scrub_checkpoint_env(&mut cmd);
        crate::apply_clean_env(&mut cmd);
        if let Some(hook) = hook {
            cmd.env(crate::checkpoint_hook::TOKEN_ENV, hook.token);
        }

        apply_borrow_claude_env(&mut cmd, profile, &self.api_key, ctx.reasoning_tier)?;

        Ok(cmd)
    }

    fn parse_fn(&self) -> ParseFn {
        ParseFn::Claude
    }

    fn stdin_prompt(&self, ctx: &BuildContext) -> Option<StdinPrompt> {
        Some(StdinPrompt::from(ctx.prompt))
    }
}

/// M2 sidecar：spawn `myagent run` / `myagent plan` 子进程，经 JSONL 协议回传事件。
/// 二进制路径解析走三级优先级（见 `resolve_myagent_bin`）：
/// ① MYAGENT_BIN 环境变量（dev 指向 harness cargo build，空串/纯空白视为未设置）；
/// ② 打包产物内与主程序同目录的 sidecar：macOS 只认
///    `.app/Contents/MacOS/myagent`；Windows 只认安装目录同级的 `myagent.exe`
///    （Tauri v2 的 NSIS / MSI 都会去掉 target triple 后把 externalBin 放进主程序目录）。
///    `target/debug` / `target/release`（含 `target/<triple>/<profile>`）即使被
///    tauri-build 顺带放了同名二进制也不认，防止本地直跑 app 时静默
///    命中打包快照而非最新 engine；Linux 仍不开启同目录解析；
/// ③ 裸名 "myagent"，交给 PATH 查找（兜底；双击启动的 .app 的 PATH 不含 ~/.local/bin，故不能只靠这级）。
/// key 可选：Some → 注入 MYAGENT_API_KEY；None → sidecar 继承父进程环境兜底（引擎侧优先级 = {PREFIX}_API_KEY > MYAGENT_API_KEY > stored config）。
/// 显式配置的 provider 专属环境变量（{PREFIX}_API_KEY 等）会压过 shell 继承的同名变量，避免用户 shell 里旧的/其他账号的 provider key 意外覆盖 GUI 配置。
/// 前缀为 MYAGENT 开头的 provider 名不做专属注入——防撞 MYAGENT_API_KEY/MYAGENT_SEARCH_* 等保留名（codex 审 Low）。
/// 不调 apply_clean_env：保留继承环境给 harness config 兜底（与 NativeBackend codex 路径一致）。
pub struct HarnessBackend {
    pub profile: AgentProfile,
    pub api_key: Option<String>,
    pub search_api_key: Option<String>,
    pub search_backend: Option<String>,
}

pub fn harness_plan_mode_enabled() -> bool {
    std::env::var("MYAGENT_APP_HARNESS_MODE").as_deref() == Ok("plan")
}

/// 解析 myagent 二进制路径，优先级：
/// 1. MYAGENT_BIN 环境变量（dev 模式 / 显式覆盖）——空串或纯空白视为未设置
/// 2. 打包主程序同目录的 sidecar：macOS = `Contents/MacOS/myagent`；
///    Windows = 安装目录下的 `myagent.exe`。Cargo `target/debug` / `target/release`
///    及 target-triple 嵌套形态明确排除，避免静默命中 tauri-build 打包快照。
/// 3. 裸名 "myagent"（交给 PATH 查找）
///
/// 纯函数：不读环境变量、不调 current_exe、不自行读文件系统；平台、路径与
/// 「是否为普通文件」判定都由参数注入，让 macOS host 也能真正覆盖 Windows 分支。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MyagentSidecarPlatform {
    MacOs,
    Windows,
    Other,
}

fn current_myagent_sidecar_platform() -> MyagentSidecarPlatform {
    if cfg!(target_os = "macos") {
        MyagentSidecarPlatform::MacOs
    } else if cfg!(target_os = "windows") {
        MyagentSidecarPlatform::Windows
    } else {
        MyagentSidecarPlatform::Other
    }
}

fn path_component_eq_ascii(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

/// tauri-build 会把 externalBin 复制到 Cargo output 目录。这些是构建中间产物，
/// 不是 Windows 安装目录；直接运行 `target/{debug,release}/agentloom(.exe)` 时必须忽略它们。
fn is_cargo_target_profile_dir(dir: &Path) -> bool {
    if !path_component_eq_ascii(dir, "debug") && !path_component_eq_ascii(dir, "release") {
        return false;
    }

    let Some(parent) = dir.parent() else {
        return false;
    };
    path_component_eq_ascii(parent, "target")
        || parent
            .parent()
            .is_some_and(|target_dir| path_component_eq_ascii(target_dir, "target"))
}

fn resolve_myagent_bin_from(
    env_bin: Option<&str>,
    exe_dir: Option<&Path>,
    platform: MyagentSidecarPlatform,
    is_regular_file: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(bin) = env_bin.map(str::trim).filter(|b| !b.is_empty()) {
        return PathBuf::from(bin);
    }
    if let Some(dir) = exe_dir {
        let sidecar = match platform {
            // `Path::ends_with` 按路径组件比较，不是字符串后缀。
            MyagentSidecarPlatform::MacOs if dir.ends_with("Contents/MacOS") => {
                Some(dir.join("myagent"))
            }
            // Tauri v2 NSIS / MSI 都把去掉 `-<target-triple>` 的 externalBin
            // 放在 `$INSTDIR` / `INSTALLDIR`，与主 exe 同级。
            MyagentSidecarPlatform::Windows if !is_cargo_target_profile_dir(dir) => {
                Some(dir.join("myagent.exe"))
            }
            MyagentSidecarPlatform::MacOs
            | MyagentSidecarPlatform::Windows
            | MyagentSidecarPlatform::Other => None,
        };
        if let Some(sidecar) = sidecar {
            if is_regular_file(&sidecar) {
                return sidecar;
            }
        }
    }
    PathBuf::from("myagent")
}

/// `resolve_myagent_bin_from` 的薄 wrapper：读真实环境变量 + 当前可执行文件目录。
/// pub(crate)：L3 队长装配（lib.rs `harness_lead_cmd_in`）与 `HarnessBackend` 共用同一条二进制解析路径。
pub(crate) fn resolve_myagent_bin() -> PathBuf {
    let env_bin = std::env::var("MYAGENT_BIN").ok();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    resolve_myagent_bin_from(
        env_bin.as_deref(),
        exe_dir.as_deref(),
        current_myagent_sidecar_platform(),
        Path::is_file,
    )
}

/// 双击启动的 .app 继承 launchd 精简 PATH（/usr/bin:/bin:/usr/sbin:/sbin），
/// 缺 node/npm/cargo/gh 等用户级工具，导致 myagent 的 shell_exec 跑不了测试。
/// 这里把常见安装目录【追加到现有 PATH 之后】（不是之前）：
///   - 已在 PATH 里的目录不重复追加（dev 模式下 shell PATH 已含这些 → 结果与改动前逐字节相同）
///   - 不存在的目录不追加
///   - 追加在后面而非前面：不让用户目录里的同名可执行文件压过系统工具，降低 PATH 注入面
///
/// 参数与返回值都用 OsStr/OsString —— PATH 可能包含非 UTF-8 路径，用 &str 会在
/// `to_str()` 处静默跳过这类条目。用 `std::env::split_paths`/`join_paths` 而非手写
/// 冒号切分：冒号只是 Unix 的分隔符，Windows 是分号，手写会把 Windows PATH 拆烂
/// （例如把 `C:\Program Files\nodejs;C:\Windows\system32` 拆成裸的相对路径 `C` 和
/// 被粘连的残片）。
///
/// 纯函数：不读环境变量、不碰真实文件系统（`dir_exists` 谓词注入），方便测试。
#[cfg(unix)]
fn augment_path(current: &OsStr, home: &Path, dir_exists: &dyn Fn(&Path) -> bool) -> OsString {
    // 空 PATH 特判：`split_paths("")` 会产出一个空 PathBuf（历史上代表"当前目录"），
    // 若原样收进候选列表，`join_paths` 会把它拼成开头带分隔符的结果（如 ":a"）。
    // 空输入直接从空列表起步，避免这条、保持「不产生开头分隔符」的既有行为。
    let mut all: Vec<PathBuf> = if current.is_empty() {
        Vec::new()
    } else {
        std::env::split_paths(current).collect()
    };
    let existing: std::collections::HashSet<PathBuf> = all.iter().cloned().collect();

    let candidates = [
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".cargo/bin"),
    ];
    for candidate in candidates {
        if existing.contains(&candidate) {
            continue;
        }
        if !dir_exists(&candidate) {
            continue;
        }
        all.push(candidate);
    }

    match std::env::join_paths(&all) {
        Ok(joined) => joined,
        // `join_paths` 在任一路径本身含平台分隔符时返回 Err（例如 HOME="/Users/a:b"
        // 时拼出的候选目录 "/Users/a:b/.local/bin" 自身就含冒号）。这种边界情况原样
        // 返回 current、不做任何修改——比手写切分更安全，顺带干掉了旧版「HOME 含
        // 冒号会把 PATH 拆坏」的隐患，不需要额外的过滤逻辑。
        Err(_) => current.to_os_string(),
    }
}

/// marker 圈定 `path_from_login_shell` 脚本输出里的 PATH 行，防止 shell rc
/// 打印的 banner（neofetch、欢迎语……）被误当成 PATH 解析。
#[cfg(unix)]
const PATH_BEGIN_MARKER: &str = "__AGENTLOOM_PATH_BEGIN__";
#[cfg(unix)]
const PATH_END_MARKER: &str = "__AGENTLOOM_PATH_END__";

/// 从 login shell 的 stdout 里提取被 marker 圈定的 PATH。
/// shell 的 rc 文件可能打印 banner（neofetch、欢迎语），所以必须用 marker 定界：
/// 取两个 marker 之间的内容，逐行 trim 后取第一行非空内容（正常只有一行）；
/// 缺任一 marker、或 marker 之间全是空白 → `None`；出现多组 marker 取第一组。
///
/// 纯函数：只做字符串解析，不碰进程/环境，方便测试。
#[cfg(unix)]
fn parse_shell_path_output(stdout: &str) -> Option<String> {
    let begin_at = stdout.find(PATH_BEGIN_MARKER)?;
    let after_begin = &stdout[begin_at + PATH_BEGIN_MARKER.len()..];
    let end_at = after_begin.find(PATH_END_MARKER)?;
    let between = &after_begin[..end_at];

    between
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// 解释 login shell 的 stdout：lossy 解码 → marker 提取 → 健全性检查。
/// 纯函数（`dir_exists` 谓词注入），不 spawn、不碰真实文件系统，可单测。
///
/// 健全性检查的用途是拦「解析成 banner 垃圾/空串」这类粗错，**不是安全边界**——
/// 能改用户 rc 的攻击者早已能执行任意代码。
#[cfg(unix)]
fn interpret_shell_stdout(stdout: &[u8], dir_exists: &dyn Fn(&Path) -> bool) -> Option<String> {
    let decoded = String::from_utf8_lossy(stdout);
    let path = parse_shell_path_output(&decoded)?;

    // 健全性检查：解析出的 PATH 必须非空、且至少有一个条目是真实存在的目录。
    // 一个连 /usr/bin 都没有的 PATH 显然是解析错了，回退比信它更安全。
    if path.is_empty() {
        return None;
    }
    let has_real_dir = std::env::split_paths(&path).any(|p| dir_exists(&p));
    if !has_real_dir {
        return None;
    }

    Some(path)
}

/// macOS/Linux：GUI app 由 launchd / display manager 启动，不读 shell rc 文件，
/// PATH 残缺 → 看不到 node/npm/cargo/gh 等用户级工具。硬编码「常见安装目录」
/// （见 `augment_path`）对 homebrew 用户有效，但对 nvm/asdf/mise/volta 这类把
/// 工具链装进带版本号路径的用户完全猜不到。正解是不猜，直接问用户的 login shell
/// 要真实 PATH（VS Code / Cursor 同款做法）：跑 `$SHELL -ilc '<marker 脚本>'`，
/// `-i` 让它读 `.zshrc`、`-l` 让它读 `.zprofile`。
///
/// 失败（spawn 失败/超时/非零退出/解析失败/健全性检查不过）返回 `None`，
/// 由调用方回退到硬编码猜测。
#[cfg(unix)]
#[cfg_attr(test, allow(dead_code))]
fn path_from_login_shell() -> Option<String> {
    use std::process::Stdio;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    // 用 `printenv PATH` 而不是 `echo $PATH`：fish shell 里 `$PATH` 是空格分隔的
    // 列表变量，`echo $PATH` 会输出空格分隔的字符串（错的，会把整条 PATH 拆烂）。
    // `printenv PATH` 读的是导出的环境变量，任何 shell 下都是平台分隔符（Unix
    // 冒号）分隔的真实值。
    let script =
        format!("printf '{PATH_BEGIN_MARKER}\\n'; printenv PATH; printf '{PATH_END_MARKER}\\n'");

    let mut cmd = crate::proc::command(shell);
    cmd.arg("-ilc")
        .arg(script)
        // 必须关闭 stdin：否则 rc 文件里若有读 stdin 的逻辑（含交互式 `read`）会
        // 挂住整个 spawn。
        .stdin(Stdio::null())
        // 丢弃 rc 的噪音输出和 `-i` 在非 tty 下可能打印的警告。
        .stderr(Stdio::null())
        .stdout(Stdio::piped());

    let child = cmd.spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    // 超时保护：用户 rc 里可能有慢插件甚至卡住的 `read`，不能无限等。3 秒后
    // child 已被 move 进后台线程拿不回所有权，只能按 pid 硬杀。
    let output = match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Ok(out)) if out.status.success() => out,
        Ok(_) => return None,
        Err(_) => {
            let _ = crate::proc::command("kill")
                .arg("-9")
                .arg(pid.to_string())
                .status();
            return None;
        }
    };

    interpret_shell_stdout(&output.stdout, &|p: &Path| p.is_dir())
}

/// 非测试构建：真的去问 login shell 要 PATH（spawn 一次 `$SHELL -ilc`）。
#[cfg(all(unix, not(test)))]
fn shell_path_or_none() -> Option<String> {
    path_from_login_shell()
}

/// 测试构建下不 spawn 真实 login shell —— 测试必须气密、不依赖机器 rc。
/// 回退链会走 augment_path 分支，那条分支有独立单测覆盖。
#[cfg(all(unix, test))]
fn shell_path_or_none() -> Option<String> {
    None
}

/// `augmented_path_for_spawn` 的缓存：spawn login shell 有 100-500ms 开销，
/// 不能每次 spawn agent 都跑一遍。缓存的是最终结果（含「不追加」的 `None`）。
#[cfg(unix)]
static SPAWN_PATH: std::sync::OnceLock<Option<OsString>> = std::sync::OnceLock::new();

/// 回退链的纯逻辑（不读 env、不 spawn、不碰文件系统，方便测试）：
/// 1. `skip_shell` → 跳过 shell 解析，直接走第 3 步（调试/CI 用；由 `AGENTLOOM_SKIP_SHELL_PATH` 驱动，见 `augmented_path_for_spawn` 注释）。
/// 2. `shell_path` 有值 → 直接用它，不再叠加 `augment_path`——目标是「agent 看到
///    的环境 == 用户终端看到的环境」，用户终端里没有的目录，agent 也不该凭空多出来。
/// 3. 否则 → 若有 `home` 则用 `augment_path` 兜底（硬编码猜测，保底），无 `home`
///    则无法兜底、直接 `None`。shell 解析本身不需要 `home`，所以第 2 步不受
///    `home` 是否存在影响，顺序是「先试 shell、后判 HOME」。
/// 4. 结果与 `current` 相同 → 返回 `None`（避免多余的 `cmd.env`）。
#[cfg(unix)]
fn resolve_spawn_path(
    current: &OsStr,
    skip_shell: bool,
    shell_path: Option<&str>,
    home: Option<&Path>,
    dir_exists: &dyn Fn(&Path) -> bool,
) -> Option<OsString> {
    if !skip_shell {
        if let Some(shell_path) = shell_path {
            let shell_path = OsString::from(shell_path);
            return if shell_path == current {
                None
            } else {
                Some(shell_path)
            };
        }
    }

    let home = home?;
    let augmented = augment_path(current, home, dir_exists);
    if augmented == current {
        None
    } else {
        Some(augmented)
    }
}

#[cfg(unix)]
fn env_flag_enabled(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };

    let value = value.trim();
    if value.is_empty() || value == "0" {
        return false;
    }

    !(value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no"))
}

/// macOS/Linux：GUI app 由 launchd / display manager 启动，不读 shell rc 文件，
/// PATH 残缺。收集真实输入（当前 PATH、skip 开关、shell 解析结果、HOME、目录是否
/// 存在的谓词）后交给纯函数 `resolve_spawn_path` 判定——回退链的语义详见该函数注释。
///
/// skip 开关由环境变量 `AGENTLOOM_SKIP_SHELL_PATH` 驱动：跳过「spawn login shell
/// 解析真实 PATH」这一步（省一次 spawn），直接走 `resolve_spawn_path` 第 3 步的
/// `augment_path` 硬编码兜底分支；调试 / CI / 不希望（或不能）spawn login shell
/// 的环境用。unset、空字符串，或 trim 后不区分大小写等于 `0` / `false` / `no`
/// 的值视为假（不跳过 shell 解析）；其余非空值视为真（跳过）。
///
/// 整个解析只做一次，用 `OnceLock` 缓存。
#[cfg(unix)]
pub(crate) fn augmented_path_for_spawn() -> Option<OsString> {
    SPAWN_PATH
        .get_or_init(|| {
            let current = std::env::var_os("PATH").unwrap_or_default();

            let skip_shell_env = std::env::var("AGENTLOOM_SKIP_SHELL_PATH").ok();
            let skip_shell = env_flag_enabled(skip_shell_env.as_deref());

            // skip_shell 时不必真的去 spawn shell（即便 shell_path_or_none 的结果会被
            // resolve_spawn_path 忽略），省一次不必要的 spawn。
            let shell_path = if skip_shell {
                None
            } else {
                shell_path_or_none()
            };

            let home = std::env::var_os("HOME").map(PathBuf::from);

            resolve_spawn_path(
                &current,
                skip_shell,
                shell_path.as_deref(),
                home.as_deref(),
                &|p: &Path| p.is_dir(),
            )
        })
        .clone()
}

/// 启动时预热 PATH 解析缓存。解析要 spawn 一次 login shell（0.2-3 秒），
/// 而 send_message 是同步 tauri command、跑主线程——不预热的话首次发消息会冻 UI。
/// 在 setup() 的后台线程里调用；结果进 OnceLock，之后所有调用都命中缓存。
pub(crate) fn warm_up_spawn_path() {
    let _ = augmented_path_for_spawn();
}

/// Windows：环境变量存在注册表（`HKCU\Environment`），Explorer 登录时加载，
/// 它启动的任何进程（含双击的 .exe）都继承完整 PATH —— 不存在 macOS 那种
/// 「launchd 不读 shell rc 文件导致 GUI app PATH 残缺」的落差，无需修复，
/// 动它反而是错的。
#[cfg(windows)]
pub(crate) fn augmented_path_for_spawn() -> Option<OsString> {
    None
}

/// harness provider 专属 env 注入：{PREFIX}_API_KEY/{PREFIX}_BASE_URL/{PREFIX}_MODEL（provider 名
/// 大写转下划线；`MYAGENT` 前缀开头的 provider 名跳过，防撞 MYAGENT_API_KEY 等保留名）+ 通用
/// MYAGENT_* 别名 + search key/backend + 流式空闲超时。`HarnessBackend::build_command_inner`
/// （Normal/Worker/…）与 L3 队长装配 `harness_lead_cmd_in`（lib.rs）共用同一份——
/// 顺序/过滤条件必须逐字节对齐，别各写一份。
///
/// `MYAGENT_TIMEOUT_SECS`：`agents.api_timeout_ms`（毫秒，用户在 GUI 配的超时）向上取整转秒，
/// 下限 1——引擎侧非法值 / 0 会硬报错起不来（`ea9ac648`/`bfc4b210`）。`None` 或 ≤0 时不设该变量，
/// 让引擎默认 120 秒生效。
pub(crate) fn apply_harness_provider_env(
    cmd: &mut Command,
    profile: &AgentProfile,
    api_key: Option<&str>,
    search_api_key: Option<&str>,
    search_backend: Option<&str>,
) {
    let env_prefix = profile.provider.to_ascii_uppercase().replace('-', "_");
    let provider_env = !env_prefix.starts_with("MYAGENT");
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        cmd.env("MYAGENT_API_KEY", key);
        if provider_env {
            cmd.env(format!("{env_prefix}_API_KEY"), key);
        }
    }
    if let Some(key) = search_api_key.filter(|k| !k.trim().is_empty()) {
        cmd.env("MYAGENT_SEARCH_API_KEY", key);
    }
    if let Some(backend) = search_backend.filter(|b| !b.trim().is_empty()) {
        cmd.env("MYAGENT_SEARCH_BACKEND", backend);
    }
    if let Some(endpoint) = profile.endpoint.as_deref().filter(|e| !e.is_empty()) {
        cmd.env("MYAGENT_BASE_URL", endpoint);
        if provider_env {
            cmd.env(format!("{env_prefix}_BASE_URL"), endpoint);
        }
    }
    if let Some(model) = profile.primary_model.as_deref().filter(|m| !m.is_empty()) {
        cmd.env("MYAGENT_MODEL", model);
        if provider_env {
            cmd.env(format!("{env_prefix}_MODEL"), model);
        }
    }
    if let Some(timeout_ms) = profile.api_timeout_ms.filter(|ms| *ms > 0) {
        // `i64::div_ceil` 的有符号版本在当前工具链未稳定（int_roundings，仅无符号已稳）；
        // 已用 `filter(*ms > 0)` 保证非负，转 u64 走稳定实现。
        let timeout_secs = (timeout_ms as u64).div_ceil(1000).max(1);
        cmd.env("MYAGENT_TIMEOUT_SECS", timeout_secs.to_string());
    }
}

/// member worker 的回合预算：与 `HARNESS_LEAD_MAX_TURNS`（lib.rs）对齐——引擎默认 40 轮是给
/// 「一次性写代码」的假设调的，对真实任务结构性偏小，常在没写完就先撞上引擎自己的预算耗尽
/// 机制（`stopReason.budgetExhaustedStillProgressing`）。放宽到 120 轮，只影响 Worker 模式命令。
const HARNESS_MEMBER_MAX_TURNS: &str = "120";

static HARNESS_PROMPT_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
const HARNESS_PROMPT_FILE_MAX_AGE: Duration = Duration::from_secs(60 * 60);

fn cleanup_expired_harness_prompt_files(prompts_dir: &Path, max_age: Duration, now: SystemTime) {
    let entries = match std::fs::read_dir(prompts_dir) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!(
                "harness prompt 临时文件清理失败（non-fatal，{}）：{error}",
                prompts_dir.display()
            );
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!(
                    "harness prompt 临时文件条目读取失败（non-fatal，{}）：{error}",
                    prompts_dir.display()
                );
                continue;
            }
        };
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                eprintln!(
                    "harness prompt 临时文件元数据读取失败（non-fatal，{}）：{error}",
                    path.display()
                );
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                eprintln!(
                    "harness prompt 临时文件修改时间读取失败（non-fatal，{}）：{error}",
                    path.display()
                );
                continue;
            }
        };
        if now.duration_since(modified).is_ok_and(|age| age > max_age) {
            if let Err(error) = std::fs::remove_file(&path) {
                eprintln!(
                    "harness prompt 过期临时文件清理失败（non-fatal，{}）：{error}",
                    path.display()
                );
            }
        }
    }
}

/// harness 的位置参数天然支持文件输入；统一落到 app 域，避免 prompt 进入 argv 撞系统上限，
/// 也消除短 prompt 恰好等于现存路径时被引擎误读的歧义。构造期只回收超过一小时的旧文件，
/// 绝不清理可能尚未被引擎读取的在途文件；session purge 仍会随 journal 目录一并回收。
pub(crate) fn write_harness_prompt_file(session_id: &str, prompt: &str) -> Result<PathBuf, String> {
    let prompt_len = prompt.len();
    let session_id = safe_id(session_id)?;
    let prompts_dir = crate::worktree::journals_dir()
        .join(session_id)
        .join("prompts");

    std::fs::create_dir_all(&prompts_dir).map_err(|error| {
        crate::ui_msg::al_err(
            "agent.promptFileDirCreateFailed",
            &[(
                "detail",
                format!(
                    "prompt {prompt_len} bytes，{}：{error}",
                    prompts_dir.display()
                ),
            )],
        )
    })?;
    cleanup_expired_harness_prompt_files(
        &prompts_dir,
        HARNESS_PROMPT_FILE_MAX_AGE,
        SystemTime::now(),
    );

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = HARNESS_PROMPT_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let prompt_path = prompts_dir.join(format!(
        "prompt-{timestamp}-{}-{counter}.txt",
        std::process::id()
    ));
    let mut prompt_file_options = std::fs::OpenOptions::new();
    prompt_file_options.write(true).create_new(true);
    #[cfg(unix)]
    prompt_file_options.mode(0o600);
    let mut prompt_file = prompt_file_options.open(&prompt_path).map_err(|error| {
        crate::ui_msg::al_err(
            "agent.promptFileCreateFailed",
            &[(
                "detail",
                format!(
                    "prompt {prompt_len} bytes，{}：{error}",
                    prompt_path.display()
                ),
            )],
        )
    })?;
    prompt_file.write_all(prompt.as_bytes()).map_err(|error| {
        crate::ui_msg::al_err(
            "agent.promptFileWriteFailed",
            &[(
                "detail",
                format!(
                    "prompt {prompt_len} bytes，{}：{error}",
                    prompt_path.display()
                ),
            )],
        )
    })?;
    Ok(prompt_path)
}

impl AgentBackend for HarnessBackend {
    fn build_command_inner(&self, ctx: &BuildContext) -> Result<Command, String> {
        harness_attachments::build_command(self, ctx)
    }

    fn parse_fn(&self) -> ParseFn {
        if harness_plan_mode_enabled() {
            ParseFn::HarnessPlan
        } else {
            ParseFn::Harness
        }
    }
}

fn set_model_env(cmd: &mut Command, key: &str, model: Option<&str>, fallback: Option<&str>) {
    if let Some(model) = model.filter(|model| !model.is_empty()).or(fallback) {
        cmd.env(key, model);
    }
}

#[cfg(test)]
mod tests;
