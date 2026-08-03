use std::collections::BTreeSet;

use serde_json::json;

/// 从 shell_exec 的 arguments JSON 抽 command 字段、跑逃逸预扫。
pub(crate) fn shell_exec_escape_rule(arguments: &str) -> Option<&'static str> {
    let cmd = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(String::from))?;
    crate::exec::controlled::escape_scan(&cmd)
}

pub(crate) fn tool_disallowed(name: &str, disallowed: &BTreeSet<String>) -> bool {
    disallowed.contains(name)
}

pub(crate) fn disallowed_tool_rejection(name: &str) -> String {
    json!({
        "error": format!(
            "tool '{name}' is disabled for this run; 该工具本趟禁用·照着固定验收标准实现或升级"
        )
    })
    .to_string()
}

/// 每轮可执行的「要联网」工具调用次数上限。本轮为常量·后续可提为配置（spec §4）。
pub const MAX_NETWORK_TOOL_CALLS_PER_TURN: usize = 5;

#[derive(Debug, PartialEq)]
pub enum NetworkGate {
    Execute,
    RefuseNetworkOff,
    RefuseCap,
}

/// 联网工具执行前的闸判断（纯函数·穷尽单测）。
/// prior_calls = 本轮此前已执行的联网工具次数；cap = 上限。
pub fn network_tool_gate(
    requires_network: bool,
    network: crate::goal::NetworkPolicy,
    prior_calls: usize,
    cap: usize,
) -> NetworkGate {
    if !requires_network {
        return NetworkGate::Execute;
    }
    if network == crate::goal::NetworkPolicy::Off {
        return NetworkGate::RefuseNetworkOff;
    }
    if prior_calls >= cap {
        return NetworkGate::RefuseCap;
    }
    NetworkGate::Execute
}
