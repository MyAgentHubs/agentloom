use super::*;

pub struct NativeBackend {
    pub provider: String,
    pub primary_model: Option<String>,
}

/// On Windows, an npm-installed Codex provides only `codex.cmd` (not `codex.exe`), while
/// `Command::new("codex")` only adds `.exe` and does not search PATHEXT, so the bare name can
/// never find it. Use the PATHEXT-aware detection path here to obtain an absolute path, then let
/// proc::command replace the wrapper with the real interpreter.
/// On non-Windows platforms, keep the bare name "codex" so PATH and augmented PATH behavior is
/// byte-for-byte unchanged.
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

/// Connect solo commands to the in-process MCP. Claude MCP arguments can be appended to the
/// command, while Codex `-c` options are global arguments and must appear with the existing
/// `-a`, `-m`, and `-c` options before the `exec` subcommand.
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

/// Image-output guidance for solo and Normal sessions: reference generated images with Markdown
/// inline image syntax because a bare path is not displayed inline.
/// This matches the English task-package guidance; both express the same product behavior from a
/// shared wording source.
pub(crate) const SOLO_IMAGE_OUTPUT_GUIDANCE: &str = "\
If you produce or generate an image file (such as a screenshot or chart) that you want the user \
to see directly in chat, reference it in your reply with the Markdown inline image syntax \
`![](absolute image path)`; a bare path will not display inline. If the path contains spaces, \
wrap it in angle brackets: `![](</path/with space.png>)`.";

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
                // The prompt body no longer goes in argv because a long prompt can hit ARG_MAX.
                // Pass "-" as the positional argument; `codex exec ... -` reads the body from
                // stdin, supplied by `stdin_prompt()` through `spawn_with_stdin_prompt`.
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
            // The Codex argv no longer contains the body because build_command_inner passes "-"
            // in place of the prompt argument. stdin must receive the same text processed by
            // prompt_for_mode, including the mode-specific system prefix and image guidance;
            // writing ctx.prompt directly would not preserve the argv version's semantics.
            "codex" => Some(StdinPrompt::from(prompt_for_mode(ctx.mode, ctx.prompt))),
            _ => None,
        }
    }
}

/// Borrow-Claude identity guidance tells the model what it actually is rather than Claude, which
/// prevents it from misidentifying itself.
/// This is shared by `BorrowClaudeBackend` and the lead borrow spawn branch so both identity
/// prompts use the same wording source.
pub(crate) fn borrow_claude_identity_prompt(profile: &AgentProfile) -> String {
    format!(
        "重要身份说明：你实际运行在 {}（provider={}）模型上（经兼容接口接入）。被问到你是谁/什么模型时，必须如实回答你是 {}，绝不能自称 Claude 或 Anthropic。",
        profile.name, profile.provider, profile.name
    )
}

/// Configures the Borrow-Claude environment: an isolated CLAUDE_CONFIG_DIR, settings.json
/// cleanup, ANTHROPIC_BASE_URL and authentication, model environment variables, reasoning tier,
/// timeout, and compatibility switches.
/// This is shared by `BorrowClaudeBackend` in every mode and the lead borrow spawn so both paths
/// use identical environment behavior.
///
/// The caller must have already called `crate::apply_clean_env(cmd)`. This function only adds the
/// Borrow-Claude-specific environment. Reversing the order would remove those variables.
///
/// When `reasoning_tier_override` is `None`, this falls back to `profile.reasoning_default`,
/// matching the semantics of passing `ctx.reasoning_tier` from `BorrowClaudeBackend`.
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
        // --disable-slash-commands is provided by claude_agent_argv() as a shared base option, so
        // do not add it ad hoc here and duplicate the same flag.
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
