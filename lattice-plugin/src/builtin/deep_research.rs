use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepResearchInput {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub depth: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub claim: String,
    pub evidence: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepResearchOutput {
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub citations: Vec<String>,
    #[serde(default)]
    pub confidence: f64,
}

pub struct DeepResearchPlugin;

impl DeepResearchPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for DeepResearchPlugin {
    type Input = DeepResearchInput;
    type Output = DeepResearchOutput;

    fn name(&self) -> &str {
        "deep-research"
    }

    fn system_prompt(&self) -> &str {
        "You perform deep research. Synthesize information. Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Research: {}\nDepth: {}\nSources:\n{}",
            input.query,
            input.depth,
            input.sources.join("\n")
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
