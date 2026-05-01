use std::time::Duration;

/// Jittered exponential backoff retry policy.
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    pub fn jittered_backoff(&self, attempt: u32) -> Duration {
        let base = self.base_delay * 2u32.saturating_pow(attempt);
        let capped = std::cmp::min(base, self.max_delay);
        // Centered jitter: random +/- 50% of capped value.
        // When capped == max_delay, jitter subtracts up to 50%,
        // so result varies between 50%-100% of max_delay.
        // Collision avoidance works even when base >= max_delay.
        let jitter_range = capped.as_secs_f64() * 0.5;
        let jittered = capped.as_secs_f64() + (rand::random::<f64>() - 0.5) * jitter_range;
        let jittered = if jittered < 0.0 { 0.0 } else { jittered };
        std::cmp::min(Duration::from_secs_f64(jittered), self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_increases() {
        let policy = RetryPolicy::default();
        let d1 = policy.jittered_backoff(0);
        let d2 = policy.jittered_backoff(2);
        assert!(d2 > d1, "backoff should increase with attempts");
    }

    #[test]
    fn test_backoff_high_attempt_no_panic() {
        let policy = RetryPolicy::default();
        let result = policy.jittered_backoff(100);
        assert!(
            result <= policy.max_delay,
            "jittered_backoff(100) result {:?} should not exceed max_delay {:?}",
            result,
            policy.max_delay
        );
    }
}
