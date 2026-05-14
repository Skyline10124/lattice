use crate::catalog::Catalog;
use crate::types::Message;

pub struct TokenEstimator;

impl TokenEstimator {
    fn is_openai_model(model_id: &str) -> bool {
        let lower = model_id.to_lowercase();
        lower.starts_with("gpt-")
            || lower.starts_with("gpt-4o")
            || lower.starts_with("gpt-5")
            || lower == "o1"
            || lower.starts_with("o1-")
            || lower == "o3"
            || lower.starts_with("o3-")
            || lower == "o4"
            || lower.starts_with("o4-")
    }

    fn tiktoken_count(text: &str, model_id: &str) -> Option<u32> {
        let bpe = if Self::is_openai_model(model_id) {
            let lower = model_id.to_lowercase();
            if lower.starts_with("gpt-5")
                || lower.starts_with("gpt-4o")
                || lower.starts_with("gpt-4.1")
                || lower == "o1"
                || lower.starts_with("o1-")
                || lower == "o3"
                || lower.starts_with("o3-")
                || lower == "o4"
                || lower.starts_with("o4-")
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
            .map(|m| {
                let base = Self::estimate_text_for_model(&m.content, model_id);
                let tool_calls_estimate = m
                    .tool_calls
                    .as_ref()
                    .map(|tcs| {
                        tcs.iter()
                            .map(|tc| {
                                Self::estimate_text_for_model(
                                    &format!(
                                        "{} {} {}",
                                        tc.id, tc.function.name, tc.function.arguments
                                    ),
                                    model_id,
                                )
                            })
                            .sum::<u32>()
                    })
                    .unwrap_or(0);
                let reasoning_estimate = m
                    .reasoning_content
                    .as_ref()
                    .map(|r| Self::estimate_text_for_model(r, model_id))
                    .unwrap_or(0);
                let name_estimate = m
                    .name
                    .as_ref()
                    .map(|n| Self::estimate_text_for_model(n, model_id))
                    .unwrap_or(0);
                base + tool_calls_estimate + reasoning_estimate + name_estimate
            })
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
    use crate::types::{FunctionCall, ToolCall};

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

    #[test]
    fn test_estimate_messages_includes_tool_calls() {
        let msgs_with_tools = vec![Message {
            role: Role::Assistant,
            content: "Let me check".to_string(),
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_123".to_string(),
                function: FunctionCall {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city": "Tokyo", "unit": "celsius"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        }];
        let msgs_without = vec![Message {
            role: Role::Assistant,
            content: "Let me check".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        let with_tools =
            TokenEstimator::estimate_messages_for_model(&msgs_with_tools, "claude-sonnet-4-6");
        let without =
            TokenEstimator::estimate_messages_for_model(&msgs_without, "claude-sonnet-4-6");
        assert!(
            with_tools > without,
            "tool_calls should add token estimate: with={} vs without={}",
            with_tools,
            without
        );
    }
}
