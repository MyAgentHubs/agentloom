use crate::keychain::KeyStore;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ConnectionTestResult {
    pub ok: bool,
    pub category: Option<String>,
    pub raw_error: Option<String>,
}

pub fn classify_status(status: u16) -> &'static str {
    match status {
        401 | 403 => "auth",
        429 => "rate_limit",
        404 => "not_found",
        _ => "other",
    }
}

pub fn classify_search_status(status: u16) -> &'static str {
    match status {
        200..=299 => "ok",
        401 | 403 => "auth",
        402 | 429 => "rate_limit",
        _ => "network",
    }
}

/// key 优先级：表单 api_key(trim 非空) 优先 -> 否则 agent_id 读 keychain -> 都无 None。
pub fn resolve_key(
    store: &dyn KeyStore,
    agent_id: Option<&str>,
    api_key: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(k) = api_key {
        if !k.trim().is_empty() {
            return Ok(Some(k.trim().to_string()));
        }
    }
    if let Some(id) = agent_id {
        return store.get(id);
    }
    Ok(None)
}

pub fn parse_models(json: &serde_json::Value) -> Vec<String> {
    json.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn resolve_missing() -> ConnectionTestResult {
    ConnectionTestResult {
        ok: false,
        category: Some("missing_key".into()),
        raw_error: None,
    }
}

fn auth_header(
    req: reqwest::blocking::RequestBuilder,
    auth_mode: Option<&str>,
    key: &str,
) -> reqwest::blocking::RequestBuilder {
    if auth_mode == Some("x_api_key") {
        req.header("x-api-key", key)
    } else {
        req.header("Authorization", format!("Bearer {key}"))
    }
}

fn is_openai_protocol(protocol: Option<&str>) -> bool {
    protocol == Some("openai")
}

pub fn build_probe_request(
    protocol: Option<&str>,
    endpoint: &str,
    model: &str,
) -> (String, serde_json::Value) {
    let url = if is_openai_protocol(protocol) {
        format!("{}/chat/completions", endpoint.trim_end_matches('/'))
    } else {
        format!("{}/v1/messages", endpoint.trim_end_matches('/'))
    };
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "ping"}],
    });
    (url, body)
}

pub fn build_models_url(protocol: Option<&str>, endpoint: &str) -> String {
    if is_openai_protocol(protocol) {
        format!("{}/models", endpoint.trim_end_matches('/'))
    } else {
        endpoint.to_string()
    }
}

/// 真网络探测（blocking·调用方必须 spawn_blocking）。不用 reqwest json feature。
pub fn probe(
    endpoint: &str,
    protocol: Option<&str>,
    auth_mode: Option<&str>,
    model: &str,
    key: &str,
) -> ConnectionTestResult {
    let (url, body) = build_probe_request(protocol, endpoint, model);
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ConnectionTestResult {
                ok: false,
                category: Some("other".into()),
                raw_error: Some(e.to_string()),
            }
        }
    };
    let req = client.post(&url);
    let req = if is_openai_protocol(protocol) {
        req
    } else {
        req.header("anthropic-version", "2023-06-01")
    };
    let req = req
        .header("content-type", "application/json")
        .body(body.to_string());
    let req = auth_header(req, auth_mode, key);
    match req.send() {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                ConnectionTestResult {
                    ok: true,
                    category: None,
                    raw_error: None,
                }
            } else {
                let code = status.as_u16();
                let snippet: String = resp.text().unwrap_or_default().chars().take(500).collect();
                ConnectionTestResult {
                    ok: false,
                    category: Some(classify_status(code).into()),
                    raw_error: Some(format!("HTTP {code}: {snippet}")),
                }
            }
        }
        Err(e) => {
            let cat = if e.is_timeout() || e.is_connect() {
                "network"
            } else {
                "other"
            };
            ConnectionTestResult {
                ok: false,
                category: Some(cat.into()),
                raw_error: Some(e.to_string()),
            }
        }
    }
}

/// 真网络探测搜索后端（blocking·调用方 spawn_blocking）。打极小查询验 key。
pub fn probe_search(backend: &str, api_key: &str) -> ConnectionTestResult {
    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            return ConnectionTestResult {
                ok: false,
                category: Some("network".into()),
                raw_error: Some(e.to_string()),
            }
        }
    };
    let resp = if backend == "exa" {
        client
            .post("https://api.exa.ai/search")
            .header("x-api-key", api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .body(r#"{"query":"ping","numResults":1,"contents":{"highlights":true}}"#)
            .send()
    } else {
        client
            .get("https://api.search.brave.com/res/v1/web/search?q=ping&count=1")
            .header("X-Subscription-Token", api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
    };
    match resp {
        Ok(r) => {
            let cat = classify_search_status(r.status().as_u16());
            ConnectionTestResult {
                ok: cat == "ok",
                category: Some(cat.into()),
                raw_error: None,
            }
        }
        Err(e) => ConnectionTestResult {
            ok: false,
            category: Some("network".into()),
            raw_error: Some(e.to_string()),
        },
    }
}

/// 拉模型（blocking·OpenAI 风格 /models·调用方 spawn_blocking）。不用 reqwest json feature。
pub fn fetch_models_blocking(
    models_endpoint: &str,
    auth_mode: Option<&str>,
    key: &str,
) -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let req = auth_header(client.get(models_endpoint), auth_mode, key);
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let text = resp.text().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(parse_models(&json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keychain::{FakeKeyStore, KeyStore};

    #[test]
    fn classify_status_maps_categories() {
        assert_eq!(classify_status(401), "auth");
        assert_eq!(classify_status(403), "auth");
        assert_eq!(classify_status(429), "rate_limit");
        assert_eq!(classify_status(404), "not_found");
        assert_eq!(classify_status(500), "other");
    }

    #[test]
    fn classify_search_status_maps() {
        use super::classify_search_status;
        assert_eq!(classify_search_status(200), "ok");
        assert_eq!(classify_search_status(204), "ok");
        assert_eq!(classify_search_status(401), "auth");
        assert_eq!(classify_search_status(403), "auth");
        assert_eq!(classify_search_status(429), "rate_limit");
        assert_eq!(classify_search_status(500), "network");
        assert_eq!(classify_search_status(503), "network");
    }

    #[test]
    fn classify_search_status_402_is_rate_limit() {
        use super::classify_search_status;
        assert_eq!(classify_search_status(402), "rate_limit");
        assert_eq!(classify_search_status(200), "ok");
        assert_eq!(classify_search_status(401), "auth");
        assert_eq!(classify_search_status(429), "rate_limit");
        assert_eq!(classify_search_status(500), "network");
    }

    #[test]
    fn resolve_key_prefers_param_then_keychain_then_none() {
        let store = FakeKeyStore::default();
        store.set("a1", "stored").unwrap();
        assert_eq!(
            resolve_key(&store, Some("a1"), Some("typed")).unwrap(),
            Some("typed".into())
        );
        assert_eq!(
            resolve_key(&store, Some("a1"), Some("  ")).unwrap(),
            Some("stored".into())
        );
        assert_eq!(
            resolve_key(&store, Some("a1"), None).unwrap(),
            Some("stored".into())
        );
        assert_eq!(resolve_key(&store, Some("nope"), None).unwrap(), None);
        assert_eq!(resolve_key(&store, None, None).unwrap(), None);
    }

    #[test]
    fn parse_models_extracts_openai_data_ids() {
        let json = serde_json::json!({"data":[{"id":"m1"},{"id":"m2"}]});
        assert_eq!(
            parse_models(&json),
            vec!["m1".to_string(), "m2".to_string()]
        );
        assert!(parse_models(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn build_probe_request_defaults_to_existing_anthropic_shape() {
        let (url, body) = build_probe_request(None, "https://anthropic.example/", "claude-test");

        assert_eq!(url, "https://anthropic.example/v1/messages");
        assert_eq!(
            body,
            serde_json::json!({
                "model": "claude-test",
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}],
            })
        );
    }

    #[test]
    fn build_probe_request_openai_appends_chat_completions_to_endpoint_root() {
        assert_eq!(
            build_probe_request(Some("openai"), "https://api.openai.test", "gpt-test").0,
            "https://api.openai.test/chat/completions"
        );
        assert_eq!(
            build_probe_request(Some("openai"), "https://api.openai.test/v1", "gpt-test").0,
            "https://api.openai.test/v1/chat/completions"
        );
        assert_eq!(
            build_probe_request(
                Some("openai"),
                "https://api.deepseek.com/v1",
                "deepseek-test"
            )
            .0,
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            build_probe_request(Some("openai"), "https://api.moonshot.cn/v1", "kimi-test").0,
            "https://api.moonshot.cn/v1/chat/completions"
        );
        assert_eq!(
            build_probe_request(Some("openai"), "https://api.openai.test/", "gpt-test").0,
            "https://api.openai.test/chat/completions"
        );
        assert_eq!(
            build_probe_request(
                Some("openai"),
                "https://open.bigmodel.cn/api/paas/v4",
                "glm-test"
            )
            .0,
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
    }

    #[test]
    fn build_probe_request_openai_body_matches_chat_completions_shape() {
        let (_url, body) =
            build_probe_request(Some("openai"), "https://api.openai.test", "gpt-test");

        assert_eq!(
            body,
            serde_json::json!({
                "model": "gpt-test",
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 1,
            })
        );
        assert!(body.get("anthropic-version").is_none());
        assert!(body.get("anthropic_version").is_none());
    }

    /// 对照 `app/src/components/settings/agentFormHelpers.ts` 里 `harness-*` 预设
    /// （access:"harness"）的 `endpoint` + `modelsEndpoint` 字段：这些预设的 `endpoint`
    /// 就是纯 OpenAI 风格 base（无 anthropic 代理垫层），
    /// `build_models_url(Some("openai"), endpoint)` 逐字节等于该预设手写的
    /// `modelsEndpoint`——证明 `fetch_agent_models` 一旦未来前端改传
    /// `(protocol="openai", endpoint=base)`，harness 这 7 个接入点可以安全退掉手拼
    /// `modelsEndpoint` 常量。
    #[test]
    fn build_models_url_matches_harness_preset_endpoints() {
        let cases = [
            // harness-deepseek · default
            (
                "https://api.deepseek.com/v1",
                "https://api.deepseek.com/v1/models",
            ),
            // harness-glm · cn
            (
                "https://open.bigmodel.cn/api/paas/v4",
                "https://open.bigmodel.cn/api/paas/v4/models",
            ),
            // harness-glm · intl
            (
                "https://api.z.ai/api/paas/v4",
                "https://api.z.ai/api/paas/v4/models",
            ),
            // harness-glm · cn-coding
            (
                "https://open.bigmodel.cn/api/coding/paas/v4",
                "https://open.bigmodel.cn/api/coding/paas/v4/models",
            ),
            // harness-glm · intl-coding
            (
                "https://api.z.ai/api/coding/paas/v4",
                "https://api.z.ai/api/coding/paas/v4/models",
            ),
            // harness-kimi · cn
            (
                "https://api.moonshot.cn/v1",
                "https://api.moonshot.cn/v1/models",
            ),
            // harness-kimi · intl
            (
                "https://api.moonshot.ai/v1",
                "https://api.moonshot.ai/v1/models",
            ),
        ];
        for (endpoint, expected_models_endpoint) in cases {
            assert_eq!(
                build_models_url(Some("openai"), endpoint),
                expected_models_endpoint,
                "harness endpoint {endpoint} 推导应等于前端手写 modelsEndpoint"
            );
        }
    }

    /// 对照 `agentFormHelpers.ts` 里 `access:"borrow"` 预设（deepseek/kimi/zhipu）：
    /// 这些预设的 `endpoint` 字段是走 Claude Code 借壳的 anthropic 代理路径
    /// （如 `.../anthropic`），跟真正拉模型用的 OpenAI 风格 base（如去掉
    /// `/anthropic` 后的域名根）不是同一个值——`build_models_url` 若拿 `endpoint`
    /// 字段去推导，结果对不上前端手写的 `modelsEndpoint`。这里显式断言「对不上」，
    /// 记录该差异：borrow 预设暂不能只靠 `(protocol, endpoint)` 二元组接线，
    /// 需要额外的「models 专用 base」字段（不在本任务范围内，前端 accessPoints 也未建模
    /// 这个字段），因此 borrow 预设的 `modelsEndpoint` 常量继续保持手拼、不接线。
    #[test]
    fn build_models_url_does_not_match_borrow_preset_chat_endpoint() {
        let cases = [
            // deepseek · default：endpoint 走 /anthropic 代理，真实 models base 是纯域名根
            (
                "https://api.deepseek.com/anthropic",
                "https://api.deepseek.com/models",
            ),
            // kimi · cn
            (
                "https://api.moonshot.cn/anthropic",
                "https://api.moonshot.cn/v1/models",
            ),
            // kimi · intl
            (
                "https://api.moonshot.ai/anthropic",
                "https://api.moonshot.ai/v1/models",
            ),
            // zhipu · cn
            (
                "https://open.bigmodel.cn/api/anthropic",
                "https://open.bigmodel.cn/api/paas/v4/models",
            ),
            // zhipu · intl
            (
                "https://api.z.ai/api/anthropic",
                "https://api.z.ai/api/paas/v4/models",
            ),
        ];
        for (chat_endpoint, real_models_endpoint) in cases {
            assert_ne!(
                build_models_url(Some("openai"), chat_endpoint),
                real_models_endpoint,
                "borrow chat_endpoint {chat_endpoint} 不应巧合等于真实 modelsEndpoint"
            );
        }
    }

    #[test]
    fn build_models_url_openai_appends_models_to_endpoint_root() {
        assert_eq!(
            build_models_url(Some("openai"), "https://api.openai.test"),
            "https://api.openai.test/models"
        );
        assert_eq!(
            build_models_url(Some("openai"), "https://api.openai.test/v1"),
            "https://api.openai.test/v1/models"
        );
        assert_eq!(
            build_models_url(Some("openai"), "https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1/models"
        );
        assert_eq!(
            build_models_url(Some("openai"), "https://api.moonshot.cn/v1"),
            "https://api.moonshot.cn/v1/models"
        );
        assert_eq!(
            build_models_url(Some("openai"), "https://api.openai.test/"),
            "https://api.openai.test/models"
        );
        assert_eq!(
            build_models_url(Some("openai"), "https://open.bigmodel.cn/api/paas/v4"),
            "https://open.bigmodel.cn/api/paas/v4/models"
        );
    }
}
