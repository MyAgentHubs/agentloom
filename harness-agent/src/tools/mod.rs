pub mod fs_edit;
pub mod fs_read;
pub mod fs_write;
pub mod glob;
pub mod grep;
pub mod ls;
pub mod search;
pub mod shell_exec;
pub mod web_search;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{HarnessError, Result};
use crate::events::EventRecorder;
use crate::provider::ToolCall;

const CHECKPOINT_ENDPOINT_ENV: &str = "AGENTLOOM_CHECKPOINT_ENDPOINT";
const CHECKPOINT_TOKEN_ENV: &str = "AGENTLOOM_CHECKPOINT_TOKEN";
const CHECKPOINT_TOKEN_HEADER: &str = "X-AgentLoom-Token";
/// Cross-backend invariant: myagent must use the same 600 s loopback checkpoint
/// HTTP ceiling as the app-owned Claude/Codex PreToolUse hook, and that ceiling
/// must stay above the app server's 10 s SQLite busy timeout so the engine does
/// not give up while the app can still be waiting to commit the checkpoint row.
const CHECKPOINT_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Success,
    Rejected,
    FailedRecoverable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub status: ToolStatus,
    pub content: String,
    pub invalidates_verification: bool,
}

impl ToolOutcome {
    pub fn success(content: String) -> Self {
        Self {
            status: ToolStatus::Success,
            content,
            invalidates_verification: false,
        }
    }

    pub fn success_mutating(content: String) -> Self {
        Self {
            status: ToolStatus::Success,
            content,
            invalidates_verification: true,
        }
    }

    pub fn recoverable(content: String) -> Self {
        Self {
            status: ToolStatus::FailedRecoverable,
            content,
            invalidates_verification: false,
        }
    }

    pub fn rejected(content: String) -> Self {
        Self {
            status: ToolStatus::Rejected,
            content,
            invalidates_verification: false,
        }
    }
}

/// 工具执行上下文：workspace 根 + 事件录制器。
pub struct ToolContext<'a> {
    pub workspace: &'a Path,
    pub recorder: &'a mut EventRecorder,
    pub file_ledger: &'a mut crate::file_ledger::FileLedger,
    pub network: crate::goal::NetworkPolicy,
    pub fs_read_scope: crate::fs_scope::FsReadScope,
}

pub fn humanize_args_error(raw_args: &str, err: &serde_json::Error, required: &[&str]) -> String {
    match serde_json::from_str::<serde_json::Value>(raw_args) {
        Err(_) => format!("arguments are not valid JSON: {err}"),
        Ok(serde_json::Value::Object(map)) => {
            let missing: Vec<&str> = required
                .iter()
                .copied()
                .filter(|k| !map.contains_key(*k))
                .collect();
            let mut msg = if !missing.is_empty() {
                format!("missing required parameter(s): {}", missing.join(", "))
            } else {
                format!("bad arguments: {err}")
            };
            if err.to_string().contains("invalid type") {
                msg.push_str("; check parameter types (e.g. replace_all must be a boolean)");
            }
            msg
        }
        Ok(_) => format!("bad arguments: {err}"),
    }
}

/// 启发式：工具参数是否疑似「被输出上限切断」——JSON 撞 EOF + 原串够长（排除琐碎 malformed）。
pub fn is_truncated_args(raw_args: &str, err: &serde_json::Error) -> bool {
    err.classify() == serde_json::error::Category::Eof && raw_args.len() > 256
}

/// 被切断时给模型的 tool-aware 友好引导（讲人话·不漏 serde/JSON/token 字样）。
pub fn truncated_args_message(tool_name: &str) -> String {
    if tool_name == "fs_edit" {
        "Your previous tool call was cut off because the response hit the output length limit, so the change was NOT applied. Split this into several smaller edits.".to_string()
    } else {
        "Your previous tool call was cut off because the response hit the output length limit, so the change was NOT applied. Use fs_edit to change only the specific lines you need instead of writing the whole file.".to_string()
    }
}

/// 统一工具接口。每个工具负责在 execute 内发 tool.started / stdout.delta /
/// stderr.delta / completed / failed（用本模块 helper 保持 envelope 形状一致）。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 工具名（= provider function name，如 "shell_exec" / "fs_read"）。
    fn name(&self) -> &str;

    /// OpenAI function-calling schema（registry.definitions() 汇总给 provider）。
    fn definition(&self) -> Value;

    /// 是否需经 Guardrails（写/改/执行=true；只读=false）。
    fn mutates(&self) -> bool;

    /// MCP server 标 trusted 的工具：免每次审批弹窗（但 --permission deny 仍拒）。默认 false。
    fn guardrail_trusted(&self) -> bool {
        false
    }

    /// 本工具是否来自 MCP（server 注入·非内建）。默认 false；MCP 代理工具（`McpToolProxy` /
    /// `McpResourceListTool` / `McpResourceReadTool`）覆写为 true。
    /// 用途（K1·安全网）：run_loop 据此识别「MCP 工具成功调用」，把它计入进度信号（novel call
    /// 去重后视同新读）——按注册来源判断，不靠工具名前缀猜（名字是外部约定，不该当结构判据）。
    fn is_mcp(&self) -> bool {
        false
    }

    /// 本工具执行时是否需要对外联网（HTTP 等）。默认否。
    /// 要联网的工具受联网门约束：--network off 时不出现在工具清单、也不执行。
    fn requires_network(&self) -> bool {
        false
    }

    /// 本次调用会写入的绝对路径（供 Guardrails 做 workspace 限定）。只读工具返回空。
    fn write_targets(&self, args: &str, workspace: &Path) -> Result<Vec<PathBuf>> {
        let _ = (args, workspace);
        Ok(Vec::new())
    }

    /// 执行工具，返回可直接回灌给 provider 的工具结果。
    async fn execute(&self, ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome>;
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.as_ref())
    }

    pub fn definitions(&self) -> Vec<Value> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    /// 按当前联网策略给出可注入的工具定义：联网关时剔除「要联网」的工具。
    pub fn definitions_for(&self, network: crate::goal::NetworkPolicy) -> Vec<Value> {
        self.tools
            .iter()
            .filter(|t| !(t.requires_network() && network == crate::goal::NetworkPolicy::Off))
            .map(|t| t.definition())
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 共享事件 helper（各工具复用，保证 tool.* envelope 字段一致）。
pub fn emit_tool_started(
    recorder: &mut EventRecorder,
    tool: &str,
    tool_call_id: &str,
    extra: Value,
) -> Result<()> {
    let mut payload = json!({ "tool": tool, "tool_call_id": tool_call_id });
    merge(&mut payload, extra);
    recorder.emit("tool.started", payload)?;
    Ok(())
}

pub fn emit_tool_completed(
    recorder: &mut EventRecorder,
    tool: &str,
    tool_call_id: &str,
    extra: Value,
) -> Result<()> {
    let mut payload = json!({ "tool": tool, "tool_call_id": tool_call_id });
    merge(&mut payload, extra);
    recorder.emit("tool.completed", payload)?;
    Ok(())
}

pub fn emit_tool_failed(
    recorder: &mut EventRecorder,
    tool: &str,
    tool_call_id: &str,
    error: &str,
) -> Result<()> {
    emit_tool_failed_with_extra(recorder, tool, tool_call_id, error, Value::Null)?;
    Ok(())
}

pub fn emit_tool_failed_with_extra(
    recorder: &mut EventRecorder,
    tool: &str,
    tool_call_id: &str,
    error: &str,
    extra: Value,
) -> Result<()> {
    let mut payload = json!({ "tool": tool, "tool_call_id": tool_call_id, "error": error });
    merge(&mut payload, extra);
    recorder.emit("tool.failed", payload)?;
    Ok(())
}

/// 工具对外联网前必调的共享检查：联网关着就拒绝（返回给模型的 in-band 错误串）。
/// shell 那条靠子进程沙箱，这条靠这道代码判断（平台无关·Windows/Linux/Mac 一致）。
pub fn check_network_egress(
    network: crate::goal::NetworkPolicy,
) -> std::result::Result<(), String> {
    match network {
        crate::goal::NetworkPolicy::On => Ok(()),
        crate::goal::NetworkPolicy::Off => {
            Err("network off: this tool requires network and is disabled".to_string())
        }
    }
}

#[derive(Debug, Clone)]
struct CheckpointHookConfig {
    endpoint: reqwest::Url,
    token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckpointTargetState {
    Missing,
    Existing(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckpointTargetIdentity {
    Missing,
    BrokenSymlink(PathBuf),
    Node(CheckpointNodeIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckpointNodeIdentity {
    kind: CheckpointNodeKind,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckpointNodeKind {
    File,
    Directory,
    Other,
}

impl CheckpointNodeIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        let kind = if metadata.is_file() {
            CheckpointNodeKind::File
        } else if metadata.is_dir() {
            CheckpointNodeKind::Directory
        } else {
            CheckpointNodeKind::Other
        };
        Self {
            kind,
            #[cfg(unix)]
            dev: metadata.dev(),
            #[cfg(unix)]
            ino: metadata.ino(),
        }
    }
}

pub(crate) fn checkpoint_target_identity(path: &Path) -> std::io::Result<CheckpointTargetIdentity> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(
            CheckpointTargetIdentity::BrokenSymlink(std::fs::read_link(path)?),
        ),
        Ok(metadata) => Ok(CheckpointTargetIdentity::Node(
            CheckpointNodeIdentity::from_metadata(&metadata),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(CheckpointTargetIdentity::Missing)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn checkpoint_target_state(path: &Path) -> std::io::Result<CheckpointTargetState> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(CheckpointTargetState::Existing(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(CheckpointTargetState::Missing)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn revalidate_target_state_after_checkpoint(
    tool_name: &str,
    workspace: &Path,
    requested_path: &str,
    expected_path: &Path,
    expected: &CheckpointTargetState,
    expected_identity: &CheckpointTargetIdentity,
) -> Result<PathBuf> {
    let path = match crate::tools::fs_read::resolve_in_workspace(workspace, requested_path) {
        Ok(path) => path,
        Err(HarnessError::PermissionDenied(_)) => {
            return Err(HarnessError::Runtime(format!(
                "{tool_name}: requested path resolved outside the workspace during checkpoint wait; aborting without writes: {requested_path}"
            )));
        }
        Err(error) => {
            return Err(HarnessError::Runtime(format!(
                "{tool_name}: cannot re-resolve {requested_path} after checkpoint: {error}"
            )));
        }
    };
    if path != expected_path {
        return Err(HarnessError::Runtime(format!(
            "{tool_name}: requested path resolved to a different target during checkpoint wait; aborting without writes: {}",
            path.display()
        )));
    }
    let observed = checkpoint_target_state(&path).map_err(|error| {
        HarnessError::Runtime(format!(
            "{tool_name}: cannot revalidate {} after checkpoint: {error}",
            path.display()
        ))
    })?;
    if observed != *expected {
        let detail = match (expected, &observed) {
            (CheckpointTargetState::Missing, CheckpointTargetState::Existing(_)) => {
                "target was created during checkpoint wait"
            }
            (CheckpointTargetState::Existing(_), CheckpointTargetState::Missing) => {
                "target was deleted during checkpoint wait"
            }
            (CheckpointTargetState::Existing(_), CheckpointTargetState::Existing(_)) => {
                "target content changed during checkpoint wait"
            }
            (CheckpointTargetState::Missing, CheckpointTargetState::Missing) => unreachable!(),
        };
        return Err(target_state_mismatch_error(tool_name, &path, detail));
    }
    let observed_identity = checkpoint_target_identity(&path).map_err(|error| {
        HarnessError::Runtime(format!(
            "{tool_name}: cannot revalidate {} identity after checkpoint: {error}",
            path.display()
        ))
    })?;
    if let CheckpointTargetIdentity::BrokenSymlink(target) = &observed_identity {
        return Err(HarnessError::Runtime(format!(
            "{tool_name}: target became a broken symlink during checkpoint wait (-> {}); aborting without writes: {}",
            target.display(),
            path.display()
        )));
    }
    if observed_identity != *expected_identity {
        return Err(HarnessError::Runtime(format!(
            "{tool_name}: {}; aborting without writes: {}",
            checkpoint_identity_change_detail(expected_identity, &observed_identity),
            path.display()
        )));
    }
    Ok(path)
}

fn checkpoint_identity_change_detail(
    expected: &CheckpointTargetIdentity,
    observed: &CheckpointTargetIdentity,
) -> String {
    match (expected, observed) {
        (CheckpointTargetIdentity::Missing, CheckpointTargetIdentity::BrokenSymlink(target)) => {
            format!(
                "target identity changed during checkpoint wait (missing target became a broken symlink to {})",
                target.display()
            )
        }
        (CheckpointTargetIdentity::Node(_), CheckpointTargetIdentity::BrokenSymlink(target)) => {
            format!(
                "target identity changed during checkpoint wait (file became a broken symlink to {})",
                target.display()
            )
        }
        (
            CheckpointTargetIdentity::Node(expected),
            CheckpointTargetIdentity::Node(observed),
        ) if expected.kind != observed.kind => {
            format!(
                "target type changed during checkpoint wait ({} -> {})",
                checkpoint_node_kind_label(expected.kind),
                checkpoint_node_kind_label(observed.kind)
            )
        }
        (
            CheckpointTargetIdentity::BrokenSymlink(expected),
            CheckpointTargetIdentity::BrokenSymlink(observed),
        ) => format!(
            "target identity changed during checkpoint wait (broken symlink target changed from {} to {})",
            expected.display(),
            observed.display()
        ),
        (CheckpointTargetIdentity::Missing, CheckpointTargetIdentity::Node(_)) => {
            "target identity changed during checkpoint wait".to_string()
        }
        (CheckpointTargetIdentity::BrokenSymlink(_), CheckpointTargetIdentity::Missing) => {
            "target identity changed during checkpoint wait (broken symlink disappeared)"
                .to_string()
        }
        (
            CheckpointTargetIdentity::BrokenSymlink(expected),
            CheckpointTargetIdentity::Node(_),
        ) => format!(
            "target identity changed during checkpoint wait (broken symlink to {} became a file)",
            expected.display()
        ),
        (
            CheckpointTargetIdentity::Node(_),
            CheckpointTargetIdentity::Node(_),
        ) => "target identity changed during checkpoint wait".to_string(),
        (CheckpointTargetIdentity::Node(_), CheckpointTargetIdentity::Missing) => {
            "target identity changed during checkpoint wait (file disappeared)".to_string()
        }
        (CheckpointTargetIdentity::Missing, CheckpointTargetIdentity::Missing) => unreachable!(),
    }
}

fn checkpoint_node_kind_label(kind: CheckpointNodeKind) -> &'static str {
    match kind {
        CheckpointNodeKind::File => "file",
        CheckpointNodeKind::Directory => "directory",
        CheckpointNodeKind::Other => "non-file object",
    }
}

pub(crate) fn target_state_mismatch_error(
    tool_name: &str,
    path: &Path,
    detail: &str,
) -> HarnessError {
    HarnessError::Runtime(format!(
        "{tool_name}: {detail}; aborting without writes: {}",
        path.display()
    ))
}

pub(crate) async fn checkpoint_pre_write(tool_name: &str, path: &Path) -> Result<bool> {
    let Some(config) = checkpoint_hook_config()? else {
        return Ok(false);
    };
    if !path.is_absolute() {
        return Err(HarnessError::Runtime(format!(
            "checkpoint hook requires an absolute path, got {}",
            path.display()
        )));
    }
    let path = path.to_str().ok_or_else(|| {
        HarnessError::Runtime(format!(
            "checkpoint hook requires a UTF-8 path, got {}",
            path.display()
        ))
    })?;
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(CHECKPOINT_HTTP_TIMEOUT)
        .build()
        .map_err(|error| {
            HarnessError::Runtime(format!("checkpoint hook client build failed: {error}"))
        })?
        .post(config.endpoint.clone())
        .header(CHECKPOINT_TOKEN_HEADER, config.token)
        .json(&json!({
            "hook_event_name": "PreToolUse",
            "tool_name": tool_name,
            "tool_input": { "path": path },
        }))
        .send()
        .await
        .map_err(|error| {
            HarnessError::Runtime(format!(
                "checkpoint hook request failed for {tool_name} {path}: {error}"
            ))
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = body.trim();
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        return Err(HarnessError::Runtime(format!(
            "checkpoint hook rejected {tool_name} {path}: HTTP {status}{detail}"
        )));
    }
    Ok(true)
}

fn checkpoint_hook_config() -> Result<Option<CheckpointHookConfig>> {
    #[cfg(test)]
    if let Some(override_env) = checkpoint_env_override_for_test() {
        return checkpoint_hook_config_from_raw(override_env.endpoint, override_env.token);
    }
    checkpoint_hook_config_from_raw(
        std::env::var(CHECKPOINT_ENDPOINT_ENV).ok(),
        std::env::var(CHECKPOINT_TOKEN_ENV).ok(),
    )
}

fn checkpoint_hook_config_from_raw(
    endpoint: Option<String>,
    token: Option<String>,
) -> Result<Option<CheckpointHookConfig>> {
    match (endpoint, token) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(HarnessError::InvalidConfig(format!(
            "checkpoint hook misconfigured: {CHECKPOINT_ENDPOINT_ENV} and {CHECKPOINT_TOKEN_ENV} must both be set"
        ))),
        (Some(endpoint), Some(token)) => Ok(Some(CheckpointHookConfig {
            endpoint: parse_checkpoint_endpoint(&endpoint)?,
            token: parse_checkpoint_token(&token)?,
        })),
    }
}

fn parse_checkpoint_token(token: &str) -> Result<String> {
    if token.trim().is_empty() {
        return Err(HarnessError::InvalidConfig(format!(
            "checkpoint hook misconfigured: {CHECKPOINT_TOKEN_ENV} must not be empty"
        )));
    }
    Ok(token.to_string())
}

fn parse_checkpoint_endpoint(endpoint: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(endpoint).map_err(|error| {
        HarnessError::InvalidConfig(format!(
            "checkpoint hook misconfigured: {CHECKPOINT_ENDPOINT_ENV} is invalid: {error}"
        ))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HarnessError::InvalidConfig(format!(
            "checkpoint hook misconfigured: {CHECKPOINT_ENDPOINT_ENV} must use http or https"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(HarnessError::InvalidConfig(format!(
            "checkpoint hook misconfigured: {CHECKPOINT_ENDPOINT_ENV} must not include userinfo"
        )));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(HarnessError::InvalidConfig(format!(
            "checkpoint hook misconfigured: {CHECKPOINT_ENDPOINT_ENV} must not include query or fragment"
        )));
    }
    let is_loopback = url
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| address.is_loopback());
    if !is_loopback {
        return Err(HarnessError::InvalidConfig(format!(
            "checkpoint hook misconfigured: {CHECKPOINT_ENDPOINT_ENV} must use a loopback IP address"
        )));
    }
    Ok(url)
}

fn merge(target: &mut Value, extra: Value) {
    if let (Some(t), Value::Object(e)) = (target.as_object_mut(), extra) {
        for (k, v) in e {
            t.insert(k, v);
        }
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct CheckpointEnvOverride {
    pub endpoint: Option<String>,
    pub token: Option<String>,
}

#[cfg(test)]
tokio::task_local! {
    static CHECKPOINT_ENV_OVERRIDE: CheckpointEnvOverride;
}

#[cfg(test)]
pub(crate) async fn with_checkpoint_env_override_for_test<Fut>(
    endpoint: Option<String>,
    token: Option<String>,
    future: Fut,
) -> Fut::Output
where
    Fut: std::future::Future,
{
    CHECKPOINT_ENV_OVERRIDE
        .scope(CheckpointEnvOverride { endpoint, token }, future)
        .await
}

#[cfg(test)]
pub(crate) fn checkpoint_env_override_for_test() -> Option<CheckpointEnvOverride> {
    CHECKPOINT_ENV_OVERRIDE.try_with(Clone::clone).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventRecorder;
    use crate::provider::{FunctionCall, ToolCall};

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn definition(&self) -> Value {
            json!({ "type": "function", "function": { "name": "echo" } })
        }
        fn mutates(&self) -> bool {
            false
        }
        async fn execute(
            &self,
            _ctx: &mut ToolContext<'_>,
            call: &ToolCall,
        ) -> Result<ToolOutcome> {
            Ok(ToolOutcome::success(call.function.arguments.clone()))
        }
    }

    struct NetTool;
    #[async_trait]
    impl Tool for NetTool {
        fn name(&self) -> &str {
            "net"
        }
        fn definition(&self) -> Value {
            json!({ "type": "function", "function": { "name": "net" } })
        }
        fn mutates(&self) -> bool {
            false
        }
        fn requires_network(&self) -> bool {
            true
        }
        async fn execute(
            &self,
            _ctx: &mut ToolContext<'_>,
            _call: &ToolCall,
        ) -> Result<ToolOutcome> {
            Ok(ToolOutcome::success("ok".into()))
        }
    }

    #[test]
    fn truncated_args_detected_and_tool_aware() {
        // 半截 JSON（content 串没收尾）→ serde EOF
        let raw = format!("{{\"path\":\"a.rs\",\"content\":\"{}", "x".repeat(300));
        let err = serde_json::from_str::<serde_json::Value>(&raw).unwrap_err();
        assert!(super::is_truncated_args(&raw, &err));
        // 短的真 malformed 不误判为截断
        let short = "{\"path\":}";
        let serr = serde_json::from_str::<serde_json::Value>(short).unwrap_err();
        assert!(!super::is_truncated_args(short, &serr));
        // tool-aware：fs_write 引导用 fs_edit；fs_edit 不被叫去「用 fs_edit」
        assert!(super::truncated_args_message("fs_write").contains("fs_edit"));
        assert!(!super::truncated_args_message("fs_edit").contains("Use fs_edit"));
    }

    #[test]
    fn requires_network_defaults_false_and_can_override() {
        assert!(!EchoTool.requires_network());
        assert!(NetTool.requires_network());
    }

    #[test]
    fn check_network_egress_blocks_when_off() {
        assert!(check_network_egress(crate::goal::NetworkPolicy::On).is_ok());
        let err = check_network_egress(crate::goal::NetworkPolicy::Off).unwrap_err();
        assert!(err.contains("network off"));
    }

    #[test]
    fn registry_lookup_and_definitions() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(EchoTool));
        assert!(reg.get("echo").is_some());
        assert!(reg.get("missing").is_none());
        assert_eq!(reg.definitions().len(), 1);
        assert_eq!(reg.get("echo").unwrap().name(), "echo");
        assert!(!reg.get("echo").unwrap().mutates());
    }

    #[test]
    fn definitions_for_filters_network_tools_when_off() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(EchoTool));
        reg.register(Box::new(NetTool));
        assert_eq!(reg.definitions_for(crate::goal::NetworkPolicy::On).len(), 2);
        let off = reg.definitions_for(crate::goal::NetworkPolicy::Off);
        assert_eq!(off.len(), 1);
        assert_eq!(off[0]["function"]["name"], "echo");
    }

    #[test]
    fn verify_reflex_outcome_invalidates_verification_truth_table() {
        assert!(ToolOutcome::success_mutating("ok".into()).invalidates_verification);
        assert!(!ToolOutcome::success("ok".into()).invalidates_verification);
        assert!(!ToolOutcome::recoverable("try again".into()).invalidates_verification);
        assert!(!ToolOutcome::rejected("denied".into()).invalidates_verification);
    }

    #[tokio::test]
    async fn echo_tool_returns_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("e.jsonl");
        let mut rec = EventRecorder::new(
            "run_t",
            None,
            None,
            &journal,
            crate::events::OutputMode::Silent,
        )
        .unwrap();
        let mut ledger = crate::file_ledger::FileLedger::new();
        let mut ctx = ToolContext {
            workspace: dir.path(),
            recorder: &mut rec,
            file_ledger: &mut ledger,
            network: crate::goal::NetworkPolicy::On,
            fs_read_scope: crate::fs_scope::FsReadScope::Workspace,
        };
        let call = ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "echo".into(),
                arguments: "{\"x\":1}".into(),
            },
        };
        let out = EchoTool.execute(&mut ctx, &call).await.unwrap();
        assert_eq!(out.content, "{\"x\":1}");
        assert_eq!(out.status, ToolStatus::Success);
    }

    #[test]
    fn checkpoint_hook_config_all_missing_disables_feature() {
        let config = checkpoint_hook_config_from_raw(None, None).unwrap();
        assert!(config.is_none());
    }

    #[test]
    fn checkpoint_hook_config_requires_both_envs() {
        let err =
            checkpoint_hook_config_from_raw(Some("http://127.0.0.1:9/checkpoint".into()), None)
                .unwrap_err();
        assert!(err.to_string().contains("checkpoint hook misconfigured"));
        let err = checkpoint_hook_config_from_raw(None, Some("secret".into())).unwrap_err();
        assert!(err.to_string().contains(CHECKPOINT_TOKEN_ENV));
    }

    #[test]
    fn checkpoint_hook_config_rejects_non_loopback_hosts() {
        let err = checkpoint_hook_config_from_raw(
            Some("http://example.com/checkpoint".into()),
            Some("secret".into()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("loopback"));
    }

    #[tokio::test]
    async fn checkpoint_hook_rejects_relative_paths() {
        let err = with_checkpoint_env_override_for_test(
            Some("http://127.0.0.1:9/checkpoint".into()),
            Some("secret".into()),
            async { checkpoint_pre_write("fs_write", Path::new("relative.txt")).await },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("absolute path"));
    }

    #[test]
    fn checkpoint_http_timeout_exceeds_app_sqlite_busy_timeout() {
        assert_eq!(CHECKPOINT_HTTP_TIMEOUT, std::time::Duration::from_secs(600));
        assert!(CHECKPOINT_HTTP_TIMEOUT > std::time::Duration::from_secs(10));
    }
}
