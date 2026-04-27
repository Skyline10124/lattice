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
    #[serde(rename = "bedrock_converse")]
    BedrockConverse,
    #[serde(rename = "codex_responses")]
    CodexResponses,
    #[serde(untagged)]
    Custom(String),
}

impl ApiProtocol {
    pub fn from_str(s: &str) -> Self {
        match s {
            "chat_completions" => ApiProtocol::OpenAiChat,
            "anthropic_messages" | "anthropic" => ApiProtocol::AnthropicMessages,
            "gemini_generate_content" | "gemini" => ApiProtocol::GeminiGenerateContent,
            "bedrock_converse" | "bedrock" => ApiProtocol::BedrockConverse,
            "codex_responses" | "codex" => ApiProtocol::CodexResponses,
            other => ApiProtocol::Custom(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogProviderEntry {
    pub provider_id: String,
    pub api_model_id: String,
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default)]
    pub credential_keys: HashMap<String, String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_api_protocol")]
    pub api_protocol: ApiProtocol,
    #[serde(default)]
    pub provider_specific: HashMap<String, String>,
}

fn default_priority() -> u32 { 1 }
fn default_weight() -> u32 { 1 }
fn default_api_protocol() -> ApiProtocol { ApiProtocol::OpenAiChat }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    pub canonical_id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub context_length: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogData {
    pub models: Vec<ModelCatalogEntry>,
    pub aliases: HashMap<String, String>,
    pub provider_defaults: HashMap<String, ProviderDefaults>,
}
