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
            "description": "Adjust the task boundary. For kind=scope WITH a `paths` list, your editable file scope is widened and the run CONTINUES. For objective/constraint (or scope without paths): if a human/decision channel is available the run STOPS for the user to decide; otherwise the change is rejected with guidance and the run CONTINUES under the existing contract. `paths` must be workspace-relative (no absolute paths, no `..`); this only widens the fs_write/fs_edit file scope and never permits shell_exec writes outside the workspace — if the tool result is `scope_extend_rejected`, do not retry the same rejected path.",
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
    build_offered_tools_with_roots(
        registry,
        capabilities,
        network,
        native_search_enabled,
        disallowed,
        &[],
    )
}

/// 同 `build_offered_tools`，额外把 `extra_read_roots`（CLI `--read-root`）追加进
/// fs_read / shell_exec 的工具描述，让模型不用先撞一次拒才知道这些目录可读。
/// 空切片时与 `build_offered_tools` 完全等价（不追加任何文案）。
pub fn build_offered_tools_with_roots(
    registry: &ToolRegistry,
    capabilities: &crate::provider::ProviderCapabilities,
    network: crate::goal::NetworkPolicy,
    native_search_enabled: bool,
    disallowed: &BTreeSet<String>,
    extra_read_roots: &[std::path::PathBuf],
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
    if !extra_read_roots.is_empty() {
        // `extra_read_roots` 可能同时含同一目录的 lexical + canonical 两份拼写（macOS
        // `/var` vs `/private/var` symlink 场景，见 `fs_scope::resolve_read_roots`）；
        // 给模型的 note 只列 canonical 那一份，按出现顺序去重，别把同一目录报两遍噪音。
        let mut seen = std::collections::BTreeSet::new();
        let mut canonical_paths = Vec::new();
        for root in extra_read_roots {
            let canonical = crate::tools::fs_read::canonicalize_lenient(root);
            let text = canonical.to_string_lossy().into_owned();
            if seen.insert(text.clone()) {
                canonical_paths.push(text);
            }
        }
        let list = canonical_paths.join(", ");
        let note = format!(
            "Note: these extra directories outside the workspace are also readable (read-only): {list}."
        );
        for tool in tools.iter_mut() {
            if matches!(
                tool["function"]["name"].as_str(),
                Some("fs_read") | Some("shell_exec") | Some("ls")
            ) {
                if let Some(orig) = tool["function"]["description"].as_str() {
                    let updated = format!("{orig} {note}");
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

#[cfg(test)]
mod extra_read_root_note_tests {
    use super::*;
    use crate::provider::ProviderCapabilities;
    use crate::tools::fs_read::FsReadTool;
    use crate::tools::ls::LsTool;
    use crate::tools::shell_exec::ShellExecTool;
    use crate::tools::ToolRegistry;

    fn caps() -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "p".into(),
            model_id: "m".into(),
            supports_streaming: false,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: None,
            output_token_limit: None,
            server_side_search: false,
        }
    }

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FsReadTool));
        registry.register(Box::new(LsTool));
        registry.register(Box::new(ShellExecTool));
        registry
    }

    // P3-2：`ls` 也走了 extra roots（`resolve_for_read(ctx.extra_read_roots)`），
    // 描述应该和 `fs_read` / `shell_exec` 一样带上 "(read-only)" note。
    #[test]
    fn ls_tool_description_gets_extra_root_note_like_fs_read_and_shell_exec() {
        let root = tempfile::tempdir().unwrap();
        let extra = root.path().join("extra");
        std::fs::create_dir(&extra).unwrap();
        let roots = crate::fs_scope::resolve_read_roots(std::slice::from_ref(&extra)).unwrap();

        let tools = build_offered_tools_with_roots(
            &registry(),
            &caps(),
            crate::goal::NetworkPolicy::On,
            false,
            &BTreeSet::new(),
            &roots,
        );

        for name in ["fs_read", "ls", "shell_exec"] {
            let tool = tools
                .iter()
                .find(|t| t["function"]["name"] == name)
                .unwrap_or_else(|| panic!("missing tool {name}"));
            let description = tool["function"]["description"].as_str().unwrap();
            assert!(
                description.contains("also readable (read-only)"),
                "{name} description missing extra-root note: {description}"
            );
        }

        // P3-4：note 里点名的每条路径都必须真能被 `resolve_for_read` 放行——不能
        // 光说不做，模型照着 note 里的路径读却撞拒。
        let workspace = tempfile::tempdir().unwrap();
        for root in &roots {
            let canonical = crate::tools::fs_read::canonicalize_lenient(root);
            assert!(
                crate::tools::fs_read::resolve_for_read(
                    workspace.path(),
                    &canonical.to_string_lossy(),
                    crate::fs_scope::FsReadScope::Workspace,
                    &roots,
                )
                .is_ok(),
                "note-eligible root {canonical:?} must be readable via resolve_for_read"
            );
        }
    }

    // P3-2：`resolve_read_roots` 可能同时留存同一目录的 lexical + canonical 两份拼写
    // （macOS `/var` vs `/private/var`）；note 文案里不应该把同一个目录列两遍。这里直接
    // 构造一份「同一真实目录、两个拼写」的 roots，不依赖 tempdir 是否恰好落在符号链接上。
    #[test]
    fn extra_root_note_lists_each_directory_once_even_with_dual_spellings() {
        let root = tempfile::tempdir().unwrap();
        let extra = root.path().join("extra");
        std::fs::create_dir(&extra).unwrap();
        let canonical = extra.canonicalize().unwrap();
        let lexical_spelling = std::path::PathBuf::from(format!(
            "{}/./extra",
            root.path().to_string_lossy().trim_end_matches('/')
        ));
        assert_ne!(
            canonical, lexical_spelling,
            "test setup requires two distinct spellings of the same directory"
        );
        let roots = vec![lexical_spelling.clone(), canonical.clone()];

        let tools = build_offered_tools_with_roots(
            &registry(),
            &caps(),
            crate::goal::NetworkPolicy::On,
            false,
            &BTreeSet::new(),
            &roots,
        );
        let fs_read_desc = tools
            .iter()
            .find(|t| t["function"]["name"] == "fs_read")
            .unwrap()["function"]["description"]
            .as_str()
            .unwrap()
            .to_string();
        let occurrences = fs_read_desc
            .matches(canonical.to_string_lossy().as_ref())
            .count();
        assert_eq!(
            occurrences, 1,
            "expected canonical dir to be listed exactly once, got {occurrences} in {fs_read_desc:?}"
        );
        assert!(
            !fs_read_desc.contains(lexical_spelling.to_string_lossy().as_ref()),
            "expected the non-canonical spelling to be dropped from the note, got {fs_read_desc:?}"
        );

        // P3-4：note 里实际点名的那份 canonical 路径必须真能被 `resolve_for_read` 放行。
        let workspace = tempfile::tempdir().unwrap();
        assert!(
            crate::tools::fs_read::resolve_for_read(
                workspace.path(),
                &canonical.to_string_lossy(),
                crate::fs_scope::FsReadScope::Workspace,
                &roots,
            )
            .is_ok(),
            "note-listed canonical dir {canonical:?} must be readable via resolve_for_read"
        );
    }
}
