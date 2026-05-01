use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Whether a resolved model has usable credentials.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStatus {
    /// The provider requires credentials but none were found.
    #[default]
    Missing,
    /// An API key or token was found and is available.
    Present,
    /// The provider does not require credentials (e.g. Ollama running locally).
    NotRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ApiProtocol {
    OpenAiChat,
    AnthropicMessages,
    GeminiGenerateContent,
    CodexResponses,
    Custom(String),
}

impl serde::Serialize for ApiProtocol {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ApiProtocol::OpenAiChat => s.serialize_str("chat_completions"),
            ApiProtocol::AnthropicMessages => s.serialize_str("anthropic_messages"),
            ApiProtocol::GeminiGenerateContent => s.serialize_str("gemini_generate_content"),
            ApiProtocol::CodexResponses => s.serialize_str("codex_responses"),
            ApiProtocol::Custom(inner) => s.serialize_str(inner),
        }
    }
}

impl<'de> serde::Deserialize<'de> for ApiProtocol {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<ApiProtocol, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "chat_completions" => Ok(ApiProtocol::OpenAiChat),
            "anthropic_messages" | "anthropic" => Ok(ApiProtocol::AnthropicMessages),
            "gemini_generate_content" | "gemini" => Ok(ApiProtocol::GeminiGenerateContent),
            "codex_responses" | "codex" => Ok(ApiProtocol::CodexResponses),
            other => {
                let lower = other.to_lowercase();
                if lower.starts_with("chat")
                    || lower.starts_with("anthropic")
                    || lower.starts_with("gemini")
                    || lower.starts_with("codex")
                {
                    return Err(serde::de::Error::custom(format!(
                        "unknown protocol '{}': did you mean one of chat_completions, anthropic_messages, gemini_generate_content, codex_responses?",
                        other
                    )));
                }
                Ok(ApiProtocol::Custom(other.to_string()))
            }
        }
    }
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
    #[serde(default)]
    pub credential_status: CredentialStatus,
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
            .field("credential_status", &self.credential_status)
            .finish()
    }
}

impl ResolvedModel {
    /// Return a pseudo-unique identifier for this resolved model.
    pub fn model_id(&self) -> String {
        format!("{}@{}", self.canonical_id, self.provider)
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
    fn test_debug_hides_api_key() {
        let model = ResolvedModel {
            canonical_id: "test-model".to_string(),
            provider: "test-provider".to_string(),
            api_key: Some("secret-123".to_string()),
            base_url: "https://test.api.com".to_string(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "test-model-id".to_string(),
            context_length: 4096,
            provider_specific: HashMap::new(),
            credential_status: CredentialStatus::Present,
        };
        let debug_str = format!("{:?}", model);
        assert!(
            !debug_str.contains("secret-123"),
            "Debug output should not contain the actual API key"
        );
        assert!(
            debug_str.contains("***"),
            "Debug output should mask the API key with ***"
        );
    }

    #[test]
    fn test_api_protocol_serde_rejects_typo() {
        let result: Result<ApiProtocol, _> = serde_json::from_str(r#""chat_compltions""#);
        assert!(result.is_err(), "serde should reject typo 'chat_compltions'");
    }

    #[test]
    fn test_api_protocol_serde_accepts_short_alias() {
        let result: ApiProtocol = serde_json::from_str(r#""anthropic""#).unwrap();
        assert_eq!(result, ApiProtocol::AnthropicMessages, "serde should accept 'anthropic' as alias");
    }

    #[test]
    fn test_api_protocol_serde_custom_unknown() {
        let result: ApiProtocol = serde_json::from_str(r#""acp""#).unwrap();
        assert_eq!(result, ApiProtocol::Custom("acp".to_string()), "serde should accept truly unknown strings as Custom");
    }
}
