//! DuckDuckGo 后端：无 key、零配置。parse 是纯函数（拿录制样例测·不联网）；
//! search 经统一出口 client 发请求，并**流式**累计响应体、超 MAX_RESPONSE_BYTES 即停（不先下完再判）。

use async_trait::async_trait;
use futures_util::StreamExt;
use scraper::{Html, Selector};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{egress_client, SearchBackend, SearchError, SearchResult, MAX_RESPONSE_BYTES};

const DEFAULT_BASE_URL: &str = "https://html.duckduckgo.com";
const USER_AGENT: &str = "Mozilla/5.0 (compatible; myagent/1.0)";
const DEFAULT_MIN_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_MAX_ATTEMPTS: usize = 3;
const DEFAULT_BASE_DELAY: Duration = Duration::from_secs(1);

pub struct DuckDuckGoBackend {
    base_url: String,
    client: reqwest::Client,
    last_request: Mutex<Option<Instant>>,
    min_interval: Duration,
    max_attempts: usize,
    base_delay: Duration,
}

impl Default for DuckDuckGoBackend {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            client: egress_client(),
            last_request: Mutex::new(None),
            min_interval: DEFAULT_MIN_INTERVAL,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: DEFAULT_BASE_DELAY,
        }
    }
}

impl DuckDuckGoBackend {
    /// 测试用：指向 wiremock。
    pub fn with_base_url(base_url: String) -> Self {
        Self {
            base_url,
            client: egress_client(),
            last_request: Mutex::new(None),
            min_interval: Duration::ZERO,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: Duration::ZERO,
        }
    }

    async fn wait_for_min_interval(&self) {
        let wait = {
            let mut g = self.last_request.lock().unwrap();
            let now = Instant::now();
            let wait = calculate_min_interval_wait(*g, now, self.min_interval);
            *g = Some(now + wait);
            wait
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    async fn fetch_once(&self, query: &str) -> std::result::Result<String, FetchError> {
        self.wait_for_min_interval().await;

        let url = format!("{}/html/", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .get(&url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .query(&[("q", query)])
            .send()
            .await
            .map_err(|e| FetchError::retryable_backend(e.to_string()))?;

        let status = resp.status();
        // 状态级反爬不依赖 body，也不受 content-length 预检影响。
        if status == reqwest::StatusCode::ACCEPTED
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            return Err(FetchError::retryable(SearchError::RateLimited));
        }
        // 已知 content-length 超限直接拒。
        if let Some(len) = resp.content_length() {
            if len as usize > MAX_RESPONSE_BYTES {
                return Err(FetchError::fatal(SearchError::Backend(
                    "response too large".into(),
                )));
            }
        }
        // 流式累计·边下边判·绝不先下完整个 body。
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| FetchError::fatal(SearchError::Backend(e.to_string())))?;
            if buf.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(FetchError::fatal(SearchError::Backend(
                    "response too large".into(),
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&buf).into_owned();

        if is_blocked_response(status, &body) {
            return Err(FetchError::retryable(SearchError::RateLimited));
        }
        if !status.is_success() {
            return Err(FetchError::fatal(SearchError::Backend(format!(
                "http {status}"
            ))));
        }
        Ok(body)
    }

    fn retry_delay(&self, failed_attempt_index: usize) -> Duration {
        let factor = 1_u32
            .checked_shl(failed_attempt_index as u32)
            .unwrap_or(u32::MAX);
        self.base_delay.saturating_mul(factor)
    }
}

enum FetchError {
    Retryable(SearchError),
    Fatal(SearchError),
}

impl FetchError {
    fn retryable(err: SearchError) -> Self {
        Self::Retryable(err)
    }

    fn retryable_backend(message: String) -> Self {
        Self::Retryable(SearchError::Backend(message))
    }

    fn fatal(err: SearchError) -> Self {
        Self::Fatal(err)
    }

    fn into_search_error(self) -> SearchError {
        match self {
            Self::Retryable(err) | Self::Fatal(err) => err,
        }
    }

    fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable(_))
    }
}

fn is_blocked_response(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::ACCEPTED
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || body.to_ascii_lowercase().contains("anomaly")
}

fn calculate_min_interval_wait(
    last_request: Option<Instant>,
    now: Instant,
    min_interval: Duration,
) -> Duration {
    last_request
        .map(|last| (last + min_interval).saturating_duration_since(now))
        .unwrap_or_default()
}

/// 纯函数：DuckDuckGo html → 归一结果（至多 count 条）。
pub fn parse_duckduckgo_html(html: &str, count: usize) -> Vec<SearchResult> {
    let doc = Html::parse_document(html);
    let title_sel = Selector::parse("a.result__a").unwrap();
    let snippet_sel = Selector::parse("a.result__snippet").unwrap();
    let block_sel = Selector::parse("div.result").unwrap();

    let mut out = Vec::new();
    for block in doc.select(&block_sel) {
        if out.len() >= count {
            break;
        }
        let Some(a) = block.select(&title_sel).next() else {
            continue;
        };
        let title = a.text().collect::<String>().trim().to_string();
        let url = a.value().attr("href").unwrap_or_default().to_string();
        let snippet = block
            .select(&snippet_sel)
            .next()
            .map(|s| s.text().collect::<String>().trim().to_string())
            .unwrap_or_default();
        if title.is_empty() || url.is_empty() {
            continue;
        }
        out.push(SearchResult {
            title,
            url,
            snippet,
        });
    }
    out
}

#[async_trait]
impl SearchBackend for DuckDuckGoBackend {
    async fn search(
        &self,
        query: &str,
        count: usize,
    ) -> std::result::Result<Vec<SearchResult>, SearchError> {
        let max_attempts = self.max_attempts.max(1);
        let mut last_error = None;

        for failed_attempt_index in 0..max_attempts {
            match self.fetch_once(query).await {
                Ok(body) => return Ok(parse_duckduckgo_html(&body, count)),
                Err(err) => {
                    let should_retry =
                        err.is_retryable() && failed_attempt_index + 1 < max_attempts;
                    let err = err.into_search_error();
                    if should_retry {
                        let delay = self.retry_delay(failed_attempt_index);
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        last_error = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| SearchError::Backend("search failed".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::search::SearchBackend;

    const SAMPLE: &str = r#"
    <div class="result results_links">
      <a class="result__a" href="https://example.com/rust">Rust 官网</a>
      <a class="result__snippet">Rust 是一门系统编程语言。</a>
    </div>
    <div class="result results_links">
      <a class="result__a" href="https://example.org/async">Async Rust</a>
      <a class="result__snippet">异步编程入门。</a>
    </div>
    "#;

    #[test]
    fn parses_title_url_snippet() {
        let r = parse_duckduckgo_html(SAMPLE, 10);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Rust 官网");
        assert_eq!(r[0].url, "https://example.com/rust");
        assert_eq!(r[0].snippet, "Rust 是一门系统编程语言。");
    }

    #[test]
    fn parse_respects_count() {
        assert_eq!(parse_duckduckgo_html(SAMPLE, 1).len(), 1);
    }

    #[test]
    fn parse_empty_html_yields_no_results() {
        assert!(parse_duckduckgo_html("<html></html>", 10).is_empty());
    }

    #[test]
    fn blocked_response_detection_recognizes_status_and_anomaly_body() {
        assert!(is_blocked_response(
            reqwest::StatusCode::ACCEPTED,
            "plain body"
        ));
        assert!(is_blocked_response(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "plain body"
        ));
        assert!(is_blocked_response(
            reqwest::StatusCode::OK,
            "DuckDuckGo AnOmAlY challenge"
        ));
        assert!(!is_blocked_response(reqwest::StatusCode::OK, SAMPLE));
    }

    #[test]
    fn min_interval_wait_handles_empty_and_recent_last_request() {
        let now = std::time::Instant::now();
        let interval = std::time::Duration::from_secs(2);

        assert_eq!(
            calculate_min_interval_wait(None, now, interval),
            std::time::Duration::ZERO
        );

        let wait = calculate_min_interval_wait(
            Some(now - std::time::Duration::from_millis(500)),
            now,
            interval,
        );
        assert_eq!(wait, std::time::Duration::from_millis(1500));

        let wait = calculate_min_interval_wait(
            Some(now + std::time::Duration::from_secs(1)),
            now,
            interval,
        );
        assert_eq!(wait, std::time::Duration::from_secs(3));
    }

    #[tokio::test]
    async fn search_hits_backend_without_credentials() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
            .mount(&server)
            .await;

        let backend = DuckDuckGoBackend::with_base_url(server.uri());
        let results = backend.search("rust", 10).await.unwrap();
        assert_eq!(results.len(), 2);

        let reqs = server.received_requests().await.unwrap();
        let r: &Request = &reqs[0];
        // 不外泄凭证：没有 Authorization、没有 Cookie。
        assert!(r.headers.get("authorization").is_none());
        assert!(r.headers.get("cookie").is_none());
    }

    #[tokio::test]
    async fn blocked_response_returns_rate_limited_after_retries() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .respond_with(ResponseTemplate::new(202).set_body_string("anomaly challenge"))
            .expect(3)
            .mount(&server)
            .await;

        let backend = DuckDuckGoBackend::with_base_url(server.uri());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::RateLimited));
    }

    #[tokio::test]
    async fn status_blocked_response_ignores_large_declared_content_length() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .respond_with(
                ResponseTemplate::new(202).set_body_bytes(vec![b'x'; MAX_RESPONSE_BYTES + 1]),
            )
            .expect(3)
            .mount(&server)
            .await;

        let backend = DuckDuckGoBackend::with_base_url(server.uri());
        let err = backend.search("rust", 10).await.unwrap_err();
        assert!(matches!(err, SearchError::RateLimited));
    }

    #[tokio::test]
    async fn blocked_response_retries_and_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .respond_with(ResponseTemplate::new(202).set_body_string("anomaly challenge"))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;

        let backend = DuckDuckGoBackend::with_base_url(server.uri());
        let results = backend.search("rust", 10).await.unwrap();
        assert_eq!(results.len(), 2);
    }
}
