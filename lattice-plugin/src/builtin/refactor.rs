use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

use super::code_review::CodeReviewOutput;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactorInput {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub review: Option<CodeReviewOutput>,
    #[serde(default)]
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub file: String,
    pub description: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactorOutput {
    #[serde(default)]
    pub refactored_code: String,
    #[serde(default)]
    pub changes: Vec<Change>,
}

pub struct RefactorPlugin;

impl RefactorPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for RefactorPlugin {
    type Input = RefactorInput;
    type Output = RefactorOutput;

    fn name(&self) -> &str {
        "refactor"
    }

    fn system_prompt(&self) -> &str {
        "You are an expert code refactoring engineer. Given code and review issues, \
         produce improved code and list each change. Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        let issues_text = match &input.review {
            Some(r) => r
                .issues
                .iter()
                .map(|i| {
                    format!(
                        "- [{}] {}:{} - {}",
                        i.severity, i.file, i.line, i.description
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            None => String::new(),
        };
        format!(
            "Refactor the following code. Fix all identified issues.\n\nCODE:\n{}\n\n\
             ISSUES TO FIX:\n{}\n\nADDITIONAL INSTRUCTIONS:\n{}\n\n\
             Return JSON: {{\"refactored_code\":\"...\", \"changes\":[{{\"file\":\"...\",\"description\":\"...\",\"before\":\"...\",\"after\":\"...\"}}]}}",
            input.code, issues_text, input.instructions
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
