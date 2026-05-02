use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseInput {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub kb_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseResult {
    pub title: String,
    pub snippet: String,
    pub relevance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseOutput {
    #[serde(default)]
    pub results: Vec<KnowledgeBaseResult>,
    #[serde(default)]
    pub relevance_scores: Vec<f64>,
}

pub struct KnowledgeBasePlugin;

impl KnowledgeBasePlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for KnowledgeBasePlugin {
    type Input = KnowledgeBaseInput;
    type Output = KnowledgeBaseOutput;

    fn name(&self) -> &str {
        "knowledge-base"
    }

    fn system_prompt(&self) -> &str {
        "You query knowledge bases. Synthesize results. Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Query: {}\nSources:\n{}",
            input.query,
            input.kb_sources.join("\n")
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
