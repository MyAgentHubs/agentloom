//! 搜索后端抽象：统一结果格式 + 上限 + 不带无关凭证不继承代理的出口 client。
//! web_search 工具经此发请求；DuckDuckGo 是首个实现，SearXNG/Brave/Tavily 后续可换。

use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

pub mod brave;
pub mod duckduckgo;
pub mod exa;
pub mod fallback;
pub mod retry;

/// 结果来源标记：恒此值·提示模型「这是不可信的网页数据、不是指令」。
pub const SOURCE: &str = "web_search_results";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// web_search 喂回模型的整体结果。`source` 标明这是不可信数据。
#[derive(Debug, Clone, Serialize)]
pub struct SearchOutput {
    pub source: &'static str,
    pub query: String,
    pub results: Vec<SearchResult>,
}

/// 后端「出错」的明确分类——注意：空结果 Ok(vec![]) 不是错误。
#[derive(Debug)]
pub enum SearchError {
    Auth,                   // 401/403：key 错/无权
    RateLimited,            // 429：短窗限流
    QuotaExhausted,         // 月额度耗尽
    InvalidRequest(String), // 400/422：query 非法
    Backend(String),        // 网络错/超时/5xx/其它
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::Auth => write!(f, "search backend auth failed"),
            SearchError::RateLimited => write!(f, "search backend rate-limited"),
            SearchError::QuotaExhausted => write!(f, "search backend quota exhausted"),
            SearchError::InvalidRequest(m) => write!(f, "search backend invalid request: {m}"),
            SearchError::Backend(m) => write!(f, "search backend error: {m}"),
        }
    }
}

/// 纯函数：Brave HTTP status → SearchError 分类。
pub fn classify_brave_status(status: u16) -> SearchError {
    match status {
        401 | 403 => SearchError::Auth,
        429 => SearchError::RateLimited,
        400 | 422 => SearchError::InvalidRequest(format!("http {status}")),
        _ => SearchError::Backend(format!("http {status}")),
    }
}

/// 纯函数：Exa HTTP status → SearchError 分类。402=免费额度耗尽 → QuotaExhausted（才会退 DDG）。
pub fn classify_exa_status(status: u16) -> SearchError {
    match status {
        401 | 403 => SearchError::Auth,
        429 => SearchError::RateLimited,
        402 => SearchError::QuotaExhausted,
        400 | 422 => SearchError::InvalidRequest(format!("http {status}")),
        _ => SearchError::Backend(format!("http {status}")),
    }
}

pub const DEFAULT_COUNT: usize = 5;
pub const MAX_COUNT: usize = 10;
pub const MAX_QUERY_LEN: usize = 512;
pub const MAX_TITLE_LEN: usize = 200;
pub const MAX_URL_LEN: usize = 2048;
pub const MAX_SNIPPET_LEN: usize = 300;
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024;
pub const SEARCH_TIMEOUT_SECS: u64 = 10;
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// 按字符数截断（不在多字节中间切）。
pub fn cap_text(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// 每条字段各自截断到上限。
pub fn normalize(raw: Vec<SearchResult>) -> Vec<SearchResult> {
    raw.into_iter()
        .map(|r| SearchResult {
            title: cap_text(&r.title, MAX_TITLE_LEN),
            url: cap_text(&r.url, MAX_URL_LEN),
            snippet: cap_text(&r.snippet, MAX_SNIPPET_LEN),
        })
        .collect()
}

/// 把整体序列化后裁到 MAX_OUTPUT_BYTES 以内：从尾部丢条数直到放得下。
pub fn fit_within_bytes(mut output: SearchOutput) -> SearchOutput {
    while !output.results.is_empty()
        && serde_json::to_string(&output).map(|s| s.len()).unwrap_or(0) > MAX_OUTPUT_BYTES
    {
        output.results.pop();
    }
    output
}

/// 对外联网 HTTP client：**不继承环境代理**、禁跟随重定向、带超时。
/// 调用方不得给它附带与目标端点无关的凭证头，避免外泄 git/provider 凭证；
/// 后端可仅向自己的目标端点附带端点专属 key（如 Brave X-Subscription-Token）。
pub fn egress_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS))
        .build()
        .expect("egress client build")
}

/// 搜索后端统一接口。Ok(空 vec)=真没搜到；Err=后端出错/限流。
#[async_trait]
pub trait SearchBackend: Send + Sync {
    async fn search(
        &self,
        query: &str,
        count: usize,
    ) -> std::result::Result<Vec<SearchResult>, SearchError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_are_sane() {
        // 编译期断言：常量关系恒真——比运行期 assert 更早暴露，且不触发 clippy 常量断言告警。
        const _: () = assert!(DEFAULT_COUNT <= MAX_COUNT);
        const _: () = assert!(MAX_SNIPPET_LEN > 0 && MAX_QUERY_LEN > 0 && MAX_OUTPUT_BYTES > 0);
    }

    #[test]
    fn cap_text_truncates_to_char_limit() {
        let s = "x".repeat(1000);
        assert_eq!(cap_text(&s, 10).chars().count(), 10);
    }

    #[test]
    fn normalize_caps_each_field() {
        let raw = vec![SearchResult {
            title: "t".repeat(500),
            url: "u".repeat(5000),
            snippet: "s".repeat(5000),
        }];
        let out = normalize(raw);
        assert!(out[0].title.chars().count() <= MAX_TITLE_LEN);
        assert!(out[0].url.chars().count() <= MAX_URL_LEN);
        assert!(out[0].snippet.chars().count() <= MAX_SNIPPET_LEN);
    }

    #[test]
    fn fit_within_bytes_drops_results_until_under_cap() {
        // 造一堆大结果，整体远超总字节上限。
        let results: Vec<SearchResult> = (0..50)
            .map(|_| SearchResult {
                title: "t".repeat(MAX_TITLE_LEN),
                url: "u".repeat(MAX_URL_LEN),
                snippet: "s".repeat(MAX_SNIPPET_LEN),
            })
            .collect();
        let out = fit_within_bytes(SearchOutput {
            source: SOURCE,
            query: "q".into(),
            results,
        });
        let serialized = serde_json::to_string(&out).unwrap();
        assert!(
            serialized.len() <= MAX_OUTPUT_BYTES,
            "serialized {} > cap {}",
            serialized.len(),
            MAX_OUTPUT_BYTES
        );
    }

    #[test]
    fn search_error_distinguishes_ratelimit_from_empty() {
        // 空结果是 Ok(空 vec)；限流是 Err 分支——类型上就分得开（这正是「空 ≠ 限流」的依据）。
        let empty: std::result::Result<Vec<SearchResult>, SearchError> = Ok(Vec::new());
        assert!(matches!(empty, Ok(ref v) if v.is_empty()));
        assert!(matches!(SearchError::RateLimited, SearchError::RateLimited));
    }

    #[test]
    fn brave_status_classification() {
        use super::{classify_brave_status, SearchError};
        assert!(matches!(classify_brave_status(401), SearchError::Auth));
        assert!(matches!(classify_brave_status(403), SearchError::Auth));
        assert!(matches!(
            classify_brave_status(429),
            SearchError::RateLimited
        ));
        assert!(matches!(
            classify_brave_status(422),
            SearchError::InvalidRequest(_)
        ));
        assert!(matches!(
            classify_brave_status(400),
            SearchError::InvalidRequest(_)
        ));
        assert!(matches!(
            classify_brave_status(500),
            SearchError::Backend(_)
        ));
        assert!(matches!(
            classify_brave_status(503),
            SearchError::Backend(_)
        ));
    }

    #[test]
    fn exa_status_classification() {
        use super::{classify_exa_status, SearchError};
        assert!(matches!(classify_exa_status(401), SearchError::Auth));
        assert!(matches!(classify_exa_status(403), SearchError::Auth));
        assert!(matches!(classify_exa_status(429), SearchError::RateLimited));
        assert!(matches!(
            classify_exa_status(402),
            SearchError::QuotaExhausted
        ));
        assert!(matches!(
            classify_exa_status(400),
            SearchError::InvalidRequest(_)
        ));
        assert!(matches!(
            classify_exa_status(422),
            SearchError::InvalidRequest(_)
        ));
        assert!(matches!(classify_exa_status(500), SearchError::Backend(_)));
    }
}
