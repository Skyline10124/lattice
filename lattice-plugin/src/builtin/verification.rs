use serde::{Deserialize, Serialize};

use crate::{Plugin, PluginError};

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    #[serde(default)]
    pub before_summary: String,
    #[serde(default)]
    pub after_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationInput {
    #[serde(default)]
    pub changes: Vec<FileChange>,
    #[serde(default)]
    pub plan_task_id: u32,
    #[serde(default)]
    pub verification_steps: Vec<String>,
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationIssue {
    pub severity: String,
    pub file: String,
    pub line: u32,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationOutput {
    pub passed: bool,
    #[serde(default)]
    pub failed_steps: Vec<String>,
    #[serde(default)]
    pub issues: Vec<VerificationIssue>,
    #[serde(default)]
    pub rollback_advice: Option<String>,
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct VerificationPlugin;

impl VerificationPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for VerificationPlugin {
    type Input = VerificationInput;
    type Output = VerificationOutput;

    fn name(&self) -> &str {
        "verification"
    }

    fn system_prompt(&self) -> &str {
        "You are a verification agent. Given a list of file changes and \
         verification steps, determine whether each step passes or fails. \
         Return a JSON object with 'passed' (bool), 'failed_steps' (array of \
         step descriptions that did not pass), 'issues' (array of objects with \
         severity, file, line, description), and 'rollback_advice' (optional \
         string describing how to roll back if the changes cannot be verified). \
         Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        let changes_str = input
            .changes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "  {}. path: {}\n     before: {}\n     after: {}",
                    i + 1,
                    c.path,
                    c.before_summary,
                    c.after_summary
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let steps_str = input
            .verification_steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("  {}. {}", i + 1, s))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "Verify the following changes against these verification steps.\n\n\
             Plan task ID: {}\n\n\
             CHANGES:\n{}\n\n\
             VERIFICATION STEPS:\n{}\n\n\
             Evaluate each step carefully and determine whether the changes pass. \
             If any step fails, include it in 'failed_steps' and describe the issue. \
             Provide rollback advice if the changes cannot be safely verified.",
            input.plan_task_id, changes_str, steps_str
        )
    }

    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
        let json = super::parse_utils::parse_json_from_response(raw)?;
        serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
