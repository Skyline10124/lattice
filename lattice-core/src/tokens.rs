use crate::catalog::Catalog;
use crate::types::Message;

pub struct TokenEstimator;

impl TokenEstimator {
    fn is_openai_model(model_id: &str) -> bool {
        let lower = model_id.to_lowercase();
        lower.starts_with("gpt-")
            || lower.starts_with("o1")
            || lower.starts_with("o3")
            || lower.starts_with("o4")
            || lower.contains("gpt-4o")
    }

    fn tiktoken_count(text: &str, model_id: &str) -> Option<u32> {
        let bpe = if Self::is_openai_model(model_id) {
            let lower = model_id.to_lowercase();
            if lower.starts_with("gpt-5")
                || lower.starts_with("gpt-4o")
                || lower.starts_with("gpt-4.1")
                || lower.starts_with("o1")
                || lower.starts_with("o3")
                || lower.starts_with("o4")
            {
                Some(tiktoken_rs::o200k_base_singleton())
            } else {
                Some(tiktoken_rs::cl100k_base_singleton())
            }
        } else {
            None
        };

        bpe.map(|bpe| bpe.encode_ordinary(text).len() as u32)
    }

    pub fn estimate_text(text: &str) -> u32 {
        Self::estimate_text_for_model(text, "")
    }

    pub fn estimate_text_for_model(text: &str, model_id: &str) -> u32 {
        Self::tiktoken_count(text, model_id).unwrap_or_else(|| (text.len() as u32).div_ceil(4))
    }

    pub fn estimate_messages(messages: &[Message]) -> u32 {
        Self::estimate_messages_for_model(messages, "")
    }

    pub fn estimate_messages_for_model(messages: &[Message], model_id: &str) -> u32 {
        messages
            .iter()
            .map(|m| Self::estimate_text_for_model(&m.content, model_id))
            .sum()
    }

    pub fn fits_in_context(messages: &[Message], model_id: &str) -> bool {
        let estimated = Self::estimate_messages_for_model(messages, model_id);
        match Catalog::get() {
            Ok(catalog) => {
                if let Some(entry) = catalog.get_model(model_id) {
                    // Reserve a 5% safety margin from the context window.
                    // Providers reject requests at the exact limit.
                    let safe_limit = if entry.context_length > 100 {
                        entry.context_length - (entry.context_length / 20)
                    } else {
                        entry.context_length // Small contexts: exact limit
                    };
                    entry.context_length == 0 || estimated < safe_limit
                } else {
                    estimated < 124416
                }
            }
            Err(_) => estimated < 124416,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;

    #[test]
    fn test_estimate_empty() {
        assert_eq!(TokenEstimator::estimate_text(""), 0);
    }

    #[test]
    fn test_estimate_short() {
        assert_eq!(TokenEstimator::estimate_text("hello"), 2);
    }

    #[test]
    fn test_tiktoken_openai_model() {
        let count = TokenEstimator::estimate_text_for_model("hello world", "gpt-4o");
        assert!(count > 0, "tiktoken should return >0 for gpt-4o");
        assert!(count < 10, "tiktoken count for short text should be small");
    }

    #[test]
    fn test_tiktoken_fallback_non_openai() {
        let rough = TokenEstimator::estimate_text_for_model("hello world", "claude-sonnet-4-6");
        let expected = ("hello world".len() as u32).div_ceil(4);
        assert_eq!(rough, expected, "non-OpenAI should use rough estimation");
    }

    #[test]
    fn test_fits_in_context() {
        let msgs = vec![Message {
            role: Role::User,
            content: "hello".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        assert!(TokenEstimator::fits_in_context(&msgs, "gpt-4o"));
    }
}
