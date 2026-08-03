use std::time::Duration;

use async_trait::async_trait;

use super::{SearchBackend, SearchError, SearchResult};

const DEFAULT_MAX_RETRIES: usize = 2;
const DEFAULT_BASE_DELAY: Duration = Duration::from_secs(1);

pub fn retry_delay(failed_attempt_index: usize, base_delay: Duration) -> Duration {
    let Some(factor) = 1_u32.checked_shl(failed_attempt_index as u32) else {
        return base_delay;
    };
    base_delay.checked_mul(factor).unwrap_or(base_delay)
}

pub struct RetryBackend {
    inner: Box<dyn SearchBackend>,
    max_retries: usize,
    base_delay: Duration,
}

impl RetryBackend {
    pub fn new(inner: Box<dyn SearchBackend>) -> Self {
        Self {
            inner,
            max_retries: DEFAULT_MAX_RETRIES,
            base_delay: DEFAULT_BASE_DELAY,
        }
    }

    pub fn with_zero_backoff(inner: Box<dyn SearchBackend>) -> Self {
        Self {
            inner,
            max_retries: DEFAULT_MAX_RETRIES,
            base_delay: Duration::ZERO,
        }
    }
}

#[async_trait]
impl SearchBackend for RetryBackend {
    async fn search(
        &self,
        query: &str,
        count: usize,
    ) -> std::result::Result<Vec<SearchResult>, SearchError> {
        let max_attempts = self.max_retries + 1;

        for failed_attempt_index in 0..max_attempts {
            match self.inner.search(query, count).await {
                Ok(results) => return Ok(results),
                Err(SearchError::RateLimited) if failed_attempt_index + 1 < max_attempts => {
                    let delay = retry_delay(failed_attempt_index, self.base_delay);
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                }
                Err(err) => return Err(err),
            }
        }

        Err(SearchError::RateLimited)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use crate::tools::search::retry::{retry_delay, RetryBackend};
    use crate::tools::search::{SearchBackend, SearchError, SearchResult};

    struct FakeSequence {
        errors: Mutex<Vec<SearchError>>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeSequence {
        fn new(errors: Vec<SearchError>) -> Self {
            Self {
                errors: Mutex::new(errors),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl SearchBackend for FakeSequence {
        async fn search(
            &self,
            _query: &str,
            _count: usize,
        ) -> std::result::Result<Vec<SearchResult>, SearchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut errors = self.errors.lock().unwrap();
            if errors.is_empty() {
                return Ok(vec![SearchResult {
                    title: "t".into(),
                    url: "https://example.test".into(),
                    snippet: "s".into(),
                }]);
            }
            Err(errors.remove(0))
        }
    }

    fn box_with_handle(errors: Vec<SearchError>) -> (Box<FakeSequence>, Arc<AtomicUsize>) {
        let backend = Box::new(FakeSequence::new(errors));
        let handle = Arc::clone(&backend.calls);
        (backend, handle)
    }

    #[tokio::test]
    async fn ratelimit_succeeds_after_one_or_two_retries() {
        let (inner, handle) = box_with_handle(vec![SearchError::RateLimited]);
        let retry = RetryBackend::with_zero_backoff(inner);
        assert_eq!(retry.search("q", 5).await.unwrap().len(), 1);
        assert_eq!(handle.load(Ordering::SeqCst), 2);

        let (inner, handle) =
            box_with_handle(vec![SearchError::RateLimited, SearchError::RateLimited]);
        let retry = RetryBackend::with_zero_backoff(inner);
        assert_eq!(retry.search("q", 5).await.unwrap().len(), 1);
        assert_eq!(handle.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn ratelimit_returns_ratelimit_after_retries_are_exhausted() {
        let (inner, handle) = box_with_handle(vec![
            SearchError::RateLimited,
            SearchError::RateLimited,
            SearchError::RateLimited,
        ]);
        let retry = RetryBackend::with_zero_backoff(inner);

        assert!(matches!(
            retry.search("q", 5).await.unwrap_err(),
            SearchError::RateLimited
        ));
        assert_eq!(handle.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_ratelimit_errors_return_immediately() {
        let cases = vec![
            SearchError::Auth,
            SearchError::QuotaExhausted,
            SearchError::InvalidRequest("bad".into()),
            SearchError::Backend("down".into()),
        ];

        for error in cases {
            let (inner, handle) = box_with_handle(vec![error]);
            let retry = RetryBackend::with_zero_backoff(inner);
            let err = retry.search("q", 5).await.unwrap_err();
            assert_eq!(handle.load(Ordering::SeqCst), 1);
            assert!(!matches!(err, SearchError::RateLimited));
        }
    }

    #[test]
    fn retry_delay_is_exponential_and_zero_stays_zero() {
        assert_eq!(
            retry_delay(0, Duration::from_secs(1)),
            Duration::from_secs(1)
        );
        assert_eq!(
            retry_delay(1, Duration::from_secs(1)),
            Duration::from_secs(2)
        );
        assert_eq!(
            retry_delay(2, Duration::from_secs(1)),
            Duration::from_secs(4)
        );
        assert_eq!(retry_delay(10, Duration::ZERO), Duration::ZERO);
        assert_eq!(
            retry_delay(usize::MAX, Duration::from_secs(1)),
            Duration::from_secs(1)
        );
    }
}
