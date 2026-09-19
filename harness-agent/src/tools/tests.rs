#![cfg(test)]

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
    async fn execute(&self, _ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<ToolOutcome> {
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
    async fn execute(&self, _ctx: &mut ToolContext<'_>, _call: &ToolCall) -> Result<ToolOutcome> {
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
        extra_read_roots: &[],
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
    let err = checkpoint_hook_config_from_raw(Some("http://127.0.0.1:9/checkpoint".into()), None)
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
