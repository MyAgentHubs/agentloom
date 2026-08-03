//! 组合后端：primary 失败且属「可降级错误」时调 fallback；否则把 Err 原样冒上去。

use async_trait::async_trait;

use super::{SearchBackend, SearchError, SearchResult};

/// 纯函数：primary 这个错误要不要退到 fallback。
pub fn falls_back_to_ddg(err: &SearchError) -> bool {
    matches!(err, SearchError::RateLimited | SearchError::QuotaExhausted)
}

pub struct FallbackBackend {
    primary: Box<dyn SearchBackend>,
    fallback: Box<dyn SearchBackend>,
}

impl FallbackBackend {
    pub fn new(primary: Box<dyn SearchBackend>, fallback: Box<dyn SearchBackend>) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl SearchBackend for FallbackBackend {
    async fn search(
        &self,
        query: &str,
        count: usize,
    ) -> std::result::Result<Vec<SearchResult>, SearchError> {
        match self.primary.search(query, count).await {
            Ok(r) => Ok(r),
            Err(e) if falls_back_to_ddg(&e) => self.fallback.search(query, count).await,
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::{falls_back_to_ddg, FallbackBackend};
    use crate::tools::search::{SearchBackend, SearchError, SearchResult};

    struct FakeErr(SearchError);

    #[async_trait]
    impl SearchBackend for FakeErr {
        async fn search(
            &self,
            _query: &str,
            _count: usize,
        ) -> std::result::Result<Vec<SearchResult>, SearchError> {
            Err(match &self.0 {
                SearchError::Auth => SearchError::Auth,
                SearchError::RateLimited => SearchError::RateLimited,
                SearchError::QuotaExhausted => SearchError::QuotaExhausted,
                SearchError::InvalidRequest(m) => SearchError::InvalidRequest(m.clone()),
                SearchError::Backend(m) => SearchError::Backend(m.clone()),
            })
        }
    }

    struct FakeOk;

    #[async_trait]
    impl SearchBackend for FakeOk {
        async fn search(
            &self,
            _query: &str,
            _count: usize,
        ) -> std::result::Result<Vec<SearchResult>, SearchError> {
            Ok(vec![SearchResult {
                title: "t".into(),
                url: "https://example.test".into(),
                snippet: "s".into(),
            }])
        }
    }

    #[test]
    fn falls_back_only_on_ratelimit_or_quota() {
        use crate::tools::search::SearchError;

        assert!(falls_back_to_ddg(&SearchError::RateLimited));
        assert!(falls_back_to_ddg(&SearchError::QuotaExhausted));
        assert!(!falls_back_to_ddg(&SearchError::Auth));
        assert!(!falls_back_to_ddg(&SearchError::InvalidRequest("x".into())));
        assert!(!falls_back_to_ddg(&SearchError::Backend("x".into())));
    }

    #[tokio::test]
    async fn primary_ratelimit_falls_to_secondary() {
        let fb = FallbackBackend::new(
            Box::new(FakeErr(SearchError::RateLimited)),
            Box::new(FakeOk),
        );
        assert_eq!(fb.search("q", 5).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn primary_auth_error_propagates_no_fallback() {
        let fb = FallbackBackend::new(Box::new(FakeErr(SearchError::Auth)), Box::new(FakeOk));
        assert!(matches!(
            fb.search("q", 5).await.unwrap_err(),
            SearchError::Auth
        ));
    }
}
