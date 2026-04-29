use artemis_core::types::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Plugin trait
// ---------------------------------------------------------------------------

/// A plugin is a typed LLM function — Input → prompt → LLM → parse → Output.
///
/// Plugins are **composable**: the output of one plugin can become the input of
/// the next, forming an agent pipeline controlled by code (not the LLM).
///
/// # Example
///
/// ```ignore
/// let code_review = CodeReviewPlugin::new();
/// let diff = fs::read_to_string("changes.diff")?;
/// let input = serde_json::to_value(ReviewInput { diff })?;
/// let output: ReviewOutput = code_review.run(&agent, &input)?;
/// if code_review.should_handoff(&output) {
///     let refactor = RefactorPlugin::new();
///     refactor.run(&agent, &output.issues)?;
/// }
/// ```
pub trait Plugin: Send + Sync {
    /// Human-readable name of this plugin.
    fn name(&self) -> &str;

    /// System prompt that defines the agent's identity and task.
    fn system_prompt(&self) -> &str;

    /// Tools this plugin can use. Return `&[]` if no tools are needed.
    fn tools(&self) -> &[ToolDefinition];

    /// Preferred model for this plugin's task (e.g., `"deepseek-v4-pro"`).
    /// Falls back to the Agent's model if empty.
    fn preferred_model(&self) -> &str {
        ""
    }

    // ---- lifecycle hooks ----

    /// Build the input value before sending to the LLM.
    /// The returned JSON value is serialized as the user message content.
    /// Default: passes through the raw input unchanged.
    fn build_input(&self, raw_input: &Value) -> Result<Value, PluginError> {
        Ok(raw_input.clone())
    }

    /// Parse and validate the LLM's response into structured output.
    fn parse_output(&self, raw_response: &str) -> Result<Value, PluginError>;

    /// Validate the parsed output. Return `Ok(())` if the output is acceptable.
    /// Return `Err` to trigger a retry or fallback.
    fn validate_output(&self, _output: &Value) -> Result<(), PluginError> {
        Ok(())
    }

    /// After receiving the model's response, decide what to do next.
    /// - `None` → the plugin is done, return the output.
    /// - `Some(name)` → hand off to the named plugin with this output as its input.
    fn should_handoff(&self, _output: &Value) -> Option<String> {
        None
    }

    /// Called when parse_output fails. The plugin can decide to retry or abort.
    /// Default: retry once, then give up.
    fn on_parse_error(&self, error: &PluginError, _attempt: u32) -> PluginErrorAction {
        if _attempt < 1 {
            PluginErrorAction::Retry
        } else {
            PluginErrorAction::Abort
        }
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors that can occur during plugin execution.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// The LLM's output could not be parsed into the expected format.
    #[error("Parse error: {0}")]
    Parse(String),

    /// The parsed output failed validation.
    #[error("Validation error: {0}")]
    Validation(String),

    /// A required tool was not available.
    #[error("Missing tool: {0}")]
    MissingTool(String),

    /// An unexpected error occurred.
    #[error("Plugin error: {0}")]
    Other(String),
}

/// What to do when parse_output fails.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginErrorAction {
    /// Retry the LLM call (the error message is fed back to the model).
    Retry,
    /// Stop the plugin and return the error.
    Abort,
}

// ---------------------------------------------------------------------------
// Plugin runner — ties a Plugin to an artemis Agent
// ---------------------------------------------------------------------------

/// Runs a Plugin by delegating to the artemis Agent for model calls.
pub struct PluginRunner<'a, A> {
    plugin: &'a dyn Plugin,
    agent: &'a mut A,
}

impl<'a, A> PluginRunner<'a, A>
where
    A: PluginAgent,
{
    pub fn new(plugin: &'a dyn Plugin, agent: &'a mut A) -> Self {
        Self { plugin, agent }
    }

    /// Execute the plugin: send input → get response → parse → validate.
    /// Retries on parse/validation errors according to the plugin's
    /// `on_parse_error` policy.
    pub fn run(&mut self, input: &Value) -> Result<Value, PluginError> {
        let formatted = self.plugin.build_input(input)?;
        let prompt = serde_json::to_string(&formatted)
            .unwrap_or_else(|_| formatted.to_string());

        let mut attempt = 0u32;
        loop {
            let raw = self
                .agent
                .send(&prompt)
                .map_err(|e| PluginError::Other(e.to_string()))?;

            match self.plugin.parse_output(&raw) {
                Ok(output) => {
                    self.plugin.validate_output(&output)?;
                    return Ok(output);
                }
                Err(e) => match self.plugin.on_parse_error(&e, attempt) {
                    PluginErrorAction::Retry => {
                        attempt += 1;
                        continue;
                    }
                    PluginErrorAction::Abort => return Err(e),
                },
            }
        }
    }

    /// Run the plugin and check for handoff.
    pub fn run_with_handoff(&mut self, input: &Value) -> Result<RunResult, PluginError> {
        let output = self.run(input)?;
        let handoff = self.plugin.should_handoff(&output);
        Ok(RunResult { output, handoff })
    }
}

/// Result of a plugin run, including optional handoff target.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub output: Value,
    pub handoff: Option<String>,
}

// ---------------------------------------------------------------------------
// Agent abstraction — any type that can call an LLM
// ---------------------------------------------------------------------------

/// Minimal interface that a PluginRunner needs from its underlying agent.
/// artemis_agent::Agent implements this trait.
pub trait PluginAgent {
    /// Send a message to the model and return the text response.
    fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;
}

// ---------------------------------------------------------------------------
// Built-in: CodeReview plugin
// ---------------------------------------------------------------------------

/// A simple code review plugin — reads a diff, finds issues.
pub struct CodeReviewPlugin;

impl CodeReviewPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for CodeReviewPlugin {
    fn name(&self) -> &str {
        "code-review"
    }

    fn system_prompt(&self) -> &str {
        "You are a senior code reviewer. Review the diff for correctness, security, and design issues. \
         Return a JSON object with an 'issues' array: each issue has 'severity' (critical/high/medium/low), \
         'file', 'line', and 'description'. Return ONLY valid JSON, no other text."
    }

    fn tools(&self) -> &[ToolDefinition] {
        &[]
    }

    fn parse_output(&self, raw: &str) -> Result<Value, PluginError> {
        let trimmed = raw.trim();
        // Try to extract JSON from markdown code blocks or raw text
        let json_str = if let Some(start) = trimmed.find("```json") {
            let after = &trimmed[start + 7..];
            after.split("```").next().unwrap_or(trimmed)
        } else if trimmed.starts_with('{') {
            trimmed
        } else {
            return Err(PluginError::Parse("Response does not contain JSON".into()));
        };

        serde_json::from_str(json_str)
            .map_err(|e| PluginError::Parse(format!("Invalid JSON: {}", e)))
    }

    fn validate_output(&self, output: &Value) -> Result<(), PluginError> {
        let issues = output
            .get("issues")
            .and_then(|i| i.as_array())
            .ok_or_else(|| PluginError::Validation("Missing 'issues' array".into()))?;

        if issues.is_empty() {
            return Err(PluginError::Validation(
                "No issues found — response may be incomplete".into(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_review_plugin_valid_json() {
        let plugin = CodeReviewPlugin::new();
        let raw = r#"{"issues":[{"severity":"high","file":"lib.rs","line":42,"description":"bug"}]}"#;
        let result = plugin.parse_output(raw).unwrap();
        assert_eq!(result["issues"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_code_review_plugin_markdown_json() {
        let plugin = CodeReviewPlugin::new();
        let raw = "Here is the review:\n```json\n{\"issues\":[{\"severity\":\"low\",\"file\":\"a.rs\",\"line\":1,\"description\":\"nit\"}]}\n```";
        let result = plugin.parse_output(raw).unwrap();
        assert_eq!(result["issues"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_code_review_plugin_invalid_json() {
        let plugin = CodeReviewPlugin::new();
        assert!(plugin.parse_output("not json at all").is_err());
    }

    #[test]
    fn test_code_review_plugin_empty_issues_fails_validation() {
        let plugin = CodeReviewPlugin::new();
        let output = serde_json::json!({"issues": []});
        assert!(plugin.validate_output(&output).is_err());
    }
}
