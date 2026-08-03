//! Exa /search 后端：带 key、POST JSON。parse/cap 纯函数（录制样例测·不联网）；
//! search 经统一出口 client·流式累计响应体·超 MAX_RESPONSE_BYTES 即停。Exa 不 retry·由上层 FallbackBackend 兜。

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;

use super::{
    classify_exa_status, egress_client, SearchBackend, SearchError, SearchResult,
    MAX_RESPONSE_BYTES, SEARCH_TIMEOUT_SECS,
};

const DEFAULT_BASE_URL: &str = "https://api.exa.ai";
const MAX_EXA_QUERY_CHARS: usize = 1000;

pub struct ExaBackend {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl ExaBackend {
    pub fn new(api_key: String) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            api_key,
            client: egress_client(),
        }
    }

    pub fn with_base_url(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            client: egress_client(),
        }
    }
}

/// 纯函数：query 保守裁到 1000 字符（非 Exa 官方上限·避免异常长 query）。
pub fn cap_exa_query(q: &str) -> String {
    q.chars().take(MAX_EXA_QUERY_CHARS).collect()
}

#[derive(Deserialize)]
struct ExaResp {
    #[serde(default)]
    results: Vec<ExaItem>,
}

#[derive(Deserialize)]
struct ExaItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    highlights: Vec<String>,
}

/// 纯函数：Exa JSON → 归一结果（至多 count 条）。snippet 取 highlights 首条·无则空·只滤空 title/url。
pub fn parse_exa_json(body: &str, count: usize) -> Vec<SearchResult> {
    let parsed: ExaResp = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    parsed
        .results
        .into_iter()
        .filter(|it| !it.title.is_empty() && !it.url.is_empty())
        .take(count)
        .map(|it| SearchResult {
            title: it.title,
            url: it.url,
            snippet: it.highlights.into_iter().next().unwrap_or_default(),
        })
        .collect()
}

#[async_trait]
impl SearchBackend for ExaBackend {
    async fn search(
        &self,
        query: &str,
        count: usize,
    ) -> std::result::Result<Vec<SearchResult>, SearchError> {
        let q = cap_exa_query(query);
        let url = format!("{}/search", self.base_url.trim_end_matches('/'));
        let body = serde_json::to_string(&serde_json::json!({
            "query": q,
            "numResults": count,
            "contents": { "highlights": true }
        }))
        .map_err(|e| SearchError::Backend(e.to_string()))?;
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body)
            .timeout(std::time::Duration::from_secs(SEARCH_TIMEOUT_SECS))
            .send()
            .await
            .map_err(|e| SearchError::Backend(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(classify_exa_status(status.as_u16()));
        }
        if let Some(len) = resp.content_length() {
            if len as usize > MAX_RESPONSE_BYTES {
                return Err(SearchError::Backend("response too large".into()));
            }
        }
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| SearchError::Backend(e.to_string()))?;
            if buf.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(SearchError::Backend("response too large".into()));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(parse_exa_json(&String::from_utf8_lossy(&buf), count))
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::search::{SearchBackend, SearchError, SearchResult, MAX_RESPONSE_BYTES};

    use super::*;

    const SAMPLE: &str = r#"{
      "results": [
        {"title": "Rust 官网", "url": "https://example.com/rust", "highlights": ["Rust 是系统语言。"]},
        {"title": "Async", "url": "https://example.org/async", "highlights": ["异步入门。"]},
        {"title": "无高亮", "url": "https://example.net/x"},
        {"title": "", "url": "https://example.net/skip", "highlights": ["missing title"]}
      ]
    }"#;

    #[test]
    fn parse_exa_json_maps_and_filters() {
        let r = parse_exa_json(SAMPLE, 10);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].title, "Rust 官网");
        assert_eq!(r[0].url, "https://example.com/rust");
        assert_eq!(r[0].snippet, "Rust 是系统语言。");
        assert_eq!(r[2].snippet, "");
    }

    #[test]
    fn parse_exa_json_respects_count() {
        assert_eq!(parse_exa_json(SAMPLE, 1).len(), 1);
    }

    #[test]
    fn cap_exa_query_caps_to_1000_chars() {
        let long = "x".repeat(2000);
        assert_eq!(cap_exa_query(&long).chars().count(), 1000);
    }

    #[tokio::test]
    async fn exa_200_sends_x_api_key_without_leaking_credentials() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
            .mount(&server)
            .await;

        let backend = ExaBackend::with_base_url(server.uri(), "k".into());
        let results = backend.search("rust", 2).await.unwrap();
        assert_eq!(results.len(), 2);

        let reqs = server.received_requests().await.unwrap();
        let r: &Request = &reqs[0];
        assert_eq!(r.headers.get("x-api-key").unwrap(), "k");
        assert!(r.headers.get("authorization").is_none());
        assert!(r.headers.get("cookie").is_none());
        assert_eq!(r.headers.get("content-type").unwrap(), "application/json");

        let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(body["query"], "rust");
        assert_eq!(body["numResults"], 2);
        assert_eq!(body["contents"]["highlights"], true);
    }

    #[tokio::test]
    async fn exa_401_maps_to_auth() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let backend = ExaBackend::with_base_url(server.uri(), "k".into());
        assert!(matches!(
            backend.search("rust", 5).await.unwrap_err(),
            SearchError::Auth
        ));
    }

    #[tokio::test]
    async fn exa_402_maps_to_quota_exhausted() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(402))
            .mount(&server)
            .await;

        let backend = ExaBackend::with_base_url(server.uri(), "k".into());
        assert!(matches!(
            backend.search("rust", 5).await.unwrap_err(),
            SearchError::QuotaExhausted
        ));
    }

    #[tokio::test]
    async fn exa_declared_content_length_over_cap_rejected() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", (MAX_RESPONSE_BYTES + 1).to_string())
                    .set_body_bytes(vec![b'x'; MAX_RESPONSE_BYTES + 1]),
            )
            .mount(&server)
            .await;

        let backend = ExaBackend::with_base_url(server.uri(), "k".into());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::Backend(ref m) if m == "response too large"));
    }

    #[tokio::test]
    async fn exa_streamed_body_over_cap_rejected() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(vec![b'x'; MAX_RESPONSE_BYTES + 1]),
            )
            .mount(&server)
            .await;

        let backend = ExaBackend::with_base_url(server.uri(), "k".into());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::Backend(ref m) if m == "response too large"));
    }

    struct FakeSecondary;

    #[async_trait::async_trait]
    impl SearchBackend for FakeSecondary {
        async fn search(
            &self,
            _q: &str,
            _c: usize,
        ) -> std::result::Result<Vec<SearchResult>, SearchError> {
            Ok(vec![SearchResult {
                title: "ddg".into(),
                url: "https://ddg/x".into(),
                snippet: "fb".into(),
            }])
        }
    }

    async fn assert_exa_status_falls_back(status: u16) {
        use crate::tools::search::fallback::FallbackBackend;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;

        let fb = FallbackBackend::new(
            Box::new(ExaBackend::with_base_url(server.uri(), "k".into())),
            Box::new(FakeSecondary),
        );
        let results = fb.search("rust", 5).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "ddg");
    }

    #[tokio::test]
    async fn exa_429_falls_back_to_secondary() {
        assert_exa_status_falls_back(429).await;
    }

    #[tokio::test]
    async fn exa_402_falls_back_to_secondary() {
        assert_exa_status_falls_back(402).await;
    }
}
