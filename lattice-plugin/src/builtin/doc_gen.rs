use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocGenInput {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub doc_type: String,
    #[serde(default)]
    pub audience: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocGenOutput {
    #[serde(default)]
    pub documentation: String,
    #[serde(default)]
    pub sections: Vec<String>,
}

pub struct DocGenPlugin;

impl DocGenPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for DocGenPlugin {
    type Input = DocGenInput;
    type Output = DocGenOutput;

    fn name(&self) -> &str {
        "doc-gen"
    }

    fn system_prompt(&self) -> &str {
        "You generate technical documentation. Return ONLY valid JSON with 'documentation' and 'sections'."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Generate {} documentation for {}.\nCODE:\n{}",
            input.doc_type, input.audience, input.code
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
