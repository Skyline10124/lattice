use artemis_core::types::ToolDefinition;
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Plugin trait — LLM does inference, Behavior controls decisions
// ---------------------------------------------------------------------------

/// A typed LLM function. The Plugin defines *what* the LLM should do;
/// the Behavior defines *how* to handle its output.
///
/// # Type parameters
/// - `I`: Input type (e.g., a diff, a file list)
/// - `O`: Output type (e.g., review issues, refactored code)
pub trait Plugin: Send + Sync {
    type Input: Serialize + DeserializeOwned + Send;
    type Output: Serialize + DeserializeOwned + Send;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// System prompt that defines the agent's identity and task.
    fn system_prompt(&self) -> &str;

    /// Format the typed input into a prompt string for the LLM.
    fn to_prompt(&self, input: &Self::Input) -> String;

    /// Parse the LLM's raw text response into the typed output.
    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError>;

    /// Tools this plugin may use.
    fn tools(&self) -> &[ToolDefinition] {
        &[]
    }

    /// Preferred model. Empty means "use the runner's default".
    fn preferred_model(&self) -> &str {
        ""
    }
}

// ---------------------------------------------------------------------------
// Behavior trait — how to handle output, errors, and handoffs
// ---------------------------------------------------------------------------

/// Controls what happens after the LLM produces output.
/// Separate from Plugin so the same plugin can run in different modes.
pub trait Behavior: Send + Sync {
    /// After receiving output, decide the next action.
    fn decide(&self, confidence: f64) -> Action;

    /// Handle a parse or validation error.
    fn on_error(&self, error: &PluginError, attempt: u32) -> ErrorAction;
}

/// What to do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Done — return the output.
    Done,
    /// Hand off to another plugin by name.
    Handoff(String),
    /// Retry the LLM call (with error feedback).
    Retry,
}

/// How to handle an error.
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorAction {
    /// Retry the LLM call.
    Retry,
    /// Stop and return the error.
    Abort,
    /// Hand off to a human for review.
    Escalate,
}

// ---------------------------------------------------------------------------
// Built-in behaviors
// ---------------------------------------------------------------------------

/// Strict: requires confidence >= threshold, escalates on persistent errors.
pub struct StrictBehavior {
    pub confidence_threshold: f64,
    pub max_retries: u32,
    pub escalate_to: Option<String>,
}

impl Default for StrictBehavior {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.7,
            max_retries: 3,
            escalate_to: None,
        }
    }
}

impl Behavior for StrictBehavior {
    fn decide(&self, confidence: f64) -> Action {
        if confidence >= self.confidence_threshold {
            Action::Done
        } else {
            Action::Retry
        }
    }

    fn on_error(&self, _error: &PluginError, attempt: u32) -> ErrorAction {
        if attempt < self.max_retries {
            ErrorAction::Retry
        } else if self.escalate_to.is_some() {
            ErrorAction::Escalate
        } else {
            ErrorAction::Abort
        }
    }
}

/// YOLO: trusts the LLM's output unconditionally. Never retries.
pub struct YoloBehavior;

impl Behavior for YoloBehavior {
    fn decide(&self, _confidence: f64) -> Action {
        Action::Done
    }

    fn on_error(&self, _error: &PluginError, _attempt: u32) -> ErrorAction {
        ErrorAction::Abort
    }
}

// ---------------------------------------------------------------------------
// PluginRunner — ties Plugin + Behavior + Agent together
// ---------------------------------------------------------------------------

use std::marker::PhantomData;

/// Runs a Plugin with a given Behavior against an Agent.
pub struct PluginRunner<'a, P: Plugin + ?Sized, B: Behavior, A: PluginAgent> {
    plugin: &'a P,
    behavior: &'a B,
    agent: &'a mut A,
    _phantom: PhantomData<(P::Input, P::Output)>,
}

/// Result of running a plugin.
pub struct RunResult {
    /// JSON-serialized output.
    pub output: String,
    /// Number of LLM calls made (including retries).
    pub turns: u32,
    /// The action taken on the final turn.
    pub final_action: Action,
}

impl<'a, P: Plugin + ?Sized, B: Behavior, A: PluginAgent> PluginRunner<'a, P, B, A> {
    pub fn new(plugin: &'a P, behavior: &'a B, agent: &'a mut A) -> Self {
        Self {
            plugin,
            behavior,
            agent,
            _phantom: PhantomData,
        }
    }

    /// Run the plugin: to_prompt → LLM → parse → behavior.decide.
    /// Retries and handoffs are handled by the Behavior.
    pub fn run(&mut self, input: &P::Input) -> Result<RunResult, PluginError> {
        let prompt = self.plugin.to_prompt(input);
        let mut attempt = 0u32;

        loop {
            let raw = self
                .agent
                .send(&prompt)
                .map_err(|e| PluginError::Other(e.to_string()))?;

            match self.plugin.parse_output(&raw) {
                Ok(output) => {
                    let confidence = extract_confidence(&raw);
                    match self.behavior.decide(confidence) {
                        Action::Done => {
                            let json = serde_json::to_string(&output)
                                .map_err(|e| PluginError::Other(e.to_string()))?;
                            return Ok(RunResult {
                                output: json,
                                turns: attempt + 1,
                                final_action: Action::Done,
                            });
                        }
                        Action::Handoff(_target) => {
                            let json = serde_json::to_string(&output)
                                .map_err(|e| PluginError::Other(e.to_string()))?;
                            return Ok(RunResult {
                                output: json,
                                turns: attempt + 1,
                                final_action: Action::Handoff(_target),
                            });
                        }
                        Action::Retry => {
                            attempt += 1;
                            continue;
                        }
                    }
                }
                Err(e) => match self.behavior.on_error(&e, attempt) {
                    ErrorAction::Retry => {
                        attempt += 1;
                        continue;
                    }
                    ErrorAction::Abort => return Err(e),
                    ErrorAction::Escalate => {
                        return Err(PluginError::Escalated {
                            original: Box::new(e),
                            after_attempts: attempt,
                        });
                    }
                },
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Agent abstraction
// ---------------------------------------------------------------------------

/// Minimal interface for an LLM-calling agent.
pub trait PluginAgent {
    fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Missing tool: {0}")]
    MissingTool(String),

    #[error("Escalated after {after_attempts} attempts: {original}")]
    Escalated {
        original: Box<PluginError>,
        after_attempts: u32,
    },

    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Try to extract a confidence score from the LLM's raw response.
/// Falls back to 1.0 if not found (assumes single-pass outputs are confident).
fn extract_confidence(raw: &str) -> f64 {
    // Look for "confidence": 0.85 or similar JSON field
    for line in raw.lines() {
        if let Some(pos) = line.find("\"confidence\"") {
            let after = &line[pos + 12..];
            if let Some(colon) = after.find(':') {
                let val = after[colon + 1..].trim().trim_end_matches(',');
                if let Ok(f) = val.parse::<f64>() {
                    return f.clamp(0.0, 1.0);
                }
            }
        }
    }
    1.0 // default: trust the output
}

// ---------------------------------------------------------------------------
// Built-in: CodeReview plugin
// ---------------------------------------------------------------------------

pub struct CodeReviewPlugin;

impl CodeReviewPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for CodeReviewPlugin {
    type Input = serde_json::Value;
    type Output = serde_json::Value;

    fn name(&self) -> &str {
        "code-review"
    }

    fn system_prompt(&self) -> &str {
        "You are a senior code reviewer. Review the provided diff for correctness, \
         security, and design issues. Return a JSON object with an 'issues' array. \
         Each issue has: severity (critical/high/medium/low), file, line, description. \
         Include a 'confidence' field (0.0-1.0) indicating how confident you are \
         in this review. Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        serde_json::to_string(input).unwrap_or_default()
    }

    fn parse_output(&self, raw: &str) -> Result<serde_json::Value, PluginError> {
        let trimmed = raw.trim();
        let json_str = if let Some(start) = trimmed.find("```json") {
            let after = &trimmed[start + 7..];
            after.split("```").next().unwrap_or(trimmed)
        } else if trimmed.starts_with('{') {
            trimmed
        } else {
            return Err(PluginError::Parse("Response does not contain JSON".into()));
        };
        serde_json::from_str(json_str).map_err(|e| PluginError::Parse(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strict_behavior_retries_low_confidence() {
        let b = StrictBehavior {
            confidence_threshold: 0.8,
            ..Default::default()
        };
        assert_eq!(b.decide(0.5), Action::Retry);
        assert_eq!(b.decide(0.9), Action::Done);
    }

    #[test]
    fn test_yolo_always_done() {
        let b = YoloBehavior;
        assert_eq!(b.decide(0.1), Action::Done);
    }

    #[test]
    fn test_strict_escalates_after_retries() {
        let b = StrictBehavior {
            max_retries: 2,
            escalate_to: Some("human".into()),
            ..Default::default()
        };
        assert_eq!(
            b.on_error(&PluginError::Parse("x".into()), 0),
            ErrorAction::Retry
        );
        assert_eq!(
            b.on_error(&PluginError::Parse("x".into()), 2),
            ErrorAction::Escalate
        );
    }

    #[test]
    fn test_code_review_parse_json() {
        let p = CodeReviewPlugin::new();
        let raw = r#"{"issues":[],"confidence":0.9}"#;
        let out = p.parse_output(raw).unwrap();
        assert_eq!(out["confidence"].as_f64().unwrap(), 0.9);
    }

    #[test]
    fn test_code_review_parse_markdown() {
        let p = CodeReviewPlugin::new();
        let raw = "```json\n{\"issues\":[],\"confidence\":0.8}\n```";
        let out = p.parse_output(raw).unwrap();
        assert_eq!(out["confidence"].as_f64().unwrap(), 0.8);
    }

    #[test]
    fn test_extract_confidence() {
        assert!((extract_confidence("{\"confidence\":0.85}") - 0.85).abs() < 0.01);
        assert!((extract_confidence("no confidence field") - 1.0).abs() < 0.01);
    }
}
