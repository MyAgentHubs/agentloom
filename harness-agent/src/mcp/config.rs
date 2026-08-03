use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Configuration for a single MCP (Model Context Protocol) server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerConfig {
    /// Server name (also the tool-name prefix `mcp__<name>__<tool>`).
    pub name: String,
    /// The command to launch the server over stdio (executable path or name).
    /// Mutually exclusive with `url`; empty when `url` is set.
    #[serde(default)]
    pub command: String,
    /// The Streamable HTTP endpoint of the server. When set, the server is
    /// reached over HTTP instead of by spawning `command`. Mutually exclusive
    /// with `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Command-line arguments passed to the server process.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables injected into the server process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Whether this server is trusted: trusted servers skip the per-call
    /// approval prompt, but `--permission deny` still rejects them.
    /// Defaults to false (fail-closed): a config missing this field is untrusted.
    #[serde(default)]
    pub trusted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_stdio_config_without_url_still_parses() {
        // A config.json written before the `url` field existed must keep loading.
        let json = r#"{
            "name": "legacy",
            "command": "node",
            "args": ["server.js"],
            "env": {"K": "v"},
            "trusted": true
        }"#;
        let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.command, "node");
        assert_eq!(cfg.url, None);
        assert_eq!(cfg.args, vec!["server.js".to_string()]);
        assert!(cfg.trusted);
    }

    #[test]
    fn http_config_with_url_and_no_command_parses() {
        let json = r#"{
            "name": "http",
            "url": "http://127.0.0.1:9000/mcp",
            "trusted": false
        }"#;
        let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.url.as_deref(), Some("http://127.0.0.1:9000/mcp"));
        assert_eq!(cfg.command, "");
        assert!(cfg.args.is_empty());
    }

    #[test]
    fn stdio_config_roundtrips_without_emitting_url() {
        let cfg = McpServerConfig {
            name: "s".into(),
            command: "node".into(),
            url: None,
            args: vec![],
            env: BTreeMap::new(),
            trusted: false,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(
            !json.contains("url"),
            "stdio config must not serialize a url key: {json}"
        );
        let back: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }
}
