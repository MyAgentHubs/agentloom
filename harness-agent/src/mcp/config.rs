use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, Result};

/// Configuration for a single MCP (Model Context Protocol) server.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
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
    /// Custom HTTP headers sent with every request to a Streamable HTTP
    /// server (meaningless for stdio servers). Values may reference
    /// `${ENV_NAME}` — see [`expand_env_placeholders`] — to be resolved from
    /// the environment at connect time, so secrets need not be written into
    /// config.json. `None`/absent for a config written before this field
    /// existed, so old `config.json` files keep loading unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
}

/// Mask a `BTreeMap<String, String>`'s values for `Debug` output, keeping keys
/// visible. Used for `env` and `headers`, which may carry secrets (API keys,
/// tokens, `${ENV_NAME}` placeholders that resolve to secrets).
fn masked_map_debug(
    f: &mut std::fmt::Formatter<'_>,
    map: &BTreeMap<String, String>,
) -> std::fmt::Result {
    f.debug_map()
        .entries(map.keys().map(|k| (k, "***")))
        .finish()
}

impl std::fmt::Debug for McpServerConfig {
    /// Hand-written so `env` and `headers` values never land in a `{:?}` log
    /// line, panic message, or event payload — only their keys do. A derived
    /// `Debug` would echo secrets (API keys, bearer tokens) in plain text.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("McpServerConfig");
        d.field("name", &self.name)
            .field("command", &self.command)
            .field("url", &self.url)
            .field("args", &self.args);
        struct MaskedEnv<'a>(&'a BTreeMap<String, String>);
        impl std::fmt::Debug for MaskedEnv<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                masked_map_debug(f, self.0)
            }
        }
        d.field("env", &MaskedEnv(&self.env));
        d.field("trusted", &self.trusted);
        match self.headers.as_ref() {
            Some(headers) => {
                d.field("headers", &Some(MaskedEnv(headers)));
            }
            None => {
                d.field("headers", &Option::<()>::None);
            }
        }
        d.finish()
    }
}

/// Expand `${ENV_NAME}` placeholders in `value` (the value of HTTP header
/// `header_name`) from the process environment. Used to resolve MCP HTTP
/// header values without writing secrets into config.json. A malformed
/// placeholder (unterminated `${`) or a reference to an unset environment
/// variable is an error rather than silently passing the literal text through
/// — a header meant to carry a secret must never end up sending the
/// placeholder string itself.
///
/// Error messages name the header only — never the value being expanded —
/// since that value may already contain a partially-resolved secret and gets
/// surfaced to the user and logged into `events.jsonl` via `mcp.server.failed`.
pub(crate) fn expand_env_placeholders(header_name: &str, value: &str) -> Result<String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or_else(|| {
            HarnessError::Runtime(format!(
                "mcp header `{header_name}` value has an unterminated `${{` placeholder"
            ))
        })?;
        let var_name = &after[..end];
        let resolved = std::env::var(var_name).map_err(|_| {
            HarnessError::Runtime(format!(
                "mcp header `{header_name}` value references unset environment variable `{var_name}`"
            ))
        })?;
        out.push_str(&resolved);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
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
        assert_eq!(cfg.headers, None);
    }

    #[test]
    fn http_config_with_headers_parses() {
        let json = r#"{
            "name": "http",
            "url": "http://127.0.0.1:9000/mcp",
            "trusted": false,
            "headers": {"Authorization": "Bearer ${MY_TOKEN}", "X-Api-Version": "1"}
        }"#;
        let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
        let headers = cfg.headers.expect("headers must parse");
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer ${MY_TOKEN}")
        );
        assert_eq!(headers.get("X-Api-Version").map(String::as_str), Some("1"));
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
            headers: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(
            !json.contains("url"),
            "stdio config must not serialize a url key: {json}"
        );
        assert!(
            !json.contains("headers"),
            "config with no headers must not serialize a headers key: {json}"
        );
        let back: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn http_config_with_headers_roundtrips() {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "Bearer ${TOKEN}".to_string());
        let cfg = McpServerConfig {
            name: "http".into(),
            command: String::new(),
            url: Some("http://127.0.0.1:9000/mcp".into()),
            args: vec![],
            env: BTreeMap::new(),
            trusted: false,
            headers: Some(headers),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("headers"));
        let back: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    // ─── expand_env_placeholders ───

    #[test]
    fn expand_env_placeholders_passes_through_plain_text() {
        assert_eq!(
            expand_env_placeholders("Authorization", "plain-value").unwrap(),
            "plain-value"
        );
    }

    #[test]
    fn expand_env_placeholders_substitutes_single_var() {
        std::env::set_var("MYAGENT_TEST_HEADER_TOKEN", "secret123");
        let result =
            expand_env_placeholders("Authorization", "Bearer ${MYAGENT_TEST_HEADER_TOKEN}")
                .unwrap();
        std::env::remove_var("MYAGENT_TEST_HEADER_TOKEN");
        assert_eq!(result, "Bearer secret123");
    }

    #[test]
    fn expand_env_placeholders_substitutes_multiple_vars() {
        std::env::set_var("MYAGENT_TEST_HEADER_A", "aaa");
        std::env::set_var("MYAGENT_TEST_HEADER_B", "bbb");
        let result = expand_env_placeholders(
            "X-Combo",
            "${MYAGENT_TEST_HEADER_A}-${MYAGENT_TEST_HEADER_B}",
        )
        .unwrap();
        std::env::remove_var("MYAGENT_TEST_HEADER_A");
        std::env::remove_var("MYAGENT_TEST_HEADER_B");
        assert_eq!(result, "aaa-bbb");
    }

    #[test]
    fn expand_env_placeholders_errors_on_unset_var() {
        std::env::remove_var("MYAGENT_TEST_HEADER_MISSING");
        let err = expand_env_placeholders("Authorization", "Bearer ${MYAGENT_TEST_HEADER_MISSING}")
            .unwrap_err();
        assert!(err.to_string().contains("MYAGENT_TEST_HEADER_MISSING"));
        assert!(err.to_string().contains("Authorization"));
    }

    #[test]
    fn expand_env_placeholders_errors_on_unterminated_placeholder() {
        let err = expand_env_placeholders("Authorization", "Bearer ${OOPS").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unterminated"));
        assert!(msg.contains("Authorization"));
        // The raw header value template (and any secret text mixed into it)
        // must never be echoed back — only the header name.
        assert!(
            !msg.contains("Bearer ${OOPS"),
            "error must not echo the header value template: {msg}"
        );
    }

    // ─── Debug masks env/headers values ───

    #[test]
    fn debug_masks_env_values_but_keeps_keys() {
        let mut env = BTreeMap::new();
        env.insert("API_KEY".to_string(), "super-secret-value".to_string());
        let cfg = McpServerConfig {
            name: "srv".into(),
            command: "node".into(),
            url: None,
            args: vec![],
            env,
            trusted: false,
            headers: None,
        };
        let debug = format!("{cfg:?}");
        assert!(debug.contains("API_KEY"), "key must be visible: {debug}");
        assert!(
            !debug.contains("super-secret-value"),
            "env value must never appear in Debug output: {debug}"
        );
        assert!(debug.contains("***"));
    }

    #[test]
    fn debug_masks_header_values_but_keeps_keys() {
        let mut headers = BTreeMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer super-secret-value".to_string(),
        );
        let cfg = McpServerConfig {
            name: "srv".into(),
            command: String::new(),
            url: Some("http://127.0.0.1:9000/mcp".into()),
            args: vec![],
            env: BTreeMap::new(),
            trusted: false,
            headers: Some(headers),
        };
        let debug = format!("{cfg:?}");
        assert!(
            debug.contains("Authorization"),
            "key must be visible: {debug}"
        );
        assert!(
            !debug.contains("super-secret-value"),
            "header value must never appear in Debug output: {debug}"
        );
        assert!(debug.contains("***"));
    }

    #[test]
    fn debug_shows_none_headers_as_none() {
        let cfg = McpServerConfig {
            name: "srv".into(),
            command: "node".into(),
            url: None,
            args: vec![],
            env: BTreeMap::new(),
            trusted: false,
            headers: None,
        };
        let debug = format!("{cfg:?}");
        assert!(debug.contains("headers: None"), "got: {debug}");
    }
}
