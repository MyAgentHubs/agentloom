use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::config::{self, StoredProvider};
use crate::error::{HarnessError, Result};
use crate::events::OutputMode;
use crate::judge::{LlmJudge, NoopJudge};
use crate::memory::learn::pipeline::run_learn_pipeline;
use crate::memory::lesson::{Lesson, LessonSource, LessonStatus};
use crate::memory::MemoryStore;
use crate::orchestrator::{
    request_interrupt, resume_solo_with_judge_and_fs_scope, run_solo_with_judge, ControlInputKind,
    RunOptions, RunOutcome,
};
use crate::plan::run_plan::{resume_plan, run_plan, PlanRunOptions};
use crate::provider::mock::MockProvider;
use crate::provider::openai_compatible::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use crate::provider::ProviderClient;
use crate::shell::PermissionPolicy;

/// 按 config 检测协议·建匹配的 provider·绑到 $client 跑 $body。保持 run_solo_with_judge<P> 单态化。
macro_rules! with_detected_provider {
    ($config:expr, $proto_override:expr, |$client:ident| $body:block) => {{
        let __cfg = $config;
        let __proto = crate::config::detect_provider_protocol(
            &__cfg.provider_id,
            &__cfg.base_url,
            $proto_override,
        )?;
        match __proto {
            crate::config::Protocol::Anthropic => {
                let $client = crate::provider::anthropic_compatible::AnthropicProvider::new(__cfg)?;
                $body
            }
            crate::config::Protocol::OpenAi => {
                let $client = OpenAiCompatibleProvider::new(__cfg)?;
                $body
            }
        }
    }};
}

/// 读 {PREFIX}_PROTOCOL 显式协议覆盖（沿用现有 env_prefix 约定）。
fn protocol_override_for(provider: &str) -> Option<String> {
    let prefix = provider.to_ascii_uppercase().replace('-', "_");
    std::env::var(format!("{prefix}_PROTOCOL")).ok()
}

/// clap value_parser for `--mcp-server <name>=<url>`. `name` must be
/// non-empty and `url` must be an http/https Streamable HTTP endpoint.
fn parse_mcp_server_flag(raw: &str) -> std::result::Result<(String, String), String> {
    let (name, url) = raw.split_once('=').ok_or_else(|| {
        format!("invalid --mcp-server value '{raw}': expected format <name>=<url>")
    })?;
    if name.is_empty() {
        return Err(format!(
            "invalid --mcp-server value '{raw}': name must not be empty"
        ));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "invalid --mcp-server value '{raw}': url must start with http:// or https:// (got '{url}')"
        ));
    }
    Ok((name.to_string(), url.to_string()))
}

/// Merge `config`-loaded MCP servers with `--mcp-server` flag injections.
/// Flag-injected servers are always `trusted: true` and override a
/// config-defined server of the same name (printing a stderr notice so the
/// override is never silent).
fn merge_mcp_servers(
    config_servers: Vec<crate::mcp::config::McpServerConfig>,
    flag_servers: Vec<(String, String)>,
) -> Vec<crate::mcp::config::McpServerConfig> {
    let mut merged: std::collections::BTreeMap<String, crate::mcp::config::McpServerConfig> =
        config_servers
            .into_iter()
            .map(|server| (server.name.clone(), server))
            .collect();
    for (name, url) in flag_servers {
        if merged.contains_key(&name) {
            eprintln!(
                "myagent: --mcp-server '{name}' overrides the config-defined MCP server with the same name"
            );
        }
        merged.insert(
            name.clone(),
            crate::mcp::config::McpServerConfig {
                name,
                command: String::new(),
                url: Some(url),
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                trusted: true,
            },
        );
    }
    merged.into_values().collect()
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum NativeSearch {
    On,
    Off,
}

impl NativeSearch {
    fn enabled(self) -> bool {
        matches!(self, NativeSearch::On)
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum PreflightGate {
    On,
    Off,
}

impl PreflightGate {
    fn enabled(self) -> bool {
        matches!(self, PreflightGate::On)
    }
}

#[derive(Debug, Parser)]
#[command(name = "myagent")]
#[command(about = "MyAgentHubs harness-agent CLI")]
pub struct Cli {
    #[command(flatten)]
    interactive: InteractiveArgs,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run(RunArgs),
    /// Two-layer plan mode.
    Plan(PlanArgs),
    Resume(ResumeArgs),
    Interrupt(InterruptArgs),
    Info(InfoArgs),
    /// Read a run journal: human summary, --jsonl replay.
    Inspect(InspectArgs),
    /// Start the persistent command-line agent session.
    Shell(InteractiveArgs),
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Inspect configured MCP servers (tools / resources / prompts).
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    /// List tools, resources, and prompts exposed by each configured MCP server.
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Args)]
struct InteractiveArgs {
    #[arg(
        long,
        default_value = "deepseek",
        help = "已知: deepseek|glm|kimi|qwen|grok|gemini|zai|anthropic|claude（后三走 Anthropic 协议·自动判）；也可传任意 OpenAI 兼容 provider 名；端点/密钥/协议用 {PREFIX}_BASE_URL / {PREFIX}_API_KEY / {PREFIX}_PROTOCOL 覆盖"
    )]
    provider: String,
    #[arg(long, value_enum, default_value_t = PermissionPolicy::Ask)]
    permission: PermissionPolicy,
    #[arg(long = "network", value_enum, default_value_t = crate::goal::NetworkPolicy::On)]
    network: crate::goal::NetworkPolicy,
    #[arg(long = "fs-read-scope", value_enum, default_value_t = crate::fs_scope::FsReadScope::Workspace)]
    fs_read_scope: crate::fs_scope::FsReadScope,
    #[arg(long = "fs-write-fence", value_enum, default_value_t = crate::exec::sandbox::FsWriteFence::Off)]
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    #[arg(long = "evidence-gate", value_enum, default_value_t = crate::orchestrator::EvidenceGate::Off)]
    evidence_gate: crate::orchestrator::EvidenceGate,
    #[arg(long = "native-search", value_enum, default_value_t = NativeSearch::On)]
    native_search: NativeSearch,
    #[arg(long = "no-memory", default_value_t = false)]
    no_memory: bool,
    #[arg(long, default_value_t = false)]
    learn: bool,
    #[arg(long = "auto-learn", default_value_t = false)]
    auto_learn: bool,
    #[arg(long, default_value_t = crate::orchestrator::MIN_TASK_TURN_BUDGET)]
    max_turns: usize,
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    jsonl: bool,
    #[arg(long)]
    journal_dir: Option<PathBuf>,
    #[arg(long = "context")]
    context_files: Vec<PathBuf>,
    #[arg(long = "criteria")]
    criteria: Vec<String>,
    #[arg(long = "contract-policy", value_enum, default_value_t = crate::guardrails::ContractPolicy::Ask)]
    contract_policy: crate::guardrails::ContractPolicy,
    #[arg(long = "max-eval-attempts", default_value_t = 3)]
    max_eval_attempts: usize,
    #[arg(
        long = "verify-every",
        default_value_t = crate::orchestrator::DEFAULT_VERIFY_EVERY,
        help = "Run approved check_cmd after this many successful fs_write/fs_edit calls when approved verifiable criteria exist (default on; 0 disables)."
    )]
    verify_every: usize,
    #[arg(
        long = "watchdog-repeat",
        default_value_t = crate::orchestrator::DEFAULT_WATCHDOG_REPEAT,
        help = "Stop early when the same reflex validation failure repeats this many consecutive times when approved verifiable criteria exist (default on; 0 disables)."
    )]
    watchdog_repeat: usize,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Prompt text or path to a mission file.
    input: String,
    #[arg(
        long,
        default_value = "deepseek",
        help = "已知: deepseek|glm|kimi|qwen|grok|gemini|zai|anthropic|claude（后三走 Anthropic 协议·自动判）；也可传任意 OpenAI 兼容 provider 名；端点/密钥/协议用 {PREFIX}_BASE_URL / {PREFIX}_API_KEY / {PREFIX}_PROTOCOL 覆盖"
    )]
    provider: String,
    #[arg(long)]
    jsonl: bool,
    #[arg(long, value_enum, default_value_t = PermissionPolicy::Ask)]
    permission: PermissionPolicy,
    #[arg(long = "network", value_enum, default_value_t = crate::goal::NetworkPolicy::On)]
    network: crate::goal::NetworkPolicy,
    #[arg(long = "fs-read-scope", value_enum, default_value_t = crate::fs_scope::FsReadScope::Workspace)]
    fs_read_scope: crate::fs_scope::FsReadScope,
    #[arg(long = "fs-write-fence", value_enum, default_value_t = crate::exec::sandbox::FsWriteFence::Off)]
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    #[arg(long = "evidence-gate", value_enum, default_value_t = crate::orchestrator::EvidenceGate::Off)]
    evidence_gate: crate::orchestrator::EvidenceGate,
    #[arg(long = "native-search", value_enum, default_value_t = NativeSearch::On)]
    native_search: NativeSearch,
    #[arg(long = "disallow-tools", value_delimiter = ',')]
    disallow_tools: Vec<String>,
    #[arg(long = "no-memory", default_value_t = false)]
    no_memory: bool,
    #[arg(long, default_value_t = false)]
    learn: bool,
    #[arg(long = "auto-learn", default_value_t = false)]
    auto_learn: bool,
    #[arg(long, default_value_t = crate::orchestrator::MIN_TASK_TURN_BUDGET)]
    max_turns: usize,
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    client_session_id: Option<String>,
    #[arg(long)]
    run_id: Option<String>,
    #[arg(long = "context")]
    context_files: Vec<PathBuf>,
    #[arg(long = "criteria")]
    criteria: Vec<String>,
    #[arg(long = "contract-policy", value_enum, default_value_t = crate::guardrails::ContractPolicy::Ask)]
    contract_policy: crate::guardrails::ContractPolicy,
    #[arg(long = "max-eval-attempts", default_value_t = 3)]
    max_eval_attempts: usize,
    #[arg(
        long = "verify-every",
        default_value_t = crate::orchestrator::DEFAULT_VERIFY_EVERY,
        help = "Run approved check_cmd after this many successful fs_write/fs_edit calls when approved verifiable criteria exist (default on; 0 disables)."
    )]
    verify_every: usize,
    #[arg(
        long = "watchdog-repeat",
        default_value_t = crate::orchestrator::DEFAULT_WATCHDOG_REPEAT,
        help = "Stop early when the same reflex validation failure repeats this many consecutive times when approved verifiable criteria exist (default on; 0 disables)."
    )]
    watchdog_repeat: usize,
    #[arg(long)]
    journal_dir: Option<PathBuf>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long = "fallback-model")]
    fallback_model: Option<String>,
    #[arg(
        long = "mcp-server",
        value_parser = parse_mcp_server_flag,
        help = "追加一个 Streamable HTTP MCP server：<name>=<url>（可重复；url 须 http/https；注入的 server 自动 trusted，免逐次 approval；同名覆盖 config 中的 server）"
    )]
    mcp_server: Vec<(String, String)>,
    #[arg(
        long = "append-system-prompt",
        help = "把这段文本追加到内置 system prompt 之后（不替换）"
    )]
    append_system_prompt: Option<String>,
}

#[derive(Debug, Args)]
struct PlanArgs {
    /// Goal text or path to a mission file.
    input: String,
    #[arg(
        long,
        default_value = "deepseek",
        help = "已知: deepseek|glm|kimi|qwen|grok|gemini|zai|anthropic|claude（后三走 Anthropic 协议·自动判）；也可传任意 OpenAI 兼容 provider 名；端点/密钥/协议用 {PREFIX}_BASE_URL / {PREFIX}_API_KEY / {PREFIX}_PROTOCOL 覆盖"
    )]
    provider: String,
    #[arg(long)]
    jsonl: bool,
    #[arg(long, value_enum, default_value_t = PermissionPolicy::Ask)]
    permission: PermissionPolicy,
    #[arg(long = "network", value_enum, default_value_t = crate::goal::NetworkPolicy::On)]
    network: crate::goal::NetworkPolicy,
    #[arg(long = "fs-read-scope", value_enum, default_value_t = crate::fs_scope::FsReadScope::Workspace)]
    fs_read_scope: crate::fs_scope::FsReadScope,
    #[arg(long = "fs-write-fence", value_enum, default_value_t = crate::exec::sandbox::FsWriteFence::Off)]
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    #[arg(long = "evidence-gate", value_enum, default_value_t = crate::orchestrator::EvidenceGate::Off)]
    evidence_gate: crate::orchestrator::EvidenceGate,
    #[arg(long = "criteria")]
    criteria: Vec<String>,
    #[arg(long, default_value_t = crate::orchestrator::MIN_TASK_TURN_BUDGET)]
    max_turns: usize,
    #[arg(long = "max-review-attempts", default_value_t = 3)]
    max_review_attempts: usize,
    #[arg(long = "max-plan-steps", default_value_t = 50)]
    max_plan_steps: usize,
    #[arg(long = "max-replan-rounds", default_value_t = 3)]
    max_replan_rounds: usize,
    #[arg(long = "contract-policy", value_enum, default_value_t = crate::guardrails::ContractPolicy::Ask)]
    contract_policy: crate::guardrails::ContractPolicy,
    #[arg(long = "max-eval-attempts", default_value_t = 3)]
    max_eval_attempts: usize,
    #[arg(long = "preflight-gate", value_enum, default_value_t = PreflightGate::On)]
    preflight_gate: PreflightGate,
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    journal_dir: Option<PathBuf>,
    #[arg(long)]
    run_id: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long = "fallback-model")]
    fallback_model: Option<String>,
    /// Resume a crashed plan run from its persisted state.
    #[arg(long)]
    resume: bool,
}

#[derive(Debug, Args)]
struct ResumeArgs {
    run_id: String,
    prompt: Option<String>,
    #[arg(
        long,
        help = "已知: deepseek|glm|kimi|qwen|grok|gemini|zai|anthropic|claude（后三走 Anthropic 协议·自动判）；也可传任意 OpenAI 兼容 provider 名；端点/密钥/协议用 {PREFIX}_BASE_URL / {PREFIX}_API_KEY / {PREFIX}_PROTOCOL 覆盖"
    )]
    provider: Option<String>,
    #[arg(long)]
    jsonl: bool,
    #[arg(long, value_enum, default_value_t = PermissionPolicy::Ask)]
    permission: PermissionPolicy,
    #[arg(long = "network", value_enum, default_value_t = crate::goal::NetworkPolicy::On)]
    network: crate::goal::NetworkPolicy,
    #[arg(long = "fs-read-scope", value_enum, default_value_t = crate::fs_scope::FsReadScope::Workspace)]
    fs_read_scope: crate::fs_scope::FsReadScope,
    #[arg(long = "fs-write-fence", value_enum, default_value_t = crate::exec::sandbox::FsWriteFence::Off)]
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    #[arg(long = "evidence-gate", value_enum, default_value_t = crate::orchestrator::EvidenceGate::Off)]
    evidence_gate: crate::orchestrator::EvidenceGate,
    #[arg(long = "native-search", value_enum, default_value_t = NativeSearch::On)]
    native_search: NativeSearch,
    #[arg(long = "disallow-tools", value_delimiter = ',')]
    disallow_tools: Vec<String>,
    #[arg(long = "no-memory", default_value_t = false)]
    no_memory: bool,
    #[arg(long, default_value_t = crate::orchestrator::MIN_TASK_TURN_BUDGET)]
    max_turns: usize,
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    journal_dir: Option<PathBuf>,
    #[arg(
        long = "verify-every",
        default_value_t = crate::orchestrator::DEFAULT_VERIFY_EVERY,
        help = "Run approved check_cmd after this many successful fs_write/fs_edit calls when approved verifiable criteria exist (default on; 0 disables)."
    )]
    verify_every: usize,
    #[arg(
        long = "watchdog-repeat",
        default_value_t = crate::orchestrator::DEFAULT_WATCHDOG_REPEAT,
        help = "Stop early when the same reflex validation failure repeats this many consecutive times when approved verifiable criteria exist (default on; 0 disables)."
    )]
    watchdog_repeat: usize,
    #[arg(long = "realign-objective")]
    realign_objective: Option<String>,
    #[arg(long = "realign-criteria")]
    realign_criteria: Vec<String>,
    #[arg(long = "realign-scope")]
    realign_scope: Option<String>,
    #[arg(long = "realign-constraint")]
    realign_constraint: Vec<String>,
    #[arg(long = "realign-reason")]
    realign_reason: Option<String>,
}

#[derive(Debug, Args)]
struct InterruptArgs {
    run_id: String,
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    journal_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct InfoArgs {
    #[arg(
        long,
        default_value = "deepseek",
        help = "已知: deepseek|glm|kimi|qwen|grok|gemini|zai|anthropic|claude（后三走 Anthropic 协议·自动判）；也可传任意 OpenAI 兼容 provider 名；端点/密钥/协议用 {PREFIX}_BASE_URL / {PREFIX}_API_KEY / {PREFIX}_PROTOCOL 覆盖"
    )]
    provider: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct InspectArgs {
    /// Run id to inspect. Mutually exclusive with --list.
    #[arg(required_unless_present = "list")]
    run_id: Option<String>,
    /// List all runs under the journal root.
    #[arg(long, conflicts_with = "run_id")]
    list: bool,
    /// Machine face: verbatim events.jsonl replay / one JSON object per run.
    #[arg(long)]
    jsonl: bool,
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    journal_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Provider(ConfigProviderArgs),
    Search(ConfigSearchArgs),
    /// Manage MCP (Model Context Protocol) servers.
    Mcp {
        #[command(subcommand)]
        command: ConfigMcpCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigMcpCommand {
    /// Register an MCP server.
    Add(ConfigMcpAddArgs),
    /// Remove a registered MCP server by name.
    Remove {
        /// Name of the MCP server to remove.
        name: String,
    },
    /// List all registered MCP servers.
    List,
}

#[derive(Debug, Args)]
struct ConfigMcpAddArgs {
    /// Unique name for this MCP server.
    name: String,
    /// Executable to launch over stdio (path or command name). Mutually
    /// exclusive with --url.
    #[arg(long, conflicts_with = "url")]
    command: Option<String>,
    /// Streamable HTTP endpoint of the server. Mutually exclusive with --command.
    #[arg(long, conflicts_with = "command")]
    url: Option<String>,
    /// Command-line arguments (comma-separated).  Use -- to pass leading-hyphen values.
    #[arg(long, value_delimiter = ',', allow_hyphen_values = true)]
    args: Vec<String>,
    /// Environment variables (comma-separated KEY=VALUE pairs).
    #[arg(long, value_delimiter = ',')]
    env: Vec<String>,
    /// Mark this server as trusted (allows tools without per-invocation approval).
    #[arg(long, default_value_t = false)]
    trusted: bool,
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    Learn {
        run_id: String,
        #[arg(long)]
        journal_dir: Option<PathBuf>,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Review {
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Accept {
        id: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Reject {
        id: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Edit {
        id: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Remember {
        text: String,
        #[arg(long = "tags", value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Suspect {
        id: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Archive {
        id: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct ConfigProviderArgs {
    id: String,
    #[arg(long)]
    api_key: String,
    #[arg(long)]
    base_url: Option<String>,
    #[arg(long)]
    model: Option<String>,
}

#[derive(Debug, Args)]
struct ConfigSearchArgs {
    #[arg(long)]
    backend: String,
    #[arg(long)]
    api_key: String,
}

pub async fn run_from_env() -> Result<RunOutcome> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Run(args)) => run_command(args).await,
        Some(Command::Plan(args)) => plan_command(args).await,
        Some(Command::Resume(args)) => resume_command(args).await,
        Some(Command::Interrupt(args)) => {
            let workspace = args.workspace.unwrap_or(std::env::current_dir()?);
            let journal_root = resolve_journal_root(args.journal_dir, &workspace);
            let path = request_interrupt(journal_root, &args.run_id)?;
            println!("{}", path.to_string_lossy());
            Ok(RunOutcome::Completed)
        }
        Some(Command::Info(args)) => {
            info_command(args)?;
            Ok(RunOutcome::Completed)
        }
        Some(Command::Inspect(args)) => {
            inspect_command(args)?;
            Ok(RunOutcome::Completed)
        }
        Some(Command::Shell(args)) => {
            interactive_command(args).await?;
            Ok(RunOutcome::Completed)
        }
        Some(Command::Config { command }) => {
            config_command(command)?;
            Ok(RunOutcome::Completed)
        }
        Some(Command::Memory { command }) => {
            memory_command(command).await?;
            Ok(RunOutcome::Completed)
        }
        Some(Command::Mcp { command }) => {
            mcp_command(command).await?;
            Ok(RunOutcome::Completed)
        }
        None => {
            interactive_command(cli.interactive).await?;
            Ok(RunOutcome::Completed)
        }
    }
}

async fn run_command(args: RunArgs) -> Result<RunOutcome> {
    crate::exec::sandbox::validate_write_fence(args.fs_write_fence)?;
    let workspace = args.workspace.unwrap_or(std::env::current_dir()?);
    let journal_root = resolve_journal_root(args.journal_dir, &workspace);
    let ws_for_learn = workspace.clone();
    let jr_for_learn = journal_root.clone();
    let provider_for_learn = args.provider.clone();
    let learn = args.learn;
    let auto_learn = args.auto_learn;
    let prompt = read_input(&args.input)?;
    let criteria = crate::goal::parse_criteria(&args.criteria)?;
    let disallowed_tools = args.disallow_tools.into_iter().collect();
    let output_mode = if args.jsonl {
        OutputMode::Jsonl
    } else {
        OutputMode::Human
    };
    let control_input = if args.jsonl {
        ControlInputKind::StdinJsonl
    } else {
        ControlInputKind::Sentinel
    };

    let result = run_with_provider(
        &args.provider,
        args.model.clone(),
        args.fallback_model.clone(),
        RunOptions {
            prompt,
            workspace,
            provider_id: String::new(),
            model: String::new(),
            client_session_id: args.client_session_id,
            output_mode,
            control_input,
            permission: args.permission,
            network: args.network,
            fs_read_scope: args.fs_read_scope,
            fs_write_fence: args.fs_write_fence,
            evidence_gate: args.evidence_gate,
            native_search_enabled: args.native_search.enabled(),
            disallowed_tools,
            memory_enabled: !args.no_memory,
            search: config::search_choice(),
            max_turns: args.max_turns,
            run_id: args.run_id,
            context_files: args.context_files,
            criteria,
            contract_policy: args.contract_policy,
            max_eval_attempts: args.max_eval_attempts,
            verify_reflex_debt: args.verify_every,
            watchdog_repeat_threshold: args.watchdog_repeat,
            journal_root,
            mcp_servers: merge_mcp_servers(
                config::load_config()
                    .map(|c| c.mcp_servers())
                    .unwrap_or_default(),
                args.mcp_server,
            ),
            append_system_prompt: args.append_system_prompt,
        },
    )
    .await?;
    if matches!(result.outcome, RunOutcome::Completed) && (learn || auto_learn) {
        post_run_learn(
            &provider_for_learn,
            &ws_for_learn,
            &jr_for_learn,
            &result.run_id,
            auto_learn,
        )
        .await?;
    }
    Ok(result.outcome)
}

async fn resume_command(args: ResumeArgs) -> Result<RunOutcome> {
    crate::exec::sandbox::validate_write_fence(args.fs_write_fence)?;
    let realign = resume_realign_input(&args)?;
    let workspace = args.workspace.unwrap_or(std::env::current_dir()?);
    let journal_root = resolve_journal_root(args.journal_dir, &workspace);
    let disallowed_tools = args.disallow_tools.into_iter().collect();
    let provider = match args.provider {
        Some(provider) => provider,
        None => read_saved_provider(&journal_root, &args.run_id)?,
    };
    let output_mode = if args.jsonl {
        OutputMode::Jsonl
    } else {
        OutputMode::Human
    };
    let control_input = if args.jsonl {
        ControlInputKind::StdinJsonl
    } else {
        ControlInputKind::Sentinel
    };

    let result = resume_with_provider(
        &provider,
        workspace,
        journal_root,
        args.run_id,
        args.prompt,
        output_mode,
        args.permission,
        args.network,
        args.fs_read_scope,
        args.fs_write_fence,
        args.max_turns,
        control_input,
        args.native_search.enabled(),
        !args.no_memory,
        config::search_choice(),
        disallowed_tools,
        args.verify_every,
        args.watchdog_repeat,
        realign,
    )
    .await?;
    Ok(result.outcome)
}

fn resume_realign_input(args: &ResumeArgs) -> Result<Option<crate::goal::ReAlignInput>> {
    let has_realign = args.realign_objective.is_some()
        || !args.realign_criteria.is_empty()
        || args.realign_scope.is_some()
        || !args.realign_constraint.is_empty();
    if !has_realign {
        return Ok(None);
    }

    Ok(Some(crate::goal::ReAlignInput {
        objective: args.realign_objective.clone(),
        add_criteria: crate::goal::parse_criteria(&args.realign_criteria)?,
        scope: args.realign_scope.clone(),
        add_constraints: args.realign_constraint.clone(),
        reason: args
            .realign_reason
            .clone()
            .unwrap_or_else(|| "user re-align".to_string()),
    }))
}

async fn run_with_provider(
    provider_name: &str,
    model_override: Option<String>,
    fallback_model: Option<String>,
    mut options: RunOptions,
) -> Result<crate::orchestrator::RunResult> {
    let provider_name = provider_name.to_ascii_lowercase();
    match provider_name.as_str() {
        "mock" => {
            options.provider_id = "mock".to_string();
            options.model = "mock-model".to_string();
            run_solo_with_judge(MockProvider::default(), Box::new(NoopJudge), options).await
        }
        provider => {
            let mut config = config::provider_config_with_model(provider, model_override)?;
            config.network = options.network;
            config.native_search_enabled = options.native_search_enabled;
            config.fallback_model = fallback_model;
            options.model = config.model.clone();
            options.provider_id = config.provider_id.clone();
            let proto = protocol_override_for(provider);
            let judge_cfg = OpenAiCompatibleConfig {
                temperature: Some(0.0),
                sampling: Default::default(),
                native_search_enabled: false,
                context_tokens: None,
                output_tokens: None,
                ..config.clone()
            };
            with_detected_provider!(config.clone(), proto.as_deref(), |provider_client| {
                with_detected_provider!(judge_cfg, proto.as_deref(), |judge_client| {
                    let judge = LlmJudge::new(judge_client);
                    run_solo_with_judge(provider_client, Box::new(judge), options).await
                })
            })
        }
    }
}

async fn plan_command(args: PlanArgs) -> Result<RunOutcome> {
    crate::exec::sandbox::validate_write_fence(args.fs_write_fence)?;
    let workspace = args.workspace.unwrap_or(std::env::current_dir()?);
    let journal_root = resolve_journal_root(args.journal_dir, &workspace);
    let objective = read_input(&args.input)?;
    let checks = crate::goal::parse_criteria(&args.criteria)?;
    let output_mode = if args.jsonl {
        OutputMode::Jsonl
    } else {
        OutputMode::Human
    };
    let plan_run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| format!("plan_{}", uuid::Uuid::new_v4()));
    let opts = PlanRunOptions {
        objective,
        checks,
        workspace,
        journal_root,
        plan_run_id,
        permission: args.permission,
        network: args.network,
        fs_read_scope: args.fs_read_scope,
        fs_write_fence: args.fs_write_fence,
        output_mode,
        max_review_attempts: args.max_review_attempts,
        max_plan_steps: args.max_plan_steps,
        max_replan_rounds: args.max_replan_rounds,
        contract_policy: args.contract_policy,
        max_eval_attempts: args.max_eval_attempts,
        default_task_max_turns: args.max_turns,
        preflight_gate: args.preflight_gate.enabled(),
    };
    let result = plan_with_provider(
        &args.provider,
        args.model.clone(),
        args.fallback_model.clone(),
        opts,
        args.resume,
    )
    .await?;
    Ok(result.outcome)
}

async fn plan_with_provider(
    provider_name: &str,
    model_override: Option<String>,
    fallback_model: Option<String>,
    opts: PlanRunOptions,
    resume: bool,
) -> Result<crate::orchestrator::RunResult> {
    match provider_name.to_ascii_lowercase().as_str() {
        "mock" => {
            if resume {
                resume_plan(MockProvider::default(), opts).await
            } else {
                run_plan(MockProvider::default(), opts).await
            }
        }
        provider => {
            let mut config = config::provider_config_with_model(provider, model_override)?;
            config.network = opts.network;
            config.fallback_model = fallback_model;
            let proto = protocol_override_for(provider);
            with_detected_provider!(config, proto.as_deref(), |client| {
                if resume {
                    resume_plan(client, opts).await
                } else {
                    run_plan(client, opts).await
                }
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn resume_with_provider(
    provider_name: &str,
    workspace: PathBuf,
    journal_root: PathBuf,
    run_id: String,
    prompt: Option<String>,
    output_mode: OutputMode,
    permission: PermissionPolicy,
    network: crate::goal::NetworkPolicy,
    fs_read_scope: crate::fs_scope::FsReadScope,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    max_turns: usize,
    control_input: ControlInputKind,
    native_search_enabled: bool,
    memory_enabled: bool,
    search: crate::config::SearchChoice,
    disallowed_tools: BTreeSet<String>,
    verify_reflex_debt: usize,
    watchdog_repeat_threshold: usize,
    realign: Option<crate::goal::ReAlignInput>,
) -> Result<crate::orchestrator::RunResult> {
    match provider_name {
        "mock" => {
            resume_solo_with_judge_and_fs_scope(
                MockProvider::default(),
                Box::new(NoopJudge),
                workspace,
                journal_root.clone(),
                run_id,
                prompt,
                output_mode,
                permission,
                network,
                fs_read_scope,
                fs_write_fence,
                max_turns,
                control_input,
                native_search_enabled,
                memory_enabled,
                search,
                disallowed_tools,
                verify_reflex_debt,
                watchdog_repeat_threshold,
                realign,
            )
            .await
        }
        provider => {
            let mut config = config::provider_config(provider)?;
            config.network = network;
            config.native_search_enabled = native_search_enabled;
            let proto = protocol_override_for(provider);
            let judge_cfg = OpenAiCompatibleConfig {
                temperature: Some(0.0),
                sampling: Default::default(),
                native_search_enabled: false,
                context_tokens: None,
                output_tokens: None,
                ..config.clone()
            };
            with_detected_provider!(config.clone(), proto.as_deref(), |provider_client| {
                with_detected_provider!(judge_cfg, proto.as_deref(), |judge_client| {
                    let judge = LlmJudge::new(judge_client);
                    resume_solo_with_judge_and_fs_scope(
                        provider_client,
                        Box::new(judge),
                        workspace,
                        journal_root.clone(),
                        run_id,
                        prompt,
                        output_mode,
                        permission,
                        network,
                        fs_read_scope,
                        fs_write_fence,
                        max_turns,
                        control_input,
                        native_search_enabled,
                        memory_enabled,
                        search,
                        disallowed_tools,
                        verify_reflex_debt,
                        watchdog_repeat_threshold,
                        realign,
                    )
                    .await
                })
            })
        }
    }
}

async fn interactive_command(args: InteractiveArgs) -> Result<()> {
    crate::exec::sandbox::validate_write_fence(args.fs_write_fence)?;
    let workspace = args.workspace.unwrap_or(std::env::current_dir()?);
    let jsonl = args.jsonl;
    let journal_root = resolve_journal_root(args.journal_dir.clone(), &workspace);
    let learn = args.learn;
    let auto_learn = args.auto_learn;
    let output_mode = if jsonl {
        OutputMode::Jsonl
    } else {
        OutputMode::Human
    };
    let mut provider = args.provider.to_ascii_lowercase();
    let mut permission = args.permission;
    let mut active_run_id: Option<String> = None;
    let criteria = crate::goal::parse_criteria(&args.criteria)?;
    let mut context_files = args.context_files;

    if !jsonl {
        println!("myagent interactive");
        println!(
            "provider: {provider} · workspace: {}",
            workspace.to_string_lossy()
        );
        println!("type /help for commands, /exit to quit");
    }

    loop {
        if !jsonl {
            print_interactive_prompt(active_run_id.as_deref())?;
        }
        let mut input = String::new();
        let bytes = io::stdin().read_line(&mut input)?;
        if bytes == 0 {
            if !jsonl {
                println!();
            }
            return Ok(());
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        if input.starts_with('/') {
            if handle_interactive_slash(
                input,
                &mut provider,
                &mut permission,
                &workspace,
                &mut active_run_id,
                &mut context_files,
                jsonl,
            )? {
                return Ok(());
            }
            continue;
        }

        let (result, learn_provider) = if let Some(run_id) = active_run_id.clone() {
            let saved_provider = read_saved_provider(&journal_root, &run_id)?;
            let result = resume_with_provider(
                &saved_provider,
                workspace.clone(),
                journal_root.clone(),
                run_id.clone(),
                Some(input.to_string()),
                output_mode,
                permission,
                args.network,
                args.fs_read_scope,
                args.fs_write_fence,
                args.max_turns,
                ControlInputKind::Sentinel,
                args.native_search.enabled(),
                !args.no_memory,
                config::search_choice(),
                Default::default(),
                args.verify_every,
                args.watchdog_repeat,
                None,
            )
            .await;
            (result, saved_provider)
        } else {
            let learn_provider = provider.clone();
            let result = run_with_provider(
                &provider,
                None,
                None,
                RunOptions {
                    prompt: input.to_string(),
                    workspace: workspace.clone(),
                    provider_id: String::new(),
                    model: String::new(),
                    client_session_id: None,
                    output_mode,
                    control_input: ControlInputKind::Sentinel,
                    permission,
                    network: args.network,
                    fs_read_scope: args.fs_read_scope,
                    fs_write_fence: args.fs_write_fence,
                    evidence_gate: args.evidence_gate,
                    native_search_enabled: args.native_search.enabled(),
                    disallowed_tools: Default::default(),
                    memory_enabled: !args.no_memory,
                    search: config::search_choice(),
                    max_turns: args.max_turns,
                    run_id: None,
                    context_files: context_files.clone(),
                    criteria: criteria.clone(),
                    contract_policy: args.contract_policy,
                    max_eval_attempts: args.max_eval_attempts,
                    verify_reflex_debt: args.verify_every,
                    watchdog_repeat_threshold: args.watchdog_repeat,
                    journal_root: journal_root.clone(),
                    mcp_servers: config::load_config()
                        .map(|c| c.mcp_servers())
                        .unwrap_or_default(),
                    append_system_prompt: None,
                },
            )
            .await;
            (result, learn_provider)
        };

        match result {
            Ok(result) => {
                if matches!(result.outcome, RunOutcome::Completed) && (learn || auto_learn) {
                    post_run_learn(
                        &learn_provider,
                        &workspace,
                        &journal_root,
                        &result.run_id,
                        auto_learn,
                    )
                    .await?;
                }
                let always_used = result.always_used;
                active_run_id = Some(result.run_id);
                permission = elevate_permission(permission, always_used);
            }
            Err(err) => eprintln!("error: {err}"),
        }
    }
}

fn elevate_permission(current: PermissionPolicy, always_used: bool) -> PermissionPolicy {
    if always_used {
        PermissionPolicy::Allow
    } else {
        current
    }
}

fn print_interactive_prompt(active_run_id: Option<&str>) -> Result<()> {
    match active_run_id {
        Some(run_id) => print!("myagent:{run_id}> "),
        None => print!("myagent> "),
    }
    io::stdout().flush()?;
    Ok(())
}

fn handle_interactive_slash(
    input: &str,
    provider: &mut String,
    permission: &mut PermissionPolicy,
    workspace: &Path,
    active_run_id: &mut Option<String>,
    context_files: &mut Vec<PathBuf>,
    jsonl: bool,
) -> Result<bool> {
    let mut parts = input.splitn(2, char::is_whitespace);
    let command = parts.next().unwrap_or_default();
    let value = parts.next().unwrap_or_default().trim();

    match command {
        "/exit" | "/quit" => Ok(true),
        "/help" => {
            if !jsonl {
                print_interactive_help();
            }
            Ok(false)
        }
        "/status" => {
            if !jsonl {
                println!("provider: {provider}");
                println!("permission: {permission:?}");
                println!("workspace: {}", workspace.to_string_lossy());
                println!(
                    "run: {}",
                    active_run_id
                        .as_deref()
                        .unwrap_or("none; next prompt starts a run")
                );
                if !context_files.is_empty() {
                    println!(
                        "context: {}",
                        context_files
                            .iter()
                            .map(|path| path.to_string_lossy())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
            Ok(false)
        }
        "/new" => {
            *active_run_id = None;
            if !jsonl {
                println!("new run will start on next prompt");
            }
            Ok(false)
        }
        "/resume" => {
            if value.is_empty() {
                if !jsonl {
                    println!("usage: /resume <run_id>");
                }
            } else {
                *active_run_id = Some(value.to_string());
                if !jsonl {
                    println!("active run: {value}");
                }
            }
            Ok(false)
        }
        "/provider" => {
            if value.is_empty() {
                if !jsonl {
                    println!("provider: {provider}");
                }
            } else {
                *provider = value.to_ascii_lowercase();
                if !jsonl {
                    println!("provider for new runs: {provider}");
                    if active_run_id.is_some() {
                        println!(
                            "current run keeps its saved provider; use /new to start with {provider}"
                        );
                    }
                }
            }
            Ok(false)
        }
        "/permission" => {
            if value.is_empty() {
                if !jsonl {
                    println!("permission: {permission:?}");
                }
            } else if let Some(parsed) = parse_permission(value) {
                *permission = parsed;
                if !jsonl {
                    println!("permission: {permission:?}");
                }
            } else {
                if !jsonl {
                    println!("usage: /permission ask|allow|deny");
                }
            }
            Ok(false)
        }
        "/context" => {
            if value.is_empty() {
                if !jsonl {
                    println!("usage: /context <file>");
                }
            } else {
                context_files.push(PathBuf::from(value));
                if !jsonl {
                    println!("context added for next new run: {value}");
                }
            }
            Ok(false)
        }
        _ => {
            if !jsonl {
                println!("unknown command: {command}");
                println!("type /help for commands");
            }
            Ok(false)
        }
    }
}

fn parse_permission(value: &str) -> Option<PermissionPolicy> {
    match value.to_ascii_lowercase().as_str() {
        "ask" => Some(PermissionPolicy::Ask),
        "allow" => Some(PermissionPolicy::Allow),
        "deny" => Some(PermissionPolicy::Deny),
        _ => None,
    }
}

fn print_interactive_help() {
    println!("Commands:");
    println!("  /help                 show this help");
    println!("  /status               show provider, workspace, and active run");
    println!("  /new                  start a fresh run on the next prompt");
    println!("  /resume <run_id>      continue a saved run");
    println!("  /provider <id>        set provider for new runs");
    println!("  /permission <policy>  set shell policy: ask, allow, deny");
    println!("  /context <file>       attach file context to the next new run");
    println!("  /exit                 quit");
}

fn info_command(args: InfoArgs) -> Result<()> {
    let capabilities = match args.provider.as_str() {
        "mock" => MockProvider::default().capabilities(),
        provider => {
            let config = OpenAiCompatibleConfig {
                provider_id: provider.to_string(),
                api_key: String::new(),
                base_url: config::default_base_url(provider),
                model: config::default_model(provider),
                timeout_secs: 120,
                temperature: None,
                sampling: Default::default(),
                network: crate::goal::NetworkPolicy::On,
                native_search_enabled: true,
                fallback_model: None,
                context_tokens: None,
                output_tokens: None,
            };
            let proto = protocol_override_for(provider);
            match crate::config::detect_provider_protocol(
                &config.provider_id,
                &config.base_url,
                proto.as_deref(),
            )? {
                crate::config::Protocol::Anthropic => {
                    crate::provider::anthropic_compatible::AnthropicProvider::new(config)?
                        .capabilities()
                }
                crate::config::Protocol::OpenAi => {
                    OpenAiCompatibleProvider::new(config)?.capabilities()
                }
            }
        }
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&capabilities)?);
    } else {
        println!(
            "{} · {} · streaming={} · tool_calling={} · shell={}",
            capabilities.provider_id,
            capabilities.model_id,
            capabilities.supports_streaming,
            capabilities.supports_tool_calling,
            capabilities.supports_shell_tool
        );
    }
    Ok(())
}

fn inspect_command(args: InspectArgs) -> Result<()> {
    let workspace = args.workspace.unwrap_or(std::env::current_dir()?);
    let journal_root = resolve_journal_root(args.journal_dir, &workspace);
    let mut stdout = io::stdout().lock();
    if args.list {
        for entry in crate::inspect::list_runs(&journal_root)? {
            if args.jsonl {
                writeln!(stdout, "{}", serde_json::to_string(&entry)?)?;
            } else {
                writeln!(
                    stdout,
                    "{} · {} · {}",
                    entry.run_id,
                    entry.terminal.as_deref().unwrap_or("-"),
                    entry.ts.as_deref().unwrap_or("-")
                )?;
            }
        }
        return Ok(());
    }
    let run_id = args
        .run_id
        .expect("clap required_unless_present guarantees run_id when --list absent");
    let paths = crate::journal::RunPaths::new(&journal_root, &run_id);
    if !paths.events_path.is_file() {
        return Err(HarnessError::Runtime(format!(
            "run `{run_id}` not found under {}",
            paths.root.to_string_lossy()
        )));
    }
    if args.jsonl {
        crate::inspect::replay(&paths.events_path, &mut stdout)?;
    } else {
        let summary = crate::inspect::summarize(&paths.events_path, &run_id)?;
        crate::inspect::render_summary(&summary, &mut stdout)?;
    }
    Ok(())
}

fn config_command(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Provider(args) => {
            let provider = args.id.to_ascii_lowercase();
            let path = config::save_provider(StoredProvider {
                id: provider.clone(),
                api_key: args.api_key,
                base_url: args
                    .base_url
                    .unwrap_or_else(|| config::default_base_url(&provider)),
                model: args
                    .model
                    .unwrap_or_else(|| config::default_model(&provider)),
                context_tokens: None,
                output_tokens: None,
            })?;
            println!("saved provider {provider} to {}", path.to_string_lossy());
            Ok(())
        }
        ConfigCommand::Search(args) => {
            let cfg = match args.backend.as_str() {
                "brave" => Some(config::SearchConfig::Brave {
                    api_key: args.api_key,
                }),
                "exa" => Some(config::SearchConfig::Exa {
                    api_key: args.api_key,
                }),
                _ => {
                    eprintln!("unsupported search backend: {}", args.backend);
                    None
                }
            };
            if let Some(cfg) = cfg {
                let path = config::save_search_config(cfg)?;
                println!("{}", path.to_string_lossy());
            }
            Ok(())
        }
        ConfigCommand::Mcp { command } => config_mcp_command(command),
    }
}

fn config_mcp_command(command: ConfigMcpCommand) -> Result<()> {
    match command {
        ConfigMcpCommand::Add(args) => {
            let env: std::collections::BTreeMap<String, String> = args
                .env
                .iter()
                .filter_map(|pair| {
                    let mut parts = pair.splitn(2, '=');
                    match (parts.next(), parts.next()) {
                        (Some(k), Some(v)) if !k.is_empty() => Some((k.to_string(), v.to_string())),
                        _ => {
                            eprintln!("warning: ignoring malformed env pair: {pair:?}");
                            None
                        }
                    }
                })
                .collect();

            if args.command.is_none() && args.url.is_none() {
                return Err(HarnessError::Runtime(
                    "one of --command or --url is required".into(),
                ));
            }

            let server = crate::mcp::config::McpServerConfig {
                name: args.name.clone(),
                command: args.command.unwrap_or_default(),
                url: args.url,
                args: args.args,
                env,
                trusted: args.trusted,
            };

            let path = config::save_mcp_server(&args.name, server)?;
            println!(
                "saved mcp server {} to {}",
                args.name,
                path.to_string_lossy()
            );
            Ok(())
        }
        ConfigMcpCommand::Remove { name } => {
            let mut cfg = config::load_config()?;
            if cfg.mcp_servers.remove(&name).is_none() {
                return Err(HarnessError::Runtime(format!(
                    "mcp server `{name}` not found"
                )));
            }
            let path = config::config_path();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, serde_json::to_vec_pretty(&cfg)?)?;
            println!("removed mcp server {name}");
            Ok(())
        }
        ConfigMcpCommand::List => {
            let cfg = config::load_config()?;
            let mut names: Vec<_> = cfg.mcp_servers.keys().collect();
            names.sort();
            if names.is_empty() {
                println!("no mcp servers configured");
            } else {
                for name in names {
                    let server = &cfg.mcp_servers[name];
                    let env_str = if server.env.is_empty() {
                        String::new()
                    } else {
                        let pairs: Vec<_> =
                            server.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
                        format!(" env=[{}]", pairs.join(","))
                    };
                    let args_str = if server.args.is_empty() {
                        String::new()
                    } else {
                        format!(" args=[{}]", server.args.join(","))
                    };
                    let trusted_str = if server.trusted { " --trusted" } else { "" };
                    let target = match &server.url {
                        Some(url) => format!("url={url}"),
                        None => format!("command={}", server.command),
                    };
                    println!("mcp {name}: {target}{args_str}{env_str}{trusted_str}");
                }
            }
            Ok(())
        }
    }
}

async fn mcp_command(command: McpCommand) -> Result<()> {
    match command {
        McpCommand::List { json } => mcp_list(json).await,
    }
}

/// 对每个配好的 MCP server 短连一次·列 tools/resources/prompts 三类·连不上的标错·短连后关停。
async fn mcp_list(as_json: bool) -> Result<()> {
    let servers = config::load_config()?.mcp_servers();
    if servers.is_empty() {
        if as_json {
            println!("[]");
        } else {
            println!("no mcp servers configured");
        }
        return Ok(());
    }

    let timeout = std::time::Duration::from_secs(10);
    let mut entries = Vec::new();
    for cfg in &servers {
        match crate::mcp::client::McpConnection::connect(cfg, timeout).await {
            Ok((conn, caps)) => {
                let tools = if caps.tools {
                    mcp_list_labels(&conn, "tools/list", "tools").await
                } else {
                    Vec::new()
                };
                let resources = if caps.resources {
                    mcp_list_labels(&conn, "resources/list", "resources").await
                } else {
                    Vec::new()
                };
                let prompts = if caps.prompts {
                    mcp_list_labels(&conn, "prompts/list", "prompts").await
                } else {
                    Vec::new()
                };
                conn.shutdown().await;
                entries.push(serde_json::json!({
                    "server": cfg.name,
                    "ok": true,
                    "tools": tools,
                    "resources": resources,
                    "prompts": prompts,
                }));
            }
            Err(err) => {
                entries.push(serde_json::json!({
                    "server": cfg.name,
                    "ok": false,
                    "error": err.to_string(),
                }));
            }
        }
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        for entry in &entries {
            let server = entry["server"].as_str().unwrap_or("");
            if entry["ok"].as_bool() == Some(true) {
                println!("mcp {server}:");
                print_mcp_list("tools", &entry["tools"]);
                print_mcp_list("resources", &entry["resources"]);
                print_mcp_list("prompts", &entry["prompts"]);
            } else {
                println!(
                    "mcp {server}: error — {}",
                    entry["error"].as_str().unwrap_or("")
                );
            }
        }
    }
    Ok(())
}

/// 拉某类清单的标签（tools/prompts 用 name·resources 用 uri）·nextCursor 续拉·失败返回已拉到的。
async fn mcp_list_labels(
    conn: &crate::mcp::client::McpConnection,
    method: &str,
    key: &str,
) -> Vec<String> {
    let mut labels = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let params = match &cursor {
            Some(cursor) => serde_json::json!({ "cursor": cursor }),
            None => serde_json::json!({}),
        };
        let result = match conn.request(method, params).await {
            Ok(result) => result,
            Err(_) => break,
        };
        if let Some(items) = result.get(key).and_then(serde_json::Value::as_array) {
            for item in items {
                let label = item
                    .get("name")
                    .or_else(|| item.get("uri"))
                    .and_then(serde_json::Value::as_str);
                if let Some(label) = label {
                    labels.push(label.to_string());
                }
            }
        }
        match result.get("nextCursor").and_then(serde_json::Value::as_str) {
            Some(next) => {
                cursor = Some(next.to_string());
                pages += 1;
                if pages > 100 {
                    break;
                }
            }
            None => break,
        }
    }
    labels
}

fn print_mcp_list(label: &str, value: &serde_json::Value) {
    let names: Vec<&str> = value
        .as_array()
        .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    if names.is_empty() {
        println!("  {label}: (none)");
    } else {
        println!("  {label}: {}", names.join(", "));
    }
}

async fn memory_command(command: MemoryCommand) -> Result<()> {
    match command {
        MemoryCommand::Learn {
            run_id,
            journal_dir,
            workspace,
        } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            learn_from_run(&workspace, journal_dir, &run_id).await
        }
        MemoryCommand::Review { workspace } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            review_candidates(&workspace)
        }
        MemoryCommand::Accept { id, workspace } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            accept_candidate(&workspace, &id)
        }
        MemoryCommand::Reject { id, workspace } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            reject_candidate(&workspace, &id)
        }
        MemoryCommand::Edit { id, workspace } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            edit_candidate(&workspace, &id)
        }
        MemoryCommand::Remember {
            text,
            tags,
            workspace,
        } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            run_memory_remember(&workspace, &text, &tags)
        }
        MemoryCommand::Suspect { id, workspace } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            let store = MemoryStore::for_workspace(&workspace)?;
            store.set_status(&id, LessonStatus::Suspect)?;
            store.append_log(&format!("suspected {id}"))?;
            Ok(())
        }
        MemoryCommand::Archive { id, workspace } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            let store = MemoryStore::for_workspace(&workspace)?;
            store.set_status(&id, LessonStatus::Archived)?;
            store.append_log(&format!("archived {id}"))?;
            Ok(())
        }
    }
}

async fn learn_from_run(
    workspace: &Path,
    journal_dir: Option<PathBuf>,
    run_id: &str,
) -> Result<()> {
    let journal_root = resolve_journal_root(journal_dir, workspace);
    let events_path = journal_root
        .join(".myagenthubs")
        .join("runs")
        .join(run_id)
        .join("events.jsonl");
    let events = std::fs::read_to_string(&events_path)?;
    let store = MemoryStore::for_workspace(workspace)?;
    let mut config = config::provider_config("deepseek")?;
    config.network = crate::goal::NetworkPolicy::On;
    config.native_search_enabled = false;
    let provider = OpenAiCompatibleProvider::new(config)?;
    let summary = run_learn_pipeline(&provider, &provider, &events, run_id, &store, false).await?;
    println!(
        "learn: candidates={} promoted={} failed={}",
        summary.candidates, summary.promoted, summary.failed
    );
    Ok(())
}

async fn post_run_learn(
    provider_name: &str,
    workspace: &Path,
    journal_root: &Path,
    run_id: &str,
    auto_learn: bool,
) -> Result<()> {
    let events_path = journal_root
        .join(".myagenthubs")
        .join("runs")
        .join(run_id)
        .join("events.jsonl");
    let events = match std::fs::read_to_string(&events_path) {
        Ok(events) => events,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let store = MemoryStore::for_workspace(workspace)?;
    let mut config = config::provider_config(provider_name)?;
    config.network = crate::goal::NetworkPolicy::On;
    config.native_search_enabled = false;
    let proto = protocol_override_for(provider_name);
    let summary = with_detected_provider!(config, proto.as_deref(), |provider| {
        run_learn_pipeline(&provider, &provider, &events, run_id, &store, auto_learn).await
    })?;
    eprintln!(
        "learn: candidates={} promoted={} failed={}",
        summary.candidates, summary.promoted, summary.failed
    );
    Ok(())
}

fn review_candidates(workspace: &Path) -> Result<()> {
    let store = MemoryStore::for_workspace(workspace)?;
    for lesson in store.list_candidates()? {
        let summary = lesson
            .body
            .lines()
            .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .unwrap_or("")
            .trim();
        println!(
            "{} · tags=[{}] · {} · evidence_runs=[{}] · episode_ref={}",
            lesson.id,
            lesson.tags.join(","),
            summary,
            lesson.evidence_runs.join(","),
            lesson.episode_ref.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

pub fn accept_candidate(workspace: &Path, id: &str) -> Result<()> {
    let store = MemoryStore::for_workspace(workspace)?;
    let mut lesson = store.read_lesson(id)?;
    if lesson.status != LessonStatus::Candidate {
        return Err(HarnessError::Runtime(format!(
            "memory: {id} not a candidate"
        )));
    }
    lesson.status = LessonStatus::Active;
    store.write_lesson(&lesson)?;
    store.append_log(&format!("accepted {id}"))?;
    Ok(())
}

pub fn reject_candidate(workspace: &Path, id: &str) -> Result<()> {
    let store = MemoryStore::for_workspace(workspace)?;
    store.set_status(id, LessonStatus::Archived)?;
    store.append_log(&format!("rejected {id}"))?;
    Ok(())
}

fn edit_candidate(workspace: &Path, id: &str) -> Result<()> {
    let store = MemoryStore::for_workspace(workspace)?;
    let lesson = store.read_lesson(id)?;
    if lesson.status != LessonStatus::Candidate {
        return Err(HarnessError::Runtime(format!(
            "memory: {id} not a candidate"
        )));
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!(
        "myagent-memory-edit-{id}-{}-{nonce}.md",
        std::process::id()
    ));
    std::fs::write(&temp_path, lesson.to_markdown())?;

    let editor = std::env::var_os("EDITOR")
        .ok_or_else(|| HarnessError::Runtime("memory: EDITOR not set".into()))?;
    let status = std::process::Command::new(editor)
        .arg(&temp_path)
        .status()?;
    if !status.success() {
        return Err(HarnessError::Runtime(format!(
            "memory: editor exited with {status}"
        )));
    }

    let edited = std::fs::read_to_string(&temp_path)?;
    let parsed = Lesson::parse(&edited)?;
    if parsed.id != id {
        return Err(HarnessError::Runtime(format!(
            "memory: edited lesson id changed from {id} to {}",
            parsed.id
        )));
    }
    if parsed.status != LessonStatus::Candidate {
        return Err(HarnessError::Runtime(format!(
            "memory: edited lesson {id} must remain candidate"
        )));
    }
    store.write_lesson(&parsed)?;
    store.append_log(&format!("edited {id}"))?;
    let _ = std::fs::remove_file(temp_path);
    Ok(())
}

pub fn run_memory_remember(workspace: &Path, text: &str, tags: &[String]) -> Result<()> {
    let store = MemoryStore::for_workspace(workspace)?;
    let id = format!("lesson-{}", store.list_all()?.len() + 1);
    let lesson = Lesson {
        id: id.clone(),
        status: LessonStatus::Active,
        source: LessonSource::UserTaught,
        created: "unset".to_string(),
        last_confirmed: "unset".to_string(),
        last_used: None,
        evidence_runs: Vec::new(),
        tags: tags.to_vec(),
        observed_commands: vec![],
        episode_ref: None,
        body: text.to_string(),
    };
    store.write_lesson(&lesson)?;
    store.append_log(&format!("created {id} (user_taught)"))?;
    Ok(())
}

fn read_input(input: &str) -> Result<String> {
    let path = Path::new(input);
    if path.exists() {
        return Ok(std::fs::read_to_string(path)?);
    }
    Ok(input.to_string())
}

fn resolve_journal_root(journal_dir: Option<PathBuf>, _workspace: &Path) -> PathBuf {
    if let Some(dir) = journal_dir {
        return dir;
    }
    if let Ok(val) = std::env::var("MYAGENT_JOURNAL_DIR") {
        if !val.is_empty() {
            return PathBuf::from(val);
        }
    }
    crate::config::default_journal_root()
}

fn read_saved_provider(workspace: &Path, run_id: &str) -> Result<String> {
    let path = workspace
        .join(".myagenthubs")
        .join("runs")
        .join(run_id)
        .join("conversation.json");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    value
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| HarnessError::Runtime(format!("run {run_id} does not record provider")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn make_candidate(id: &str) -> Lesson {
        Lesson {
            id: id.to_string(),
            status: LessonStatus::Candidate,
            source: LessonSource::AutoError,
            created: "t".to_string(),
            last_confirmed: "t".to_string(),
            last_used: None,
            evidence_runs: vec!["run-1".to_string()],
            tags: vec!["build".to_string()],
            observed_commands: vec!["cmd-1".to_string()],
            episode_ref: Some("win-0123456789abcdef".to_string()),
            body: "## 问题特征\ncargo build fails with E0463\n## 修复·做法\nRun `rustup update` before retrying.\n## 适用条件·边界\nRust toolchain drift in local workspace.\n".to_string(),
        }
    }

    #[test]
    fn run_args_parse_network_off() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["myagent", "run", "hi", "--network", "off"]).unwrap();
        match cli.command {
            Some(Command::Run(args)) => assert_eq!(args.network, crate::goal::NetworkPolicy::Off),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn run_args_network_defaults_on() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
        match cli.command {
            Some(Command::Run(args)) => assert_eq!(args.network, crate::goal::NetworkPolicy::On),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn fs_read_scope_defaults_to_workspace_for_all_cli_entrypoints() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["myagent"]).unwrap();
        assert_eq!(
            cli.interactive.fs_read_scope,
            crate::fs_scope::FsReadScope::Workspace
        );

        for argv in [
            vec!["myagent", "run", "hi"],
            vec!["myagent", "plan", "build"],
            vec!["myagent", "resume", "run-1"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            let scope = match cli.command.unwrap() {
                Command::Run(args) => args.fs_read_scope,
                Command::Plan(args) => args.fs_read_scope,
                Command::Resume(args) => args.fs_read_scope,
                other => panic!("unexpected command: {other:?}"),
            };
            assert_eq!(scope, crate::fs_scope::FsReadScope::Workspace);
        }
    }

    #[test]
    fn run_args_parse_explicit_fs_read_scope() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["myagent", "run", "hi", "--fs-read-scope", "project-deps"])
            .unwrap();
        match cli.command {
            Some(Command::Run(args)) => {
                assert_eq!(
                    args.fs_read_scope,
                    crate::fs_scope::FsReadScope::ProjectDeps
                );
            }
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn fs_write_fence_defaults_off_for_all_cli_entrypoints() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["myagent"]).unwrap();
        assert_eq!(
            cli.interactive.fs_write_fence,
            crate::exec::sandbox::FsWriteFence::Off
        );

        for argv in [
            vec!["myagent", "run", "hi"],
            vec!["myagent", "plan", "build"],
            vec!["myagent", "resume", "run-1"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            let fence = match cli.command.unwrap() {
                Command::Run(args) => args.fs_write_fence,
                Command::Plan(args) => args.fs_write_fence,
                Command::Resume(args) => args.fs_write_fence,
                other => panic!("unexpected command: {other:?}"),
            };
            assert_eq!(fence, crate::exec::sandbox::FsWriteFence::Off);
        }
    }

    #[test]
    fn run_args_parse_explicit_fs_write_fence() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["myagent", "run", "hi", "--fs-write-fence", "on"]).unwrap();
        match cli.command {
            Some(Command::Run(args)) => {
                assert_eq!(args.fs_write_fence, crate::exec::sandbox::FsWriteFence::On);
            }
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn max_turn_defaults_apply_to_all_cli_entrypoints() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["myagent"]).unwrap();
        assert_eq!(
            cli.interactive.max_turns,
            crate::orchestrator::MIN_TASK_TURN_BUDGET
        );

        let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
        match cli.command {
            Some(Command::Run(args)) => {
                assert_eq!(args.max_turns, crate::orchestrator::MIN_TASK_TURN_BUDGET);
            }
            other => panic!("expected run, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["myagent", "plan", "build"]).unwrap();
        match cli.command {
            Some(Command::Plan(args)) => {
                assert_eq!(args.max_turns, crate::orchestrator::MIN_TASK_TURN_BUDGET);
            }
            other => panic!("expected plan, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["myagent", "resume", "run-1"]).unwrap();
        match cli.command {
            Some(Command::Resume(args)) => {
                assert_eq!(args.max_turns, crate::orchestrator::MIN_TASK_TURN_BUDGET);
            }
            other => panic!("expected resume, got {other:?}"),
        }
    }

    #[test]
    fn plan_subcommand_parses_objective_and_knobs() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "myagent",
            "plan",
            "build the thing",
            "--provider",
            "mock",
            "--max-review-attempts",
            "4",
            "--max-plan-steps",
            "30",
            "--max-replan-rounds",
            "7",
            "--max-turns",
            "6",
            "--criteria",
            "cmd: cargo test",
            "--resume",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Plan(args)) => {
                assert_eq!(args.input, "build the thing");
                assert_eq!(args.provider, "mock");
                assert_eq!(args.max_review_attempts, 4);
                assert_eq!(args.max_plan_steps, 30);
                assert_eq!(args.max_replan_rounds, 7);
                assert_eq!(args.max_turns, 6);
                assert_eq!(args.criteria, vec!["cmd: cargo test".to_string()]);
                assert!(args.resume);
            }
            _ => panic!("expected plan"),
        }
    }

    #[test]
    fn plan_subcommand_defaults_max_replan_rounds_to_three() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["myagent", "plan", "build"]).unwrap();
        match cli.command {
            Some(Command::Plan(args)) => assert_eq!(args.max_replan_rounds, 3),
            _ => panic!("expected plan"),
        }
    }

    #[test]
    fn plan_subcommand_defaults_preflight_gate_on() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["myagent", "plan", "do the thing"]).unwrap();
        match cli.command {
            Some(Command::Plan(args)) => {
                assert!(matches!(args.preflight_gate, PreflightGate::On));
            }
            other => panic!("expected plan, got {other:?}"),
        }
    }

    #[test]
    fn plan_subcommand_parses_preflight_gate_off() {
        use clap::Parser;
        let cli =
            Cli::try_parse_from(["myagent", "plan", "do the thing", "--preflight-gate", "off"])
                .unwrap();
        match cli.command {
            Some(Command::Plan(args)) => {
                assert!(matches!(args.preflight_gate, PreflightGate::Off));
            }
            other => panic!("expected plan, got {other:?}"),
        }
    }

    #[test]
    fn verify_and_watchdog_defaults_apply_to_all_cli_entrypoints() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["myagent"]).unwrap();
        assert_eq!(
            cli.interactive.verify_every,
            crate::orchestrator::DEFAULT_VERIFY_EVERY
        );
        assert_eq!(
            cli.interactive.watchdog_repeat,
            crate::orchestrator::DEFAULT_WATCHDOG_REPEAT
        );

        let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
        match cli.command {
            Some(Command::Run(args)) => {
                assert_eq!(args.verify_every, crate::orchestrator::DEFAULT_VERIFY_EVERY);
                assert_eq!(
                    args.watchdog_repeat,
                    crate::orchestrator::DEFAULT_WATCHDOG_REPEAT
                );
            }
            _ => panic!("expected run"),
        }

        let cli = Cli::try_parse_from(["myagent", "resume", "run-1"]).unwrap();
        match cli.command {
            Some(Command::Resume(args)) => {
                assert_eq!(args.verify_every, crate::orchestrator::DEFAULT_VERIFY_EVERY);
                assert_eq!(
                    args.watchdog_repeat,
                    crate::orchestrator::DEFAULT_WATCHDOG_REPEAT
                );
            }
            _ => panic!("expected resume"),
        }
    }

    #[test]
    fn resume_args_parse_realign_flags() {
        use clap::Parser;

        let cli = Cli::try_parse_from([
            "myagent",
            "resume",
            "run-1",
            "--realign-objective",
            " ship smaller slice ",
            "--realign-criteria",
            "cmd: cargo test",
            "--realign-scope",
            " harness-agent ",
            "--realign-constraint",
            " no UI work ",
            "--realign-reason",
            "stuck repeating",
        ])
        .unwrap();

        let Some(Command::Resume(args)) = cli.command else {
            panic!("expected resume");
        };
        let input = resume_realign_input(&args).unwrap().expect("realign input");
        assert_eq!(input.objective.as_deref(), Some(" ship smaller slice "));
        assert_eq!(input.add_criteria.len(), 1);
        assert_eq!(input.add_criteria[0].id, "c1");
        assert_eq!(input.scope.as_deref(), Some(" harness-agent "));
        assert_eq!(input.add_constraints, vec![" no UI work "]);
        assert_eq!(input.reason, "stuck repeating");
    }

    #[test]
    fn resume_realign_reason_alone_is_noop() {
        use clap::Parser;

        let cli = Cli::try_parse_from([
            "myagent",
            "resume",
            "run-1",
            "--realign-reason",
            "only a reason",
        ])
        .unwrap();

        let Some(Command::Resume(args)) = cli.command else {
            panic!("expected resume");
        };
        assert!(resume_realign_input(&args).unwrap().is_none());
    }

    #[test]
    fn run_args_parse_learn_flags() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["myagent", "run", "do x", "--learn"]).unwrap();
        match cli.command {
            Some(Command::Run(args)) => assert!(args.learn && !args.auto_learn),
            _ => panic!("expected run"),
        }

        let cli = Cli::try_parse_from(["myagent", "run", "do x", "--auto-learn"]).unwrap();
        match cli.command {
            Some(Command::Run(args)) => assert!(args.auto_learn),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn parses_config_search_subcommand() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "myagent",
            "config",
            "search",
            "--backend",
            "brave",
            "--api-key",
            "k",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Config {
                command: ConfigCommand::Search(ConfigSearchArgs { backend, api_key }),
            }) => {
                assert_eq!(backend, "brave");
                assert_eq!(api_key, "k");
            }
            _ => panic!("expected config search"),
        }
    }

    #[test]
    #[serial]
    fn config_command_search_exa_writes_exa_config() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("MYAGENT_HOME", tmp.path());
        config_command(ConfigCommand::Search(ConfigSearchArgs {
            backend: "exa".into(),
            api_key: "k".into(),
        }))
        .unwrap();
        let loaded = crate::config::load_config().unwrap().search;
        assert!(
            matches!(loaded, Some(crate::config::SearchConfig::Exa { api_key }) if api_key == "k")
        );
        std::env::remove_var("MYAGENT_HOME");
    }

    #[test]
    #[serial]
    fn config_command_search_brave_unchanged_and_unknown_not_written() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("MYAGENT_HOME", tmp.path());
        config_command(ConfigCommand::Search(ConfigSearchArgs {
            backend: "brave".into(),
            api_key: "bk".into(),
        }))
        .unwrap();
        assert!(matches!(
            crate::config::load_config().unwrap().search,
            Some(crate::config::SearchConfig::Brave { api_key }) if api_key == "bk"
        ));
        config_command(ConfigCommand::Search(ConfigSearchArgs {
            backend: "bogus".into(),
            api_key: "x".into(),
        }))
        .unwrap();
        assert!(matches!(
            crate::config::load_config().unwrap().search,
            Some(crate::config::SearchConfig::Brave { .. })
        ));
        std::env::remove_var("MYAGENT_HOME");
    }

    #[test]
    #[serial]
    fn memory_remember_writes_active() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::env::set_var("MYAGENT_HOME", tmp.path());

        run_memory_remember(ws.path(), "cargo E0463 用 rustup update", &["build".into()]).unwrap();

        let store = MemoryStore::for_workspace(ws.path()).unwrap();
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].source, LessonSource::UserTaught);
        assert_eq!(active[0].status, LessonStatus::Active);

        let log = std::fs::read_to_string(store.root().join("log.md")).unwrap();
        assert!(log.contains("user_taught"));

        std::env::remove_var("MYAGENT_HOME");
    }

    #[test]
    #[serial]
    fn accept_promotes_and_respects_cap() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("MYAGENT_HOME", home.path());
        let ws = tempfile::tempdir().unwrap();
        let store = MemoryStore::for_workspace(ws.path()).unwrap();
        store.init().unwrap();

        store.write_lesson(&make_candidate("lesson-c1")).unwrap();
        accept_candidate(ws.path(), "lesson-c1").unwrap();
        assert_eq!(
            store.read_lesson("lesson-c1").unwrap().status,
            LessonStatus::Active
        );
        assert!(store.read_index().unwrap().contains("lesson-c1"));

        for i in 0..49 {
            let mut active = make_candidate(&format!("lesson-a{i}"));
            active.status = LessonStatus::Active;
            store.write_lesson(&active).unwrap();
        }
        assert_eq!(store.list_active().unwrap().len(), 50);
        store.write_lesson(&make_candidate("lesson-over")).unwrap();
        assert!(accept_candidate(ws.path(), "lesson-over").is_err());

        std::env::remove_var("MYAGENT_HOME");
    }

    #[test]
    #[serial]
    fn reject_archives_not_in_index() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("MYAGENT_HOME", home.path());
        let ws = tempfile::tempdir().unwrap();
        let store = MemoryStore::for_workspace(ws.path()).unwrap();
        store.init().unwrap();

        store.write_lesson(&make_candidate("lesson-r1")).unwrap();
        reject_candidate(ws.path(), "lesson-r1").unwrap();
        assert_eq!(
            store.read_lesson("lesson-r1").unwrap().status,
            LessonStatus::Archived
        );
        assert!(!store.read_index().unwrap().contains("lesson-r1"));

        std::env::remove_var("MYAGENT_HOME");
    }

    #[test]
    fn elevate_permission_raises_to_allow_only_when_always_used() {
        use crate::shell::PermissionPolicy::*;
        assert_eq!(elevate_permission(Ask, true), Allow);
        assert_eq!(elevate_permission(Ask, false), Ask);
        assert_eq!(elevate_permission(Deny, true), Allow);
        assert_eq!(elevate_permission(Allow, false), Allow);
    }

    // ─── --mcp-server / --append-system-prompt flag parsing ───

    #[test]
    fn run_args_parse_mcp_server_single() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "myagent",
            "run",
            "hi",
            "--mcp-server",
            "lead=http://127.0.0.1:9000/mcp",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Run(args)) => assert_eq!(
                args.mcp_server,
                vec![("lead".to_string(), "http://127.0.0.1:9000/mcp".to_string())]
            ),
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn run_args_parse_mcp_server_repeatable() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "myagent",
            "run",
            "hi",
            "--mcp-server",
            "lead=http://127.0.0.1:9000/mcp",
            "--mcp-server",
            "aux=https://example.com/mcp",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Run(args)) => assert_eq!(
                args.mcp_server,
                vec![
                    ("lead".to_string(), "http://127.0.0.1:9000/mcp".to_string()),
                    ("aux".to_string(), "https://example.com/mcp".to_string()),
                ]
            ),
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn run_args_mcp_server_defaults_empty() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
        match cli.command {
            Some(Command::Run(args)) => assert!(args.mcp_server.is_empty()),
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn run_args_mcp_server_missing_equals_is_rejected() {
        use clap::Parser;
        let err = Cli::try_parse_from(["myagent", "run", "hi", "--mcp-server", "lead-no-url"])
            .unwrap_err();
        assert!(err.to_string().contains("expected format"));
    }

    #[test]
    fn run_args_mcp_server_non_http_url_is_rejected() {
        use clap::Parser;
        let err = Cli::try_parse_from([
            "myagent",
            "run",
            "hi",
            "--mcp-server",
            "lead=ftp://example.com/mcp",
        ])
        .unwrap_err();
        assert!(err.to_string().contains("http:// or https://"));
    }

    #[test]
    fn run_args_mcp_server_empty_name_is_rejected() {
        use clap::Parser;
        let err = Cli::try_parse_from([
            "myagent",
            "run",
            "hi",
            "--mcp-server",
            "=http://example.com/mcp",
        ])
        .unwrap_err();
        assert!(err.to_string().contains("name must not be empty"));
    }

    #[test]
    fn run_args_parse_append_system_prompt() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "myagent",
            "run",
            "hi",
            "--append-system-prompt",
            "TEAM LEAD MODE: use dispatch_worker.",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Run(args)) => assert_eq!(
                args.append_system_prompt.as_deref(),
                Some("TEAM LEAD MODE: use dispatch_worker.")
            ),
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn run_args_append_system_prompt_defaults_none() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
        match cli.command {
            Some(Command::Run(args)) => assert!(args.append_system_prompt.is_none()),
            other => panic!("expected run, got {other:?}"),
        }
    }

    // ─── merge_mcp_servers: config vs. flag-injected servers ───

    fn mcp_cfg(name: &str, url: &str, trusted: bool) -> crate::mcp::config::McpServerConfig {
        crate::mcp::config::McpServerConfig {
            name: name.to_string(),
            command: String::new(),
            url: Some(url.to_string()),
            args: Vec::new(),
            env: Default::default(),
            trusted,
        }
    }

    #[test]
    fn merge_mcp_servers_flag_overrides_same_name_config_server_and_is_trusted() {
        let config_servers = vec![
            mcp_cfg("serverA", "http://config-a.example/mcp", false),
            mcp_cfg("serverB", "http://config-b.example/mcp", true),
        ];
        let flag_servers = vec![(
            "serverA".to_string(),
            "http://flag-a.example/mcp".to_string(),
        )];
        let merged = merge_mcp_servers(config_servers, flag_servers);

        let a = merged.iter().find(|s| s.name == "serverA").unwrap();
        assert_eq!(a.url.as_deref(), Some("http://flag-a.example/mcp"));
        assert!(a.trusted, "flag-injected server must be trusted");

        // serverB is untouched by the flag override.
        let b = merged.iter().find(|s| s.name == "serverB").unwrap();
        assert_eq!(b.url.as_deref(), Some("http://config-b.example/mcp"));
        assert!(b.trusted);

        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_mcp_servers_no_flags_returns_config_servers_unchanged() {
        let config_servers = vec![mcp_cfg("serverB", "http://config-b.example/mcp", true)];
        let merged = merge_mcp_servers(config_servers.clone(), Vec::new());
        assert_eq!(merged, config_servers);
    }

    #[test]
    fn merge_mcp_servers_new_flag_name_adds_to_config_servers() {
        let config_servers = vec![mcp_cfg("serverB", "http://config-b.example/mcp", true)];
        let flag_servers = vec![("lead".to_string(), "http://127.0.0.1:9000/mcp".to_string())];
        let merged = merge_mcp_servers(config_servers, flag_servers);
        assert_eq!(merged.len(), 2);
        let lead = merged.iter().find(|s| s.name == "lead").unwrap();
        assert_eq!(lead.url.as_deref(), Some("http://127.0.0.1:9000/mcp"));
        assert!(lead.trusted);
    }
}
