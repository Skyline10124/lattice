use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestGenInput {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub focus_areas: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestGenOutput {
    #[serde(default)]
    pub tests: String,
    #[serde(default)]
    pub coverage_estimate: f64,
}

pub struct TestGenPlugin;

impl TestGenPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for TestGenPlugin {
    type Input = TestGenInput;
    type Output = TestGenOutput;

    fn name(&self) -> &str {
        "test-gen"
    }

    fn system_prompt(&self) -> &str {
        "You are an expert test engineer. Generate comprehensive tests. \
         Return ONLY valid JSON with 'tests' and 'coverage_estimate' (0.0-1.0)."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        let focus = if input.focus_areas.is_empty() {
            String::from("general coverage")
        } else {
            input.focus_areas.join(", ")
        };
        format!(
            "Generate tests for {} code. Focus: {}.\n\nCODE:\n{}",
            input.language, focus, input.code
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
