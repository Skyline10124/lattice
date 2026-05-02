use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeReviewInput {
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub context_rules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub severity: String,
    pub file: String,
    pub line: u32,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeReviewOutput {
    #[serde(default)]
    pub issues: Vec<Issue>,
    #[serde(default)]
    pub confidence: f64,
}

pub struct CodeReviewPlugin;

impl CodeReviewPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CodeReviewPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for CodeReviewPlugin {
    type Input = CodeReviewInput;
    type Output = CodeReviewOutput;

    fn name(&self) -> &str {
        "code-review"
    }

    fn system_prompt(&self) -> &str {
        "You are a senior code reviewer. Review the provided code for bugs, \
         security issues, and design problems. Return a JSON object with an \
         'issues' array and a 'confidence' field (0.0-1.0). Each issue: \
         severity (critical/high/medium/low), file, line, description. Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        format!(
            "Please review the following code for bugs, security issues, and design problems.\n\n\
             Return a JSON object with an 'issues' array and a 'confidence' field (0.0-1.0).\n\
             Each issue: severity, file, line, description.\n\nCODE TO REVIEW:\n{}",
            input.input
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
