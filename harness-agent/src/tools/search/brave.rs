//! Brave Search API 后端：带 key、结构化 JSON。parse/cap 纯函数（录制样例测·不联网）；
//! search 经统一出口 client·流式累计响应体·超 MAX_RESPONSE_BYTES 即停。Brave 不做 retry·由上层 FallbackBackend 兜。

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;

use super::{
    classify_brave_status, egress_client, SearchBackend, SearchError, SearchResult,
    MAX_RESPONSE_BYTES, SEARCH_TIMEOUT_SECS,
};

const DEFAULT_BASE_URL: &str = "https://api.search.brave.com";
const MAX_BRAVE_QUERY_CHARS: usize = 400;
const MAX_BRAVE_QUERY_WORDS: usize = 50;

pub struct BraveBackend {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl BraveBackend {
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

/// 纯函数：query 裁到 Brave 上限（400 字符 / 50 词）。
pub fn cap_brave_query(q: &str) -> String {
    let words: Vec<&str> = q.split_whitespace().take(MAX_BRAVE_QUERY_WORDS).collect();
    words
        .join(" ")
        .chars()
        .take(MAX_BRAVE_QUERY_CHARS)
        .collect()
}

#[derive(Deserialize)]
struct BraveResp {
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    results: Vec<BraveItem>,
}

#[derive(Deserialize)]
struct BraveItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

/// 纯函数：Brave JSON → 归一结果（至多 count 条）。
pub fn parse_brave_json(body: &str, count: usize) -> Vec<SearchResult> {
    let parsed: BraveResp = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(web) = parsed.web else {
        return Vec::new();
    };
    web.results
        .into_iter()
        .filter(|it| !it.title.is_empty() && !it.url.is_empty())
        .take(count)
        .map(|it| SearchResult {
            title: it.title,
            url: it.url,
            snippet: it.description,
        })
        .collect()
}

#[async_trait]
impl SearchBackend for BraveBackend {
    async fn search(
        &self,
        query: &str,
        count: usize,
    ) -> std::result::Result<Vec<SearchResult>, SearchError> {
        let q = cap_brave_query(query);
        let url = format!("{}/res/v1/web/search", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .get(&url)
            .header("X-Subscription-Token", &self.api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .query(&[
                ("q", q.as_str()),
                ("count", &count.to_string()),
                ("result_filter", "web"),
                ("text_decorations", "false"),
            ])
            .timeout(std::time::Duration::from_secs(SEARCH_TIMEOUT_SECS))
            .send()
            .await
            .map_err(|e| SearchError::Backend(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(classify_brave_status(status.as_u16()));
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
        Ok(parse_brave_json(&String::from_utf8_lossy(&buf), count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::search::{SearchBackend, SearchError, MAX_RESPONSE_BYTES};

    const SAMPLE: &str = r#"
    {
      "web": {
        "results": [
          {
            "title": "Rust 官网",
            "url": "https://example.com/rust",
            "description": "Rust 是一门系统编程语言。"
          },
          {
            "title": "Async Rust",
            "url": "https://example.org/async",
            "description": "异步编程入门。"
          },
          {
            "title": "",
            "url": "https://example.net/skip",
            "description": "missing title"
          }
        ]
      }
    }
    "#;

    #[test]
    fn parse_brave_json_parses_web_results() {
        let r = parse_brave_json(SAMPLE, 10);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Rust 官网");
        assert_eq!(r[0].url, "https://example.com/rust");
        assert_eq!(r[0].snippet, "Rust 是一门系统编程语言。");
    }

    #[test]
    fn parse_brave_json_respects_count() {
        assert_eq!(parse_brave_json(SAMPLE, 1).len(), 1);
    }

    #[test]
    fn cap_brave_query_caps_to_400_chars_and_50_words() {
        let long_chars = "x".repeat(500);
        assert_eq!(cap_brave_query(&long_chars).chars().count(), 400);

        let many_words = (0..80)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let capped = cap_brave_query(&many_words);
        assert_eq!(capped.split_whitespace().count(), 50);
        assert!(capped.chars().count() <= 400);
    }

    #[tokio::test]
    async fn brave_200_sends_subscription_token_without_leaking_credentials() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
            .mount(&server)
            .await;

        let backend = BraveBackend::with_base_url(server.uri(), "k".into());
        let results = backend.search("rust", 2).await.unwrap();
        assert_eq!(results.len(), 2);

        let reqs = server.received_requests().await.unwrap();
        let r: &Request = &reqs[0];
        assert_eq!(r.headers.get("x-subscription-token").unwrap(), "k");
        assert!(r.headers.get("authorization").is_none());
        assert!(r.headers.get("cookie").is_none());
        assert_eq!(
            r.url.query_pairs().find(|(k, _)| k == "q").unwrap().1,
            "rust"
        );
        assert_eq!(
            r.url.query_pairs().find(|(k, _)| k == "count").unwrap().1,
            "2"
        );
        assert_eq!(
            r.url
                .query_pairs()
                .find(|(k, _)| k == "result_filter")
                .unwrap()
                .1,
            "web"
        );
        assert_eq!(
            r.url
                .query_pairs()
                .find(|(k, _)| k == "text_decorations")
                .unwrap()
                .1,
            "false"
        );
    }

    #[tokio::test]
    async fn brave_401_maps_to_auth() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let backend = BraveBackend::with_base_url(server.uri(), "k".into());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::Auth));
    }

    #[tokio::test]
    async fn brave_429_maps_to_rate_limited() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let backend = BraveBackend::with_base_url(server.uri(), "k".into());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::RateLimited));
    }

    #[tokio::test]
    async fn brave_declared_content_length_over_cap_rejected() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", (MAX_RESPONSE_BYTES + 1).to_string())
                    .set_body_bytes(vec![b'x'; MAX_RESPONSE_BYTES + 1]),
            )
            .mount(&server)
            .await;

        let backend = BraveBackend::with_base_url(server.uri(), "k".into());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::Backend(ref m) if m == "response too large"));
    }

    #[tokio::test]
    async fn brave_streamed_body_over_cap_rejected() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(vec![b'x'; MAX_RESPONSE_BYTES + 1]),
            )
            .mount(&server)
            .await;

        let backend = BraveBackend::with_base_url(server.uri(), "k".into());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::Backend(ref m) if m == "response too large"));
    }

    #[tokio::test]
    async fn brave_400_maps_to_invalid_request() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        let backend = BraveBackend::with_base_url(server.uri(), "k".into());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn brave_500_maps_to_backend() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let backend = BraveBackend::with_base_url(server.uri(), "k".into());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::Backend(_)));
    }
}
