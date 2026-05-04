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
    fn test_backoff_increases_median() {
        let policy = RetryPolicy::default();
        // With ±50% jitter, attempt=0 p50 ≈ 1s, attempt=2 p50 ≈ 4s.
        // Take 1000 samples each and compare medians — medians should
        // respect the exponential growth even with random jitter.
        let mut d1_samples: Vec<u64> = Vec::new();
        let mut d2_samples: Vec<u64> = Vec::new();
        for _ in 0..1000 {
            d1_samples.push(policy.jittered_backoff(0).as_millis() as u64);
            d2_samples.push(policy.jittered_backoff(2).as_millis() as u64);
        }
        d1_samples.sort();
        d2_samples.sort();
        let median_d1 = d1_samples[500];
        let median_d2 = d2_samples[500];
        assert!(
            median_d2 > median_d1,
            "median backoff at attempt=2 ({:?}ms) should exceed attempt=0 ({:?}ms)",
            median_d2,
            median_d1
        );
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
