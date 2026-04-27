use crate::errors::ArtemisError;
use std::time::Duration;

/// Classifies HTTP errors into retry decisions.
pub struct ErrorClassifier;

impl ErrorClassifier {
    pub fn classify(status_code: u16, body: &str, model: &str) -> ArtemisError {
        match status_code {
            429 => ArtemisError::RateLimit {
                provider: String::new(),
                retry_after: None,
            },
            401 | 403 => ArtemisError::Authentication {
                provider: String::new(),
            },
            404 => ArtemisError::ModelNotFound {
                model: model.to_string(),
            },
            500 | 502 | 503 => ArtemisError::ProviderUnavailable {
                provider: String::new(),
                reason: format!("HTTP {}", status_code),
            },
            _ => ArtemisError::Network {
                message: format!("HTTP {}: {}", status_code, body),
                status: Some(status_code),
            },
        }
    }

    pub fn is_retryable(error: &ArtemisError) -> bool {
        matches!(
            error,
            ArtemisError::RateLimit { .. } | ArtemisError::ProviderUnavailable { .. }
        )
    }
}

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
        let base = self.base_delay * 2u32.pow(attempt);
        let capped = std::cmp::min(base, self.max_delay);
        let jitter = rand::random::<f64>() * capped.as_secs_f64() * 0.5;
        Duration::from_secs_f64(capped.as_secs_f64() + jitter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_429() {
        let err = ErrorClassifier::classify(429, "{}", "gpt-4o");
        assert!(matches!(err, ArtemisError::RateLimit { .. }));
        assert!(ErrorClassifier::is_retryable(&err));
    }

    #[test]
    fn test_classify_404_model() {
        let err = ErrorClassifier::classify(404, "{}", "unknown-model");
        assert!(matches!(err, ArtemisError::ModelNotFound { model } if model == "unknown-model"));
    }

    #[test]
    fn test_classify_401_not_retryable() {
        let err = ErrorClassifier::classify(401, "{}", "gpt-4o");
        assert!(!ErrorClassifier::is_retryable(&err));
    }

    #[test]
    fn test_backoff_increases() {
        let policy = RetryPolicy::default();
        let d1 = policy.jittered_backoff(0);
        let d2 = policy.jittered_backoff(2);
        assert!(d2 > d1, "backoff should increase with attempts");
    }
}
