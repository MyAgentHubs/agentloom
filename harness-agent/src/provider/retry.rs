use std::time::Duration;

/// Pure retry-policy configuration for HTTP provider calls.
///
/// Small, `Copy`-able struct with no dependencies on I/O or async.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_retries: usize,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 500,
            max_delay_ms: 8000,
        }
    }
}

/// Returns `true` only for HTTP status codes that are safe to retry:
/// 429 (Too Many Requests) and any 5xx server error.
///
/// All other codes (e.g. 400, 401, 404) are considered permanent failures.
pub fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// 主模型一轮结束后是否该换 fallback：传输错(is_err)，或最终仍是可重试状态(429/5xx)。
/// 非可重试 4xx / 成功 → 不换（fallback 救不了 4xx）。
pub fn warrants_fallback(is_err: bool, final_status: Option<u16>) -> bool {
    is_err || final_status.is_some_and(is_retryable_status)
}

/// Exponential backoff delay clamped to `policy.max_delay_ms`.
///
/// - attempt 0 → `base_delay_ms`
/// - attempt 1 → `2 × base_delay_ms`
/// - attempt 2 → `4 × base_delay_ms`
/// - … and so on, never exceeding `max_delay_ms`.
///
/// This function will **not** panic for arbitrarily large `attempt` values;
/// it uses checked arithmetic and saturates at `max_delay_ms`.
pub fn backoff_delay(policy: &RetryPolicy, attempt: usize) -> Duration {
    let delay_ms = match 1u64.checked_shl(attempt as u32) {
        Some(multiplier) => policy
            .base_delay_ms
            .saturating_mul(multiplier)
            .min(policy.max_delay_ms),
        None => policy.max_delay_ms,
    };
    Duration::from_millis(delay_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_retryable_status ──────────────────────────────────────────

    #[test]
    fn retryable_status_429() {
        assert!(is_retryable_status(429));
    }

    #[test]
    fn retryable_status_500() {
        assert!(is_retryable_status(500));
    }

    #[test]
    fn retryable_status_503() {
        assert!(is_retryable_status(503));
    }

    #[test]
    fn non_retryable_400() {
        assert!(!is_retryable_status(400));
    }

    #[test]
    fn non_retryable_401() {
        assert!(!is_retryable_status(401));
    }

    #[test]
    fn non_retryable_404() {
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn non_retryable_200() {
        assert!(!is_retryable_status(200));
    }

    #[test]
    fn non_retryable_308() {
        assert!(!is_retryable_status(308));
    }

    // ── warrants_fallback ────────────────────────────────────────────

    #[test]
    fn warrants_fallback_transport_error() {
        assert!(warrants_fallback(true, None));
    }

    #[test]
    fn warrants_fallback_retryable_429() {
        assert!(warrants_fallback(false, Some(429)));
    }

    #[test]
    fn warrants_fallback_retryable_503() {
        assert!(warrants_fallback(false, Some(503)));
    }

    #[test]
    fn warrants_fallback_no_for_success_200() {
        assert!(!warrants_fallback(false, Some(200)));
    }

    #[test]
    fn warrants_fallback_no_for_client_error_404() {
        assert!(!warrants_fallback(false, Some(404)));
    }

    #[test]
    fn warrants_fallback_no_for_ok_no_status() {
        assert!(!warrants_fallback(false, None));
    }

    // ── backoff_delay ────────────────────────────────────────────────

    #[test]
    fn backoff_attempt_0_is_base() {
        let policy = RetryPolicy::default();
        assert_eq!(backoff_delay(&policy, 0), Duration::from_millis(500));
    }

    #[test]
    fn backoff_attempt_1_is_2x_base() {
        let policy = RetryPolicy::default();
        assert_eq!(backoff_delay(&policy, 1), Duration::from_millis(1000));
    }

    #[test]
    fn backoff_attempt_2_is_4x_base() {
        let policy = RetryPolicy::default();
        assert_eq!(backoff_delay(&policy, 2), Duration::from_millis(2000));
    }

    #[test]
    fn backoff_clamped_at_max() {
        let policy = RetryPolicy::default();
        // With base=500, attempt 4 gives 8000 (=500*16), attempt 5 would give 16000 > max
        assert_eq!(backoff_delay(&policy, 4), Duration::from_millis(8000));
        assert_eq!(backoff_delay(&policy, 5), Duration::from_millis(8000));
        assert_eq!(backoff_delay(&policy, 10), Duration::from_millis(8000));
    }

    #[test]
    fn backoff_very_large_attempt_no_panic() {
        let policy = RetryPolicy::default();
        // These must not panic, just clamp.
        let d = backoff_delay(&policy, usize::MAX);
        assert_eq!(d, Duration::from_millis(8000));
        let d = backoff_delay(&policy, 100);
        assert_eq!(d, Duration::from_millis(8000));
    }

    #[test]
    fn backoff_different_base() {
        let policy = RetryPolicy {
            max_retries: 5,
            base_delay_ms: 100,
            max_delay_ms: 1000,
        };
        assert_eq!(backoff_delay(&policy, 0), Duration::from_millis(100));
        assert_eq!(backoff_delay(&policy, 1), Duration::from_millis(200));
        assert_eq!(backoff_delay(&policy, 2), Duration::from_millis(400));
        assert_eq!(backoff_delay(&policy, 3), Duration::from_millis(800));
        assert_eq!(backoff_delay(&policy, 4), Duration::from_millis(1000)); // clamped
    }

    // ── RetryPolicy::default() ───────────────────────────────────────

    #[test]
    fn default_policy_values() {
        let p = RetryPolicy::default();
        assert_eq!(p.max_retries, 3);
        assert_eq!(p.base_delay_ms, 500);
        assert_eq!(p.max_delay_ms, 8000);
    }
}
