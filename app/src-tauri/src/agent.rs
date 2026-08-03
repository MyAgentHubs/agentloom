pub use crate::db::AgentProfile;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

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

pub trait AgentBackend {
    fn build_command(&self, ctx: &BuildContext) -> Result<Command, String> {
        let mut cmd = self.build_command_inner(ctx)?;
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        Ok(cmd)
    }
    fn build_command_inner(&self, ctx: &BuildContext) -> Result<Command, String>;
    fn parse_fn(&self) -> ParseFn;
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

fn system_prompt_for_mode(mode: BuildMode) -> Option<&'static str> {
    match mode {
        BuildMode::Normal => None,
        BuildMode::Summarize => None,
        BuildMode::Worker => Some(crate::WORKER_ONESHOT_PROMPT),
        BuildMode::LeadDraft => Some(crate::lead_draft::LEAD_DRAFT_SYS_PROMPT),
        BuildMode::LeadAction => Some(crate::lead_step::LEAD_DECISION_SYS_PROMPT),
    }
}

const CODEX_IMAGE_OUTPUT_INSTRUCTION: &str = "If you generate any image files, save or copy them into the current workspace and state each image's absolute path in your final reply. Do not leave generated images only under $CODEX_HOME/generated_images.";

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
                let (mut cmd, claude_bin) =
                    crate::claude_sandboxed_cmd_in(ctx.wt, ctx.prompt, &extra_ref)?;
                crate::log_claude_bin(ctx.session_id, &claude_bin);
                scrub_checkpoint_env(&mut cmd);
                crate::apply_clean_env(&mut cmd);
                if let Some(hook) = hook {
                    cmd.env(crate::checkpoint_hook::TOKEN_ENV, hook.token);
                }
                Ok(cmd)
            }
            "codex" => {
                let mut cmd = crate::proc::command("codex");
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
                let prompt = prompt_for_mode(ctx.mode, ctx.prompt);
                let sandbox = if matches!(
                    ctx.mode,
                    BuildMode::LeadDraft | BuildMode::LeadAction | BuildMode::Summarize
                ) {
                    "read-only"
                } else {
                    "workspace-write"
                };
                cmd.args([
                    "exec",
                    "--json",
                    "--ignore-user-config",
                    "--skip-git-repo-check",
                    "--sandbox",
                    sandbox,
                    prompt.as_str(),
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
        let mut extra: Vec<String> = vec![
            "--disable-slash-commands".to_string(),
            "--append-system-prompt".to_string(),
            system_prompt,
        ];
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
        let (mut cmd, claude_bin) = crate::claude_sandboxed_cmd_in(ctx.wt, ctx.prompt, &extra_ref)?;
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
/// MYAGENT_* 别名 + search key/backend。`HarnessBackend::build_command_inner`（Normal/Worker/…）与
/// L3 队长装配 `harness_lead_cmd_in`（lib.rs）共用同一份——顺序/过滤条件必须逐字节对齐，别各写一份。
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
}

/// member worker 的回合预算：与 `HARNESS_LEAD_MAX_TURNS`（lib.rs）对齐——引擎默认 40 轮是给
/// 「一次性写代码」的假设调的，对真实任务结构性偏小，常在没写完就先撞上引擎自己的预算耗尽
/// 机制（`stopReason.budgetExhaustedStillProgressing`）。放宽到 120 轮，只影响 Worker 模式命令。
const HARNESS_MEMBER_MAX_TURNS: &str = "120";

impl AgentBackend for HarnessBackend {
    fn build_command_inner(&self, ctx: &BuildContext) -> Result<Command, String> {
        let mut cmd = crate::proc::command(resolve_myagent_bin());
        if let Some(path) = augmented_path_for_spawn() {
            cmd.env("PATH", path);
        }
        let plan_mode = harness_plan_mode_enabled();
        cmd.arg(if plan_mode { "plan" } else { "run" })
            .arg(ctx.prompt)
            .arg("--jsonl")
            .args(["--provider", self.profile.provider.as_str()])
            .args(["--permission", harness_permission_for_mode(ctx.mode)])
            .arg("--workspace")
            .arg(ctx.wt)
            .arg("--journal-dir")
            .arg(crate::worktree::journals_dir().join(ctx.session_id));
        if !plan_mode {
            if let Some(disallowed_tools) = harness_read_only_disallowed_tools(ctx.mode) {
                cmd.args(["--disallow-tools", disallowed_tools]);
            }
        }
        if ctx.mode == BuildMode::Worker {
            cmd.args(["--max-turns", HARNESS_MEMBER_MAX_TURNS]);
        }
        if !plan_mode {
            cmd.args(["--client-session-id", ctx.session_id]);
        }
        for c in ctx.criteria {
            cmd.arg("--criteria").arg(c);
        }
        apply_harness_provider_env(
            &mut cmd,
            &self.profile,
            self.api_key.as_deref(),
            self.search_api_key.as_deref(),
            self.search_backend.as_deref(),
        );
        let hook = checkpoint_hook_for_mode(ctx)?;
        configure_harness_checkpoint_env(&mut cmd, hook.as_ref());
        Ok(cmd)
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
mod tests {
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

    fn set_checkpoint_envs_for_test(
        endpoint: Option<&str>,
        token: Option<&str>,
    ) -> CheckpointEnvGuard {
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
            args.iter().any(|arg| arg == "draft this"),
            "expected original prompt as Claude prompt arg: {args:?}"
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
            let prompt_arg = args.last().expect("codex prompt arg should exist");

            assert!(
                prompt_arg.contains(IMAGE_OUTPUT_INSTRUCTION),
                "expected image persistence instruction for {mode:?}: {args:?}"
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
            let prompt_arg = args.last().expect("codex prompt arg should exist");

            assert!(
                !prompt_arg.contains(IMAGE_OUTPUT_INSTRUCTION),
                "did not expect image persistence instruction for {mode:?}: {args:?}"
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
                "agent::tests::native_codex_command_uses_augmented_path",
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
        let prompt_arg = args.last().expect("codex prompt arg should exist");

        assert!(
            prompt_arg.contains(crate::lead_step::LEAD_DECISION_SYS_PROMPT),
            "expected lead action system prompt in Codex prompt arg: {args:?}"
        );
        assert!(
            prompt_arg.ends_with("decide next"),
            "expected user prompt after system prompt: {args:?}"
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
        assert!(
            args.iter().any(|a| a == "fix the bug"),
            "prompt positional: {args:?}"
        );
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
        assert!(
            args.iter().any(|a| a == "fix the bug"),
            "prompt positional: {args:?}"
        );
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
    fn resolve_bin_env_wins_over_sidecar() {
        let resolved = resolve_myagent_bin_from(
            Some("/custom/path/myagent"),
            Some(Path::new("/Applications/AgentLoom.app/Contents/MacOS")),
            MyagentSidecarPlatform::MacOs,
            |_| true,
        );

        assert_eq!(resolved, PathBuf::from("/custom/path/myagent"));
    }

    #[test]
    fn resolve_bin_finds_sidecar_in_macos_app_bundle() {
        let exe_dir = Path::new("/Applications/AgentLoom.app/Contents/MacOS");
        let sidecar = exe_dir.join("myagent");
        let resolved =
            resolve_myagent_bin_from(None, Some(exe_dir), MyagentSidecarPlatform::MacOs, |path| {
                path == sidecar
            });

        assert_eq!(resolved, sidecar);
    }

    #[test]
    fn resolve_bin_falls_back_to_path_when_no_sidecar() {
        let resolved = resolve_myagent_bin_from(
            None,
            Some(Path::new("/Applications/AgentLoom.app/Contents/MacOS")),
            MyagentSidecarPlatform::MacOs,
            |_| false,
        );

        assert_eq!(resolved, PathBuf::from("myagent"));
    }

    #[test]
    fn resolve_bin_ignores_blank_env() {
        let exe_dir = Path::new("/Applications/AgentLoom.app/Contents/MacOS");
        let sidecar = exe_dir.join("myagent");

        assert_eq!(
            resolve_myagent_bin_from(
                Some(""),
                Some(exe_dir),
                MyagentSidecarPlatform::MacOs,
                |path| path == sidecar
            ),
            sidecar
        );
        assert_eq!(
            resolve_myagent_bin_from(
                Some("   "),
                Some(exe_dir),
                MyagentSidecarPlatform::MacOs,
                |path| path == sidecar
            ),
            sidecar
        );
    }

    #[test]
    fn resolve_bin_sidecar_must_be_a_file_not_a_dir() {
        let resolved = resolve_myagent_bin_from(
            None,
            Some(Path::new("/Applications/AgentLoom.app/Contents/MacOS")),
            MyagentSidecarPlatform::MacOs,
            // 注入的文件类型检查把目录/缺失/读失败统一当作 false。
            |_| false,
        );

        assert_eq!(resolved, PathBuf::from("myagent"));
    }

    #[test]
    fn resolve_bin_no_exe_dir_and_no_env_falls_back_to_path() {
        let resolved =
            resolve_myagent_bin_from(None, None, MyagentSidecarPlatform::Windows, |_| true);

        assert_eq!(resolved, PathBuf::from("myagent"));
    }

    #[test]
    fn resolve_bin_finds_windows_installed_sidecar_next_to_main_exe() {
        let exe_dir = Path::new("C:/Program Files/AgentLoom");
        let sidecar = exe_dir.join("myagent.exe");
        let resolved = resolve_myagent_bin_from(
            None,
            Some(exe_dir),
            MyagentSidecarPlatform::Windows,
            |path| path == sidecar,
        );

        assert_eq!(resolved, sidecar);
    }

    #[test]
    fn resolve_bin_windows_sidecar_must_be_a_file() {
        let exe_dir = Path::new("C:/Users/test/AppData/Local/AgentLoom");
        let resolved =
            resolve_myagent_bin_from(None, Some(exe_dir), MyagentSidecarPlatform::Windows, |_| {
                false
            });

        assert_eq!(resolved, PathBuf::from("myagent"));
    }

    /// 回归锁：Windows dev / 直跑 release binary 时，tauri-build 放到 Cargo output
    /// 的同名 `myagent.exe` 也绝不能被当作安装 sidecar 命中。
    #[test]
    fn resolve_bin_ignores_windows_sidecar_in_cargo_target_profiles() {
        for exe_dir in [
            Path::new("C:/repo/app/src-tauri/target/debug"),
            Path::new("C:/repo/app/src-tauri/target/release"),
            Path::new("C:/repo/app/src-tauri/target/x86_64-pc-windows-msvc/debug"),
            Path::new("C:/repo/app/src-tauri/target/x86_64-pc-windows-msvc/release"),
        ] {
            let resolved = resolve_myagent_bin_from(
                None,
                Some(exe_dir),
                MyagentSidecarPlatform::Windows,
                |_| true,
            );

            assert_eq!(resolved, PathBuf::from("myagent"), "{}", exe_dir.display());
        }
    }

    #[test]
    fn resolve_bin_ignores_macos_sidecar_outside_app_bundle() {
        let resolved = resolve_myagent_bin_from(
            None,
            Some(Path::new("/repo/app/src-tauri/target/release")),
            MyagentSidecarPlatform::MacOs,
            |_| true,
        );

        assert_eq!(resolved, PathBuf::from("myagent"));
    }

    #[test]
    fn resolve_bin_does_not_enable_same_dir_sidecar_on_other_platforms() {
        let resolved = resolve_myagent_bin_from(
            None,
            Some(Path::new("/opt/agentloom")),
            MyagentSidecarPlatform::Other,
            |_| true,
        );

        assert_eq!(resolved, PathBuf::from("myagent"));
    }

    #[cfg(unix)]
    #[test]
    fn augment_path_appends_missing_dirs_in_order_when_all_exist() {
        let result = augment_path(
            OsStr::new("/usr/bin:/bin"),
            Path::new("/Users/x"),
            &|_p: &Path| true,
        );

        let expected = std::env::join_paths([
            "/usr/bin",
            "/bin",
            "/Users/x/.local/bin",
            "/opt/homebrew/bin",
            "/opt/homebrew/sbin",
            "/usr/local/bin",
            "/Users/x/.cargo/bin",
        ])
        .unwrap();
        assert_eq!(result, expected);
    }

    #[cfg(unix)]
    #[test]
    fn augment_path_does_not_duplicate_dir_already_present() {
        let result = augment_path(
            OsStr::new("/usr/bin:/opt/homebrew/bin"),
            Path::new("/Users/x"),
            &|_p: &Path| true,
        );

        let segments: Vec<PathBuf> = std::env::split_paths(&result).collect();
        // 只出现一次
        assert_eq!(
            segments
                .iter()
                .filter(|p| p.as_path() == Path::new("/opt/homebrew/bin"))
                .count(),
            1
        );
        // 仍在原位置（第二段），未被挪到末尾
        assert_eq!(segments[1], Path::new("/opt/homebrew/bin"));
    }

    #[cfg(unix)]
    #[test]
    fn augment_path_skips_dirs_that_do_not_exist() {
        let result = augment_path(
            OsStr::new("/usr/bin:/bin"),
            Path::new("/Users/x"),
            &|p: &Path| p == Path::new("/opt/homebrew/bin"),
        );

        let expected = std::env::join_paths(["/usr/bin", "/bin", "/opt/homebrew/bin"]).unwrap();
        assert_eq!(result, expected);
    }

    #[cfg(unix)]
    #[test]
    fn augment_path_unchanged_when_no_candidate_dirs_exist() {
        let current = OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin");
        let result = augment_path(current, Path::new("/Users/x"), &|_p: &Path| false);

        assert_eq!(result, current);
    }

    #[cfg(unix)]
    #[test]
    fn augment_path_empty_current_has_no_leading_colon() {
        let result = augment_path(OsStr::new(""), Path::new("/Users/x"), &|_p: &Path| true);

        let result_str = result.to_string_lossy();
        assert!(!result_str.starts_with(':'));
        assert!(result_str.starts_with("/Users/x/.local/bin"));
    }

    /// 回归锁：dev 模式下用户 shell 的 PATH 已包含全部 5 个候选目录时，
    /// 结果必须逐字节等于输入——不产生任何变化（不重复追加、不重排）。
    #[cfg(unix)]
    #[test]
    fn augment_path_dev_mode_no_change_when_all_candidates_already_present() {
        let current = OsStr::new(
            "/usr/bin:/bin:/usr/sbin:/sbin:/Users/x/.local/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/Users/x/.cargo/bin",
        );

        let result = augment_path(current, Path::new("/Users/x"), &|_p: &Path| true);

        assert_eq!(result, current);
    }

    /// 替代旧版「HOME 含冒号会把 PATH 拆坏」的手工防御测试：`join_paths` 对任一
    /// 候选路径含分隔符的情况返回 `Err`，我们据此原样返回 `current`、不做任何
    /// 修改——这就是新实现对付「HOME 含分隔符」这类边界情况的方式。
    #[cfg(unix)]
    #[test]
    fn augment_path_returns_current_unchanged_when_join_paths_fails() {
        let current = OsStr::new("/usr/bin:/bin");
        let home = Path::new("/Users/a:b"); // 含冒号 —— 拼出的候选目录本身就含分隔符

        let result = augment_path(current, home, &|_p: &Path| true);

        assert_eq!(result, current);
    }

    /// 期望值改用 `std::env::join_paths` 构造而非手写冒号字符串——测试本身也不
    /// 硬编码分隔符，天然对 Windows 的分号分隔符同样成立。
    #[cfg(unix)]
    #[test]
    fn augment_path_uses_platform_separator() {
        let current = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();

        let result = augment_path(&current, Path::new("/Users/x"), &|_p: &Path| true);

        let expected = std::env::join_paths([
            "/usr/bin",
            "/bin",
            "/Users/x/.local/bin",
            "/opt/homebrew/bin",
            "/opt/homebrew/sbin",
            "/usr/local/bin",
            "/Users/x/.cargo/bin",
        ])
        .unwrap();
        assert_eq!(result, expected);
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_normal_single_line() {
        let stdout = "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(result, Some("/usr/bin:/bin".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_ignores_banner_before_begin_marker() {
        let stdout = "Welcome to neofetch!\nSome banner line\n\n__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(result, Some("/usr/bin:/bin".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_ignores_noise_after_end_marker() {
        let stdout =
            "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\nbye now\nmore noise\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(result, Some("/usr/bin:/bin".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_missing_begin_marker_returns_none() {
        let stdout = "/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(result, None);
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_missing_end_marker_returns_none() {
        let stdout = "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(result, None);
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_empty_between_markers_returns_none() {
        let stdout = "__AGENTLOOM_PATH_BEGIN__\n__AGENTLOOM_PATH_END__\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(result, None);
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_whitespace_only_between_markers_returns_none() {
        let stdout = "__AGENTLOOM_PATH_BEGIN__\n   \n\t\n  \n__AGENTLOOM_PATH_END__\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(result, None);
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_multiple_lines_takes_first_non_empty() {
        let stdout =
            "__AGENTLOOM_PATH_BEGIN__\n\n/usr/bin:/bin\n/some/other/line\n__AGENTLOOM_PATH_END__\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(result, Some("/usr/bin:/bin".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_multiple_marker_groups_takes_first() {
        let stdout = "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n__AGENTLOOM_PATH_BEGIN__\n/should/not/be/used\n__AGENTLOOM_PATH_END__\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(result, Some("/usr/bin:/bin".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn parse_shell_path_output_preserves_paths_with_spaces() {
        let stdout =
            "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/Applications/My App/bin\n__AGENTLOOM_PATH_END__\n";
        let result = parse_shell_path_output(stdout);
        assert_eq!(
            result,
            Some("/usr/bin:/Applications/My App/bin".to_string())
        );
    }

    /// unix-only：这些测试针对 shell PATH 解析链路，Windows 下相关函数不存在
    /// （Windows 从注册表继承完整 PATH，无需修复，见 augmented_path_for_spawn 的 windows 分支）。
    #[cfg(unix)]
    mod unix_path_resolution_tests {
        use super::*;

        #[test]
        fn env_flag_enabled_uses_semantic_boolean_values() {
            for value in [
                Some("1"),
                Some("true"),
                Some("yes"),
                Some("随便什么乱七八糟的字符串"),
            ] {
                assert!(env_flag_enabled(value), "{value:?} should be enabled");
            }

            for value in [
                None,
                Some(""),
                Some("0"),
                Some("false"),
                Some("no"),
                Some("FALSE"),
                Some("No"),
                Some(" 0 "),
            ] {
                assert!(!env_flag_enabled(value), "{value:?} should be disabled");
            }
        }

        #[test]
        fn tests_never_spawn_login_shell() {
            assert!(
                shell_path_or_none().is_none(),
                "测试构建下不得 spawn 真实 login shell"
            );
        }

        #[test]
        fn resolve_spawn_path_skip_shell_ignores_shell_path_and_falls_back_to_augment() {
            let current = OsStr::new("/usr/bin:/bin");
            let home = Path::new("/Users/x");
            let dir_exists = |_p: &Path| true;

            let result = resolve_spawn_path(
                current,
                true,
                Some("/shell/only/path"),
                Some(home),
                &dir_exists,
            );

            let expected = augment_path(current, home, &dir_exists);
            assert_eq!(result, Some(expected));
        }

        #[test]
        fn resolve_spawn_path_shell_success_does_not_overlay_augment_path() {
            let current = OsStr::new("/usr/bin:/bin");

            let result = resolve_spawn_path(
                current,
                false,
                Some("/a:/b"),
                Some(Path::new("/Users/x")),
                &|_p: &Path| true,
            );

            assert_eq!(result, Some(OsString::from("/a:/b")));
        }

        #[test]
        fn resolve_spawn_path_falls_back_to_augment_path_when_shell_path_none() {
            let current = OsStr::new("/usr/bin:/bin");
            let home = Path::new("/Users/x");
            let dir_exists = |_p: &Path| true;

            let result = resolve_spawn_path(current, false, None, Some(home), &dir_exists);

            let expected = augment_path(current, home, &dir_exists);
            assert_eq!(result, Some(expected));
        }

        #[test]
        fn resolve_spawn_path_none_when_no_shell_and_no_home() {
            let current = OsStr::new("/usr/bin:/bin");

            let result = resolve_spawn_path(current, false, None, None, &|_p: &Path| true);

            assert_eq!(result, None);
        }

        #[test]
        fn resolve_spawn_path_none_when_shell_path_equals_current() {
            let current = OsStr::new("/usr/bin:/bin");

            let result = resolve_spawn_path(
                current,
                false,
                Some("/usr/bin:/bin"),
                Some(Path::new("/Users/x")),
                &|_p: &Path| true,
            );

            assert_eq!(result, None);
        }

        #[test]
        fn resolve_spawn_path_none_when_augment_path_no_change() {
            let current = OsStr::new("/usr/bin:/bin");
            let home = Path::new("/Users/x");

            let result = resolve_spawn_path(current, false, None, Some(home), &|_p: &Path| false);

            assert_eq!(result, None);
        }

        #[test]
        fn resolve_spawn_path_uses_shell_path_even_without_home() {
            let current = OsStr::new("/usr/bin:/bin");

            let result = resolve_spawn_path(current, false, Some("/a:/b"), None, &|_p: &Path| true);

            assert_eq!(result, Some(OsString::from("/a:/b")));
        }

        #[test]
        fn interpret_shell_stdout_normal_returns_path() {
            let stdout: &[u8] =
                b"__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n";

            let result = interpret_shell_stdout(stdout, &|_p: &Path| true);

            assert_eq!(result, Some("/usr/bin:/bin".to_string()));
        }

        /// 健全性检查生效：marker 圈定的 PATH 条目全部不存在 → 回退比信它更安全，返回 None。
        #[test]
        fn interpret_shell_stdout_sanity_check_rejects_when_no_real_dir() {
            let stdout: &[u8] =
                b"__AGENTLOOM_PATH_BEGIN__\n/nonexistent/a:/nonexistent/b\n__AGENTLOOM_PATH_END__\n";

            let result = interpret_shell_stdout(stdout, &|_p: &Path| false);

            assert_eq!(result, None);
        }

        /// 健全性检查只要一个条目命中就整体通过——返回完整 PATH，不是只返回命中的那个条目。
        #[test]
        fn interpret_shell_stdout_sanity_check_passes_with_one_real_dir() {
            let stdout: &[u8] = b"__AGENTLOOM_PATH_BEGIN__\n/nonexistent/a:/usr/bin:/nonexistent/b\n__AGENTLOOM_PATH_END__\n";

            let result = interpret_shell_stdout(stdout, &|p: &Path| p == Path::new("/usr/bin"));

            assert_eq!(
                result,
                Some("/nonexistent/a:/usr/bin:/nonexistent/b".to_string())
            );
        }

        #[test]
        fn interpret_shell_stdout_missing_marker_returns_none() {
            let stdout: &[u8] = b"/usr/bin:/bin\n";

            let result = interpret_shell_stdout(stdout, &|_p: &Path| true);

            assert_eq!(result, None);
        }

        #[test]
        fn interpret_shell_stdout_empty_between_markers_returns_none() {
            let stdout: &[u8] = b"__AGENTLOOM_PATH_BEGIN__\n__AGENTLOOM_PATH_END__\n";

            let result = interpret_shell_stdout(stdout, &|_p: &Path| true);

            assert_eq!(result, None);
        }

        /// 非 UTF-8 字节回归锁：`from_utf8_lossy` 把每个非法字节各自替换成一个
        /// U+FFFD（替换字符）——0xFF、0xFE 在任何位置都不是合法 UTF-8 序列的一部分，
        /// 各自单独构成一个「最大非法子序列」，所以两个非法字节产生两个 U+FFFD，
        /// 不会合并成一个。
        ///
        /// 这条锁住的是已知可接受行为（reviewer 已分析过、非待修 bug）：非 UTF-8 路径
        /// 被 lossy 解码后变成一个不存在的垃圾目录条目，效果是「该目录下的工具找不到」，
        /// 优雅降级——而不是安全问题。因为 U+FFFD 的 UTF-8 编码是 `EF BF BD`，不含
        /// Unix 的 `:` 或 Windows 的 `;` 这两个平台路径分隔符字节，所以替换字符不会
        /// 注入分隔符、不会把一个 PATH 条目错误劈成两个——split_paths 之后仍是原来
        /// 的条目数。
        #[test]
        fn interpret_shell_stdout_non_utf8_bytes_become_replacement_char() {
            let stdout: &[u8] =
                b"__AGENTLOOM_PATH_BEGIN__\n/usr/\xFF\xFEbin\n__AGENTLOOM_PATH_END__\n";

            let result = interpret_shell_stdout(stdout, &|_p: &Path| true);

            let path = result.expect("dir_exists 恒 true，健全性检查应通过");
            assert_eq!(path, "/usr/\u{FFFD}\u{FFFD}bin");
            assert_eq!(path.matches('\u{FFFD}').count(), 2);
            // 替换字符没有引入分隔符：split_paths 后仍是单个 PATH 条目，不是两个。
            assert_eq!(std::env::split_paths(&path).count(), 1);
        }

        /// banner 干扰 + 健全性检查同时生效的组合场景：marker 前后都有噪音，
        /// 圈定的 PATH 里有一个不存在的条目和一个真实存在的条目。
        #[test]
        fn interpret_shell_stdout_banner_noise_and_sanity_check_combined() {
            let stdout: &[u8] = b"Welcome to neofetch!\nSome banner line\n\n__AGENTLOOM_PATH_BEGIN__\n/nonexistent/a:/usr/bin\n__AGENTLOOM_PATH_END__\nbye now\nmore noise\n";

            let result = interpret_shell_stdout(stdout, &|p: &Path| p == Path::new("/usr/bin"));

            assert_eq!(result, Some("/nonexistent/a:/usr/bin".to_string()));
        }
    }
}
