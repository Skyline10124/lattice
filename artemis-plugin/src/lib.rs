use artemis_core::retry::RetryPolicy;
use artemis_core::streaming::TokenUsage;
use artemis_core::types::{Message, Role, ToolDefinition};
use artemis_memory::Memory;
use artemis_token_pool::TokenPool;
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
// PluginHooks — lifecycle observability
// ---------------------------------------------------------------------------

/// Hooks into the PluginRunner lifecycle for logging, metrics, and tracing.
///
/// All methods have default no-op implementations so users only override
/// the hooks they care about.
pub trait PluginHooks: Send + Sync {
    /// Called before the first LLM call.
    fn on_start(&self, _plugin: &str, _input_tokens: u32) {}

    /// Called after each LLM response is parsed and an action is decided.
    fn on_turn(&self, _attempt: u32, _tokens: Option<TokenUsage>, _action: &Action) {}

    /// Called when a parse error occurs.
    fn on_error(&self, _attempt: u32, _error: &PluginError) {}

    /// Called when the plugin run completes (successfully or with a handoff).
    fn on_complete(&self, _result: &RunResult) {}
}

// ---------------------------------------------------------------------------
// PluginConfig — safety parameters
// ---------------------------------------------------------------------------

/// Configuration for a PluginRunner run.
#[derive(Debug, Clone, Copy)]
pub struct PluginConfig {
    /// Maximum number of LLM calls (including retries). Default: 10.
    pub max_turns: u32,
    /// Maximum output size in bytes. Default: 1 MB.
    pub max_output_bytes: usize,
    /// Reserved for future use: whether to check context length before sending. Default: true.
    pub context_check: bool,
    /// Reserved for future use: timeout per LLM call in seconds. Default: 120.
    pub timeout_per_call_secs: u64,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            max_turns: 10,
            max_output_bytes: 1_048_576,
            context_check: true,
            timeout_per_call_secs: 120,
        }
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
    config: &'a PluginConfig,
    hooks: Option<&'a dyn PluginHooks>,
    retry_policy: Option<&'a RetryPolicy>,
    memory: Option<Box<dyn Memory>>,
    /// Reserved for future budget enforcement.
    #[allow(dead_code)]
    token_pool: Option<&'a dyn TokenPool>,
    _phantom: PhantomData<(P::Input, P::Output)>,
}

/// Result of running a plugin.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// JSON-serialized output.
    pub output: String,
    /// Number of LLM calls made (including retries).
    pub turns: u32,
    /// The action taken on the final turn.
    pub final_action: Action,
}

impl<'a, P: Plugin + ?Sized, B: Behavior, A: PluginAgent> PluginRunner<'a, P, B, A> {
    pub fn new(
        plugin: &'a P,
        behavior: &'a B,
        agent: &'a mut A,
        config: &'a PluginConfig,
        hooks: Option<&'a dyn PluginHooks>,
        retry_policy: Option<&'a RetryPolicy>,
        memory: Option<Box<dyn Memory>>,
        token_pool: Option<&'a dyn TokenPool>,
    ) -> Self {
        Self {
            plugin,
            behavior,
            agent,
            config,
            hooks,
            retry_policy,
            memory,
            token_pool,
            _phantom: PhantomData,
        }
    }

    /// Run the plugin: to_prompt → LLM → parse → behavior.decide.
    /// Retries and handoffs are handled by the Behavior.
    ///
    /// Lifecycle hooks (`on_start`, `on_turn`, `on_error`, `on_complete`)
    /// are called at each stage when hooks are configured.
    /// Backoff is applied between retries when a retry_policy is set.
    /// Output size is validated against config.max_output_bytes.
    /// If memory is set, the prompt and final output are saved.
    pub fn run(&mut self, input: &P::Input) -> Result<RunResult, PluginError> {
        // Set the plugin's system prompt before the first LLM call.
        self.agent.set_system_prompt(self.plugin.system_prompt());

        let prompt = self.plugin.to_prompt(input);
        let mut attempt = 0u32;

        let est_input_tokens = (prompt.len() as u32).div_ceil(4);

        if let Some(hooks) = self.hooks {
            hooks.on_start(self.plugin.name(), est_input_tokens);
        }

        loop {
            if attempt >= self.config.max_turns {
                return Err(PluginError::MaxTurnsExceeded(self.config.max_turns));
            }

            let tokens_before = self.agent.token_usage();

            let raw = self
                .agent
                .send(&prompt)
                .map_err(|e| PluginError::Other(e.to_string()))?;

            let tokens_after = self.agent.token_usage();
            let token_delta = tokens_after.saturating_sub(tokens_before);

            match self.plugin.parse_output(&raw) {
                Ok(output) => {
                    let confidence = extract_confidence(&raw);
                    let action = self.behavior.decide(confidence);

                    if let Some(hooks) = self.hooks {
                        hooks.on_turn(
                            attempt,
                            Some(TokenUsage {
                                prompt_tokens: 0,
                                completion_tokens: token_delta as u32,
                                total_tokens: token_delta as u32,
                            }),
                            &action,
                        );
                    }

                    match action {
                        Action::Done => {
                            let json = serde_json::to_string(&output)
                                .map_err(|e| PluginError::Other(e.to_string()))?;
                            if json.len() > self.config.max_output_bytes {
                                return Err(PluginError::OutputTooLarge(
                                    json.len(),
                                    self.config.max_output_bytes,
                                ));
                            }
                            let result = RunResult {
                                output: json.clone(),
                                turns: attempt + 1,
                                final_action: Action::Done,
                            };
                            if let Some(hooks) = self.hooks {
                                hooks.on_complete(&result);
                            }
                            if let Some(ref mut memory) = self.memory {
                                memory.save(
                                    self.plugin.name(),
                                    &Message {
                                        role: Role::User,
                                        content: prompt.clone(),
                                        reasoning_content: None,
                                        tool_calls: None,
                                        tool_call_id: None,
                                        name: None,
                                    },
                                );
                                memory.save(
                                    self.plugin.name(),
                                    &Message {
                                        role: Role::Assistant,
                                        content: json,
                                        reasoning_content: None,
                                        tool_calls: None,
                                        tool_call_id: None,
                                        name: None,
                                    },
                                );
                            }
                            return Ok(result);
                        }
                        Action::Handoff(target) => {
                            let json = serde_json::to_string(&output)
                                .map_err(|e| PluginError::Other(e.to_string()))?;
                            if json.len() > self.config.max_output_bytes {
                                return Err(PluginError::OutputTooLarge(
                                    json.len(),
                                    self.config.max_output_bytes,
                                ));
                            }
                            let result = RunResult {
                                output: json,
                                turns: attempt + 1,
                                final_action: Action::Handoff(target),
                            };
                            if let Some(hooks) = self.hooks {
                                hooks.on_complete(&result);
                            }
                            return Ok(result);
                        }
                        Action::Retry => {
                            attempt += 1;
                            if let Some(policy) = self.retry_policy {
                                std::thread::sleep(policy.jittered_backoff(attempt));
                            }
                            continue;
                        }
                    }
                }
                Err(e) => {
                    if let Some(hooks) = self.hooks {
                        hooks.on_error(attempt, &e);
                    }
                    match self.behavior.on_error(&e, attempt) {
                        ErrorAction::Retry => {
                            attempt += 1;
                            if let Some(policy) = self.retry_policy {
                                std::thread::sleep(policy.jittered_backoff(attempt));
                            }
                            continue;
                        }
                        ErrorAction::Abort => return Err(e),
                        ErrorAction::Escalate => {
                            return Err(PluginError::Escalated {
                                original: Box::new(e),
                                after_attempts: attempt,
                            });
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Agent abstraction
// ---------------------------------------------------------------------------

/// Minimal interface for an LLM-calling agent.
pub trait PluginAgent {
    /// Send a user message and return the assistant's text response.
    fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;

    /// Set the system prompt. Called once per plugin run before the first send().
    fn set_system_prompt(&mut self, _prompt: &str) {}

    /// Returns the agent's cumulative token usage so far.
    /// Defaults to 0 for agents that do not track tokens.
    fn token_usage(&self) -> u64 {
        0
    }
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

    #[error("Context window exceeded: {0} tokens required")]
    ContextExceeded(u32),

    #[error("Max turns exceeded ({0})")]
    MaxTurnsExceeded(u32),

    #[error("Output too large: {0} bytes (max {1})")]
    OutputTooLarge(usize, usize),

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
        if let Some((_, after)) = line.split_once("\"confidence\"") {
            if let Some(colon) = after.find(':') {
                let val = after[colon + 1..]
                    .trim()
                    .trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-');
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
        let diff = input.get("diff").and_then(|v| v.as_str()).unwrap_or("");
        format!(
            "Please review the following code for bugs, security issues, and design problems.\n\n\
             Return a JSON object with an 'issues' array and a 'confidence' field (0.0-1.0).\n\
             Each issue: severity (critical/high/medium/low), file, line, description.\n\n\
             CODE TO REVIEW:\n{}",
            diff
        )
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

    #[test]
    fn test_plugin_config_defaults() {
        let config = PluginConfig::default();
        assert_eq!(config.max_turns, 10);
        assert_eq!(config.max_output_bytes, 1_048_576);
        assert!(config.context_check);
        assert_eq!(config.timeout_per_call_secs, 120);
    }

    #[test]
    fn test_plugin_error_display() {
        let err = PluginError::MaxTurnsExceeded(5);
        assert_eq!(format!("{}", err), "Max turns exceeded (5)");

        let err = PluginError::ContextExceeded(500_000);
        assert_eq!(
            format!("{}", err),
            "Context window exceeded: 500000 tokens required"
        );

        let err = PluginError::OutputTooLarge(2_000_000, 1_000_000);
        assert_eq!(
            format!("{}", err),
            "Output too large: 2000000 bytes (max 1000000)"
        );
    }

    /// A PluginHooks implementation that records lifecycle calls for testing.
    struct TestHooks {
        starts: std::sync::atomic::AtomicU32,
        turns: std::sync::atomic::AtomicU32,
        errors: std::sync::atomic::AtomicU32,
        completes: std::sync::atomic::AtomicU32,
    }

    impl TestHooks {
        fn new() -> Self {
            Self {
                starts: std::sync::atomic::AtomicU32::new(0),
                turns: std::sync::atomic::AtomicU32::new(0),
                errors: std::sync::atomic::AtomicU32::new(0),
                completes: std::sync::atomic::AtomicU32::new(0),
            }
        }
    }

    impl PluginHooks for TestHooks {
        fn on_start(&self, _plugin: &str, _input_tokens: u32) {
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn on_turn(&self, _attempt: u32, _tokens: Option<TokenUsage>, _action: &Action) {
            self.turns
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn on_error(&self, _attempt: u32, _error: &PluginError) {
            self.errors
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn on_complete(&self, _result: &RunResult) {
            self.completes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// A mock agent that returns a fixed response.
    struct MockAgent {
        response: String,
    }

    impl PluginAgent for MockAgent {
        fn send(&mut self, _message: &str) -> Result<String, Box<dyn std::error::Error>> {
            Ok(self.response.clone())
        }
    }

    #[test]
    fn test_plugin_runner_hooks_lifecycle() {
        let plugin = CodeReviewPlugin::new();
        let behavior = YoloBehavior;
        let mut agent = MockAgent {
            response: r#"{"issues":[{"severity":"high","file":"a.rs","line":1,"description":"bad"}],"confidence":0.95}"#.into(),
        };
        let config = PluginConfig::default();
        let hooks = TestHooks::new();
        let mut runner = PluginRunner::new(
            &plugin,
            &behavior,
            &mut agent,
            &config,
            Some(&hooks),
            None,
            None,
            None,
        );

        let input = serde_json::json!({"diff": "+unsafe code"});
        let result = runner.run(&input).unwrap();

        assert_eq!(result.turns, 1);
        assert_eq!(result.final_action, Action::Done);
        assert_eq!(
            hooks.starts.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "on_start should be called once"
        );
        assert_eq!(
            hooks.turns.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "on_turn should be called once"
        );
        assert_eq!(
            hooks.errors.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "on_error should not be called"
        );
        assert_eq!(
            hooks.completes.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "on_complete should be called once"
        );
    }

    #[test]
    fn test_plugin_runner_max_turns_exceeded() {
        let plugin = CodeReviewPlugin::new();
        let behavior = StrictBehavior {
            confidence_threshold: 1.0, // never satisfied
            ..Default::default()
        };
        let mut agent = MockAgent {
            response: r#"{"issues":[],"confidence":0.5}"#.into(),
        };
        let config = PluginConfig {
            max_turns: 2,
            ..Default::default()
        };
        let mut runner = PluginRunner::new(
            &plugin, &behavior, &mut agent, &config, None, None, None, None,
        );

        let input = serde_json::json!({});
        let err = runner.run(&input).unwrap_err();
        assert!(matches!(err, PluginError::MaxTurnsExceeded(2)));
    }

    #[test]
    fn test_plugin_runner_output_too_large() {
        // Create a plugin whose output exceeds the max_output_bytes limit.
        struct LargeOutputPlugin;
        impl Plugin for LargeOutputPlugin {
            type Input = serde_json::Value;
            type Output = serde_json::Value;

            fn name(&self) -> &str {
                "large-output"
            }
            fn system_prompt(&self) -> &str {
                ""
            }
            fn to_prompt(&self, _input: &Self::Input) -> String {
                "do it".into()
            }
            fn parse_output(&self, _raw: &str) -> Result<serde_json::Value, PluginError> {
                // Return a value that serializes to more than 100 bytes.
                Ok(serde_json::json!({"data": "A".repeat(200)}))
            }
        }

        let plugin = LargeOutputPlugin;
        let behavior = YoloBehavior;
        let mut agent = MockAgent {
            response: "any".into(),
        };
        let config = PluginConfig {
            max_output_bytes: 50,
            ..Default::default()
        };
        let mut runner = PluginRunner::new(
            &plugin, &behavior, &mut agent, &config, None, None, None, None,
        );

        let input = serde_json::json!({});
        let err = runner.run(&input).unwrap_err();
        assert!(matches!(err, PluginError::OutputTooLarge(_, 50)));
    }

    #[test]
    fn test_plugin_runner_memory_save() {
        use artemis_memory::InMemoryMemory;

        let plugin = CodeReviewPlugin::new();
        let behavior = YoloBehavior;
        let mut agent = MockAgent {
            response: r#"{"issues":[],"confidence":0.9}"#.into(),
        };
        let config = PluginConfig::default();
        let memory = Box::new(InMemoryMemory::new());
        let mut runner = PluginRunner::new(
            &plugin,
            &behavior,
            &mut agent,
            &config,
            None,
            None,
            Some(memory),
            None,
        );

        let input = serde_json::json!({"diff": "test"});
        let result = runner.run(&input).unwrap();
        assert_eq!(result.final_action, Action::Done);

        // The memory was moved into the runner; we can't access it directly from
        // here after it's been consumed. The save happened during run().
        // For a proper test we'd need the memory to be accessible after the run,
        // but this validates that the save path compiles and runs without panic.
    }
}
