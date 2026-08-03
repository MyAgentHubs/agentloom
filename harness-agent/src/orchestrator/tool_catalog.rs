use std::collections::BTreeSet;

use crate::config::SearchChoice;
use crate::tools::shell_exec::ShellExecTool;
use crate::tools::ToolRegistry;

/// 纯函数（可单测）：选中后端的标识。
pub fn search_backend_kind(choice: &SearchChoice) -> &'static str {
    match choice {
        SearchChoice::Brave { .. } => "fallback_brave_ddg",
        SearchChoice::Exa { .. } => "fallback_exa_ddg",
        SearchChoice::Ddg => "ddg",
    }
}

pub(crate) fn make_search_backend(
    choice: &SearchChoice,
) -> crate::tools::web_search::WebSearchTool {
    use crate::tools::search::{
        brave::BraveBackend, duckduckgo::DuckDuckGoBackend, exa::ExaBackend,
        fallback::FallbackBackend, retry::RetryBackend,
    };
    use crate::tools::web_search::WebSearchTool;

    match choice {
        SearchChoice::Brave { api_key } => {
            WebSearchTool::with_backend(Box::new(FallbackBackend::new(
                Box::new(RetryBackend::new(Box::new(BraveBackend::new(
                    api_key.clone(),
                )))),
                Box::new(DuckDuckGoBackend::default()),
            )))
        }
        SearchChoice::Exa { api_key } => {
            WebSearchTool::with_backend(Box::new(FallbackBackend::new(
                Box::new(RetryBackend::new(Box::new(ExaBackend::new(
                    api_key.clone(),
                )))),
                Box::new(DuckDuckGoBackend::default()),
            )))
        }
        SearchChoice::Ddg => WebSearchTool::default(),
    }
}

pub fn build_default_registry(search: &SearchChoice, memory_enabled: bool) -> ToolRegistry {
    build_default_registry_with_write_fence(
        search,
        memory_enabled,
        crate::exec::sandbox::FsWriteFence::Off,
    )
}

pub fn build_default_registry_with_write_fence(
    search: &SearchChoice,
    memory_enabled: bool,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ShellExecTool.with_write_fence(fs_write_fence)));
    registry.register(Box::new(crate::tools::fs_read::FsReadTool));
    registry.register(Box::new(crate::tools::ls::LsTool));
    registry.register(Box::new(crate::tools::glob::GlobTool));
    registry.register(Box::new(crate::tools::grep::GrepTool));
    registry.register(Box::new(crate::tools::fs_write::FsWriteTool));
    registry.register(Box::new(crate::tools::fs_edit::FsEditTool));
    registry.register(Box::new(make_search_backend(search)));
    if memory_enabled {
        registry.register(Box::new(crate::memory::tool::MemoryLookupTool));
    }
    registry
}

/// inline 派发的提议类工具定义。安全不变量：这些工具**绝不能** requires_network——
/// 它们在登记处外被单独派发，不经 definitions_for 过滤、也不经分发处联网闸。收编进登记处后才可解禁。
fn propose_scope_change_def() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "propose_scope_change",
            "description": "Adjust the task boundary. For kind=scope WITH a `paths` list, your editable file scope is widened and the run CONTINUES. For objective/constraint (or scope without paths): if a human/decision channel is available the run STOPS for the user to decide; otherwise the change is rejected with guidance and the run CONTINUES under the existing contract.",
            "parameters": { "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["scope", "objective", "constraint"] },
                    "detail": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" },
                        "description": "For kind=scope: concrete crate-relative files to add to your editable scope so you can keep going without stopping." } },
                "required": ["kind", "detail"] }
        }
    })
}

/// inline 派发的提议类工具定义。安全不变量：这些工具**绝不能** requires_network——
/// 它们在登记处外被单独派发，不经 definitions_for 过滤、也不经分发处联网闸。收编进登记处后才可解禁。
fn propose_criterion_def() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "propose_criterion",
            "description": "Propose a verifiable acceptance criterion for the current goal.",
            "parameters": { "type": "object",
                "properties": {
                    "claim": { "type": "string" }, "check_cmd": { "type": "string" },
                    "success": { "anyOf": [
                        { "type": "string", "enum": ["exit_zero"] },
                        { "type": "object", "properties": { "contains": { "type": "string" } }, "required": ["contains"] } ] },
                    "timeout_s": { "type": "integer" } },
                "required": ["claim", "check_cmd"] }
        }
    })
}

/// inline 派发的 issue 复现注册工具定义。安全不变量：这个工具**绝不能** requires_network——
/// 它在登记处外被单独派发，不经 definitions_for 过滤、也不经分发处联网闸。收编进登记处后才可解禁。
fn register_issue_probe_def() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "register_issue_probe",
            "description": "Register a reproduction that FAILS on the current (buggy) code. The harness runs it itself, twice, and confirms it is genuinely red — your claim that it reproduces is not accepted as evidence. You must register a confirmed-red probe before you may edit source files. The script is stored outside the repository and never enters your patch. After each edit the harness re-runs the frozen probe automatically; the task is complete only when it turns green. Editing the probe afterwards invalidates it.",
            "parameters": {
                "type": "object",
                "properties": {
                    "script": {
                        "type": "string",
                        "description": "Full source of the reproduction. It must exercise the reported behaviour through the real product API — not grep source text, not exit 1 unconditionally."
                    },
                    "command": {
                        "type": "string",
                        "description": "Shell command that runs it. Use {probe} as the placeholder for the script's absolute path. Defaults to 'python -I -B {probe}'."
                    },
                    "red_marker": {
                        "type": "string",
                        "description": "A non-empty substring that appears in the output ONLY when the bug is present — e.g. the wrong value it prints, or your assertion message. A bare non-zero exit is NOT accepted: a typo also exits non-zero."
                    },
                    "marker_stream": {
                        "type": "string",
                        "enum": ["stdout", "stderr", "any"],
                        "description": "Which stream the marker appears on. Default: any."
                    },
                    "rationale": {
                        "type": "string",
                        "description": "Why this reproduces the reported issue."
                    }
                },
                "required": ["script", "red_marker", "rationale"]
            }
        }
    })
}

/// inline 派发的升级出口工具定义。安全不变量：这个工具**绝不能** requires_network——
/// 它在登记处外被单独派发，不经 definitions_for 过滤、也不经分发处联网闸。收编进登记处后才可解禁。
fn block_with_questions_def() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "block_with_questions",
            "description": "Escalate: stop this run and ask the user to re-align. Use when an acceptance criterion looks wrong, a key fact is missing, or you cannot converge after honest attempts. This STOPS the run; it does not continue guessing.",
            "parameters": { "type": "object",
                "properties": {
                    "blocked_reason": { "type": "string", "description": "Why you are stuck, one line." },
                    "questions": { "type": "array", "items": { "type": "string" }, "maxItems": 3, "description": "Up to 3 concrete questions for the user." },
                    "agent_diagnosis": { "type": "string", "description": "Which of goal/criteria/scope you suspect is wrong (your inference, not harness truth)." },
                    "failed_criteria": { "type": "array", "items": { "type": "string" } },
                    "evidence_refs": { "type": "array", "items": { "type": "string" } } },
                "required": ["blocked_reason", "questions"] }
        }
    })
}

/// inline 派发的工作便签工具定义。安全不变量：这个工具**绝不能** requires_network——
/// 它在登记处外被单独派发，不经 definitions_for 过滤、也不经分发处联网闸。收编进登记处后才可解禁。
fn update_working_state_def() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "update_working_state",
            "description": "Update your private working notes for this run: plan, known facts, unknowns, and next intent. This does not change the goal or acceptance criteria.",
            "parameters": { "type": "object",
                "properties": {
                    "plan": { "type": "string" },
                    "known": { "type": "array", "items": { "type": "string" } },
                    "unknown": { "type": "array", "items": { "type": "string" } },
                    "next_intent": { "type": "string" } }
            }
        }
    })
}

/// 原生服务端搜索 Active 时追加到内置 web_search description 的注明（防双份改保底·B2 候选①）。
pub const NATIVE_SEARCH_PREFERENCE_NOTE: &str = "Note: this provider may also run native server-side web search; when native search results are already in context, prefer them and only call this tool if you still lack the information.";

/// 组装「这一轮给模型的工具清单」：模型不会调工具→空；否则 = 登记处(按联网过滤) + inline 提议工具。
/// 原生服务端搜索 Active 时**不再剔除**内置 web_search（实证：部分 provider 对注入的原生搜索静默忽略，
/// 剔除会导致内置被剔、原生装死、两头落空）——改为在其 description 追加一句注明，
/// 告知模型「provider 原生搜索结果已在上下文时优先用那个」，防止双份调用又不至于两头落空。
pub fn build_offered_tools(
    registry: &ToolRegistry,
    capabilities: &crate::provider::ProviderCapabilities,
    network: crate::goal::NetworkPolicy,
    native_search_enabled: bool,
    disallowed: &BTreeSet<String>,
) -> Vec<serde_json::Value> {
    if !capabilities.supports_tool_calling {
        return Vec::new();
    }
    let mut tools = registry.definitions_for(network);
    use crate::provider::native_search::{native_search_state, NativeSearchState};
    if native_search_state(
        capabilities.server_side_search,
        network,
        native_search_enabled,
    ) == NativeSearchState::Active
    {
        for tool in tools.iter_mut() {
            if tool["function"]["name"] == "web_search" {
                if let Some(orig) = tool["function"]["description"].as_str() {
                    let updated = format!("{orig} {NATIVE_SEARCH_PREFERENCE_NOTE}");
                    tool["function"]["description"] = serde_json::Value::String(updated);
                }
            }
        }
    }
    tools.push(propose_scope_change_def());
    tools.push(propose_criterion_def());
    tools.push(register_issue_probe_def());
    tools.push(block_with_questions_def());
    tools.push(update_working_state_def());
    tools.retain(|t| !disallowed.contains(t["function"]["name"].as_str().unwrap_or_default()));
    tools
}
