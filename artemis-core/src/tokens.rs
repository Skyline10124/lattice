use crate::catalog::Catalog;
use crate::types::Message;

pub struct TokenEstimator;

impl TokenEstimator {
    /// Rough token count: ~4 chars per token for non-OpenAI models.
    pub fn estimate_text(text: &str) -> u32 {
        (text.len() as u32).div_ceil(4)
    }

    /// Estimate tokens for a list of messages.
    pub fn estimate_messages(messages: &[Message]) -> u32 {
        messages.iter().map(|m| Self::estimate_text(&m.content)).sum()
    }

    /// Check if messages fit within a model's context window.
    pub fn fits_in_context(messages: &[Message], model_id: &str) -> bool {
        let catalog = Catalog::get();
        let estimated = Self::estimate_messages(messages);
        if let Some(entry) = catalog.get_model(model_id) {
            entry.context_length == 0 || estimated < entry.context_length
        } else {
            estimated < 131072 // default
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
    fn test_fits_in_context() {
        let msgs = vec![Message {
            role: Role::User,
            content: "hello".to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        assert!(TokenEstimator::fits_in_context(&msgs, "gpt-4o"));
    }
}
