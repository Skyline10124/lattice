use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ApiProtocol {
    #[serde(rename = "chat_completions")]
    OpenAiChat,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    #[serde(rename = "gemini_generate_content")]
    GeminiGenerateContent,
    #[serde(rename = "codex_responses")]
    CodexResponses,
    #[serde(untagged)]
    Custom(String),
}

impl std::str::FromStr for ApiProtocol {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "chat_completions" => ApiProtocol::OpenAiChat,
            "anthropic_messages" | "anthropic" => ApiProtocol::AnthropicMessages,
            "gemini_generate_content" | "gemini" => ApiProtocol::GeminiGenerateContent,
            "codex_responses" | "codex" => ApiProtocol::CodexResponses,
            other => ApiProtocol::Custom(other.to_string()),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogProviderEntry {
    pub provider_id: String,
    pub api_model_id: String,
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default)]
    pub credential_keys: HashMap<String, String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_api_protocol")]
    pub api_protocol: ApiProtocol,
    #[serde(default)]
    pub provider_specific: HashMap<String, String>,
}

fn default_priority() -> u32 {
    1
}
fn default_api_protocol() -> ApiProtocol {
    ApiProtocol::OpenAiChat
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    pub canonical_id: String,
    #[serde(default)]
    pub context_length: u32,
    pub providers: Vec<CatalogProviderEntry>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderDefaults {
    pub api_protocol: ApiProtocol,
    #[serde(default)]
    pub credential_keys: HashMap<String, String>,
    #[serde(default)]
    pub base_url: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ResolvedModel {
    pub canonical_id: String,
    pub provider: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: String,
    pub api_protocol: ApiProtocol,
    pub api_model_id: String,
    #[serde(default)]
    pub context_length: u32,
    #[serde(default)]
    pub provider_specific: HashMap<String, String>,
}

impl std::fmt::Debug for ResolvedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedModel")
            .field("canonical_id", &self.canonical_id)
            .field("provider", &self.provider)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("base_url", &self.base_url)
            .field("api_protocol", &self.api_protocol)
            .field("api_model_id", &self.api_model_id)
            .field("context_length", &self.context_length)
            .field("provider_specific", &self.provider_specific)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogData {
    pub models: Vec<ModelCatalogEntry>,
    pub aliases: HashMap<String, String>,
    pub provider_defaults: HashMap<String, ProviderDefaults>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolved_model_debug_masks_api_key() {
        let model = ResolvedModel {
            canonical_id: "test-model".to_string(),
            provider: "test-provider".to_string(),
            api_key: Some("sk-real-key-12345".to_string()),
            base_url: "https://api.example.com".to_string(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "test-model-id".to_string(),
            context_length: 4096,
            provider_specific: HashMap::new(),
        };
        let debug_str = format!("{:?}", model);
        assert!(
            !debug_str.contains("sk-real-key-12345"),
            "Debug should not contain real api_key"
        );
        assert!(debug_str.contains("***"), "Debug should mask api_key");
    }
}
