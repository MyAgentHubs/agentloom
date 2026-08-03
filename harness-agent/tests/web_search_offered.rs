//! orchestrator 层：可用性过滤决定 web_search 是否进给模型的工具清单。
use myagent::orchestrator::{
    build_default_registry, build_offered_tools, NATIVE_SEARCH_PREFERENCE_NOTE,
};
use myagent::provider::ProviderCapabilities;

fn caps(tool_calling: bool) -> ProviderCapabilities {
    ProviderCapabilities {
        provider_id: "mock".into(),
        model_id: "m".into(),
        supports_streaming: false,
        supports_reasoning_deltas: false,
        supports_tool_calling: tool_calling,
        supports_images: false,
        supports_computer_use: false,
        supports_shell_tool: true,
        max_context_tokens: None,
        output_token_limit: None,
        server_side_search: false,
    }
}

fn caps_with_native(tool_calling: bool, server_side_search: bool) -> ProviderCapabilities {
    ProviderCapabilities {
        server_side_search,
        ..caps(tool_calling)
    }
}

fn names(tools: &[serde_json::Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(String::from))
        .collect()
}

fn description_of<'a>(tools: &'a [serde_json::Value], name: &str) -> Option<&'a str> {
    tools
        .iter()
        .find(|t| t["function"]["name"] == name)
        .and_then(|t| t["function"]["description"].as_str())
}

#[test]
fn build_offered_tools_excludes_disallowed_including_propose() {
    let reg = build_default_registry(&myagent::config::SearchChoice::Ddg, false);
    let mut disallowed = std::collections::BTreeSet::new();
    disallowed.insert("propose_criterion".to_string());
    disallowed.insert("fs_read".to_string());

    let tools = build_offered_tools(
        &reg,
        &caps(true),
        myagent::goal::NetworkPolicy::Off,
        false,
        &disallowed,
    );
    let names = names(&tools);

    assert!(!names.iter().any(|n| n == "propose_criterion"));
    assert!(!names.iter().any(|n| n == "fs_read"));
    assert!(names.iter().any(|n| n == "propose_scope_change"));
}

#[test]
fn web_search_offered_when_network_on() {
    let reg = build_default_registry(&myagent::config::SearchChoice::Ddg, false);
    let tools = build_offered_tools(
        &reg,
        &caps(true),
        myagent::goal::NetworkPolicy::On,
        true,
        &Default::default(),
    );
    assert!(names(&tools).iter().any(|n| n == "web_search"));
    assert!(!description_of(&tools, "web_search")
        .unwrap()
        .contains(NATIVE_SEARCH_PREFERENCE_NOTE));
}

#[test]
fn web_search_absent_when_network_off() {
    let reg = build_default_registry(&myagent::config::SearchChoice::Ddg, false);
    let tools = build_offered_tools(
        &reg,
        &caps(true),
        myagent::goal::NetworkPolicy::Off,
        true,
        &Default::default(),
    );
    assert!(!names(&tools).iter().any(|n| n == "web_search"));
    assert!(names(&tools).iter().any(|n| n == "fs_read"));
}

#[test]
fn empty_tool_list_when_model_cannot_call_tools() {
    let reg = build_default_registry(&myagent::config::SearchChoice::Ddg, false);
    let tools = build_offered_tools(
        &reg,
        &caps(false),
        myagent::goal::NetworkPolicy::On,
        true,
        &Default::default(),
    );
    assert!(tools.is_empty());
}

#[test]
fn native_active_keeps_builtin_web_search_with_preference_note() {
    let reg = build_default_registry(&myagent::config::SearchChoice::Ddg, false);
    let tools = build_offered_tools(
        &reg,
        &caps_with_native(true, true),
        myagent::goal::NetworkPolicy::On,
        true,
        &Default::default(),
    );
    let names = names(&tools);
    assert!(names.iter().any(|n| n == "web_search"));
    let description = description_of(&tools, "web_search").unwrap();
    assert!(description.ends_with(NATIVE_SEARCH_PREFERENCE_NOTE));
    assert!(description.len() > NATIVE_SEARCH_PREFERENCE_NOTE.len());
    assert!(names.iter().any(|n| n == "propose_scope_change"));
    assert!(names.iter().any(|n| n == "propose_criterion"));
    // 阴性断言：注明句只落在 web_search 上，其他所有工具的 description 都不含它。
    for tool in &tools {
        if tool["function"]["name"] != "web_search" {
            assert!(
                !tool["function"]["description"]
                    .as_str()
                    .unwrap_or("")
                    .contains(NATIVE_SEARCH_PREFERENCE_NOTE),
                "preference note leaked into tool {}",
                tool["function"]["name"]
            );
        }
    }
}

#[test]
fn native_disabled_by_user_keeps_builtin_web_search() {
    let reg = build_default_registry(&myagent::config::SearchChoice::Ddg, false);
    let tools = build_offered_tools(
        &reg,
        &caps_with_native(true, true),
        myagent::goal::NetworkPolicy::On,
        false,
        &Default::default(),
    );
    assert!(names(&tools).iter().any(|n| n == "web_search"));
    assert!(!description_of(&tools, "web_search")
        .unwrap()
        .contains(NATIVE_SEARCH_PREFERENCE_NOTE));
}

#[test]
fn generic_provider_keeps_builtin_web_search() {
    let reg = build_default_registry(&myagent::config::SearchChoice::Ddg, false);
    let tools = build_offered_tools(
        &reg,
        &caps_with_native(true, false),
        myagent::goal::NetworkPolicy::On,
        true,
        &Default::default(),
    );
    assert!(names(&tools).iter().any(|n| n == "web_search"));
    assert!(!description_of(&tools, "web_search")
        .unwrap()
        .contains(NATIVE_SEARCH_PREFERENCE_NOTE));
}
