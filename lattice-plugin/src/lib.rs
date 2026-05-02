#![allow(clippy::new_without_default)]
use lattice_agent::memory::{EntryKind, Memory, MemoryEntry};
use lattice_core::retry::RetryPolicy;
use lattice_core::streaming::TokenUsage;
use lattice_core::types::ToolDefinition;

use serde::{de::DeserializeOwned, Serialize};
use std::time::SystemTime;
use thiserror::Error;

pub mod builtin;
pub mod bundle;
pub mod erased;
pub mod erased_runner;
pub mod registry;

// Re-exports for backward compatibility
pub use builtin::code_review::CodeReviewPlugin;
pub(crate) use builtin::parse_utils::extract_confidence;

use crate::erased::ErasedPlugin;

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

    /// Optional JSON Schema describing the shape of the output.
    /// Used for validation and documentation.
    fn output_schema(&self) -> Option<serde_json::Value> {
        None
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

/// Wraps a typed Plugin reference as an ErasedPlugin for the shared run loop.
struct ErasedPluginAdapter<'a, P: Plugin + ?Sized>(&'a P);

impl<P: Plugin + ?Sized> ErasedPlugin for ErasedPluginAdapter<'_, P> {
    fn name(&self) -> &str {
        Plugin::name(self.0)
    }
    fn system_prompt(&self) -> &str {
        Plugin::system_prompt(self.0)
    }
    fn to_prompt_json(&self, context: &serde_json::Value) -> Result<String, PluginError> {
        let typed: P::Input = serde_json::from_value(context.clone())
            .map_err(|e| PluginError::Parse(format!("{}: {}", self.name(), e)))?;
        Ok(self.0.to_prompt(&typed))
    }
    fn parse_output_json(&self, raw: &str) -> Result<serde_json::Value, PluginError> {
        let typed = self.0.parse_output(raw)?;
        serde_json::to_value(typed)
            .map_err(|e| PluginError::Parse(format!("{}: {}", self.name(), e)))
    }
    fn tools(&self) -> &[lattice_core::types::ToolDefinition] {
        Plugin::tools(self.0)
    }
    fn preferred_model(&self) -> &str {
        Plugin::preferred_model(self.0)
    }
    fn output_schema(&self) -> Option<serde_json::Value> {
        Plugin::output_schema(self.0)
    }
}

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
    ) -> Self {
        Self {
            plugin,
            behavior,
            agent,
            config,
            hooks,
            retry_policy,
            memory,
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
        let adapter = ErasedPluginAdapter(self.plugin);
        let context = serde_json::to_value(input).map_err(|e| PluginError::Other(e.to_string()))?;
        crate::erased_runner::run_plugin_loop(
            &adapter,
            self.behavior,
            self.agent,
            &context,
            self.config,
            self.hooks,
            self.retry_policy,
            self.memory.as_deref(),
        )
    }
}

// ---------------------------------------------------------------------------
// Agent abstraction — re-exported from lattice-agent
// ---------------------------------------------------------------------------

pub use lattice_agent::PluginAgent;

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
// Helpers (moved to builtin/parse_utils.rs)
// ------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate a timestamp string (Unix epoch microseconds) from the system clock.
fn timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_micros().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

pub(crate) fn save_memory_entries(
    memory: &dyn Memory,
    plugin_name: &str,
    prompt: &str,
    result: &RunResult,
) {
    let ts = timestamp();
    memory.save_entry(MemoryEntry {
        id: format!("{}-user-{}", plugin_name, ts),
        kind: EntryKind::SessionLog,
        session_id: plugin_name.to_string(),
        summary: format!("User prompt for {}", plugin_name),
        content: prompt.to_string(),
        tags: vec![],
        created_at: ts.clone(),
    });
    memory.save_entry(MemoryEntry {
        id: format!("{}-assistant-{}", plugin_name, ts),
        kind: EntryKind::SessionLog,
        session_id: plugin_name.to_string(),
        summary: format!("Assistant response for {}", plugin_name),
        content: result.output.clone(),
        tags: vec![],
        created_at: ts,
    });
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
        assert_eq!(out.confidence, 0.9);
    }

    #[test]
    fn test_code_review_parse_markdown() {
        let p = CodeReviewPlugin::new();
        let raw = "```json\n{\"issues\":[],\"confidence\":0.8}\n```";
        let out = p.parse_output(raw).unwrap();
        assert_eq!(out.confidence, 0.8);
    }

    #[test]
    fn test_extract_confidence() {
        assert!((extract_confidence("{\"confidence\":0.85}") - 0.85).abs() < 0.01);
        assert!((extract_confidence("no confidence field") - 0.0).abs() < 0.01);
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

    #[async_trait::async_trait(?Send)]
    impl PluginAgent for MockAgent {
        async fn send(&mut self, _message: &str) -> Result<String, Box<dyn std::error::Error>> {
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
        );

        let input = builtin::code_review::CodeReviewInput {
            input: "+unsafe code".into(),
            file_path: String::new(),
            context_rules: vec![],
        };
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
        let mut runner =
            PluginRunner::new(&plugin, &behavior, &mut agent, &config, None, None, None);

        let input = builtin::code_review::CodeReviewInput {
            input: String::new(),
            file_path: String::new(),
            context_rules: vec![],
        };
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
        let mut runner =
            PluginRunner::new(&plugin, &behavior, &mut agent, &config, None, None, None);

        let input = serde_json::json!({});
        let err = runner.run(&input).unwrap_err();
        assert!(matches!(err, PluginError::OutputTooLarge(_, 50)));
    }

    #[test]
    fn test_plugin_runner_memory_save() {
        use lattice_agent::memory::InMemoryMemory;

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
        );

        let input = builtin::code_review::CodeReviewInput {
            input: "test".into(),
            file_path: String::new(),
            context_rules: vec![],
        };
        let result = runner.run(&input).unwrap();
        assert_eq!(result.final_action, Action::Done);

        // The memory was moved into the runner; we can't access it directly from
        // here after it's been consumed. The save happened during run().
        // For a proper test we'd need the memory to be accessible after the run,
        // but this validates that the save path compiles and runs without panic.
    }

    #[test]
    fn test_behavior_mode_to_behavior() {
        use crate::bundle::BehaviorMode;

        let strict = BehaviorMode::Strict {
            confidence_threshold: 0.8,
            max_retries: 2,
            escalate_to: Some("human".into()),
        };
        let behavior = strict.to_behavior();
        assert!(matches!(behavior.decide(0.9), crate::Action::Done));
        assert!(matches!(behavior.decide(0.5), crate::Action::Retry));
        assert!(matches!(
            behavior.on_error(&crate::PluginError::Parse("x".into()), 3),
            crate::ErrorAction::Escalate
        ));

        let yolo = BehaviorMode::Yolo;
        let behavior = yolo.to_behavior();
        assert!(matches!(behavior.decide(0.1), crate::Action::Done));
    }

    #[test]
    fn test_plugin_registry_register_and_get() {
        use crate::bundle::{BehaviorMode, PluginBundle, PluginMeta};
        use crate::registry::PluginRegistry;

        let mut registry = PluginRegistry::new();
        let bundle = PluginBundle {
            meta: PluginMeta {
                name: "test".into(),
                version: "0.1".into(),
                description: "test plugin".into(),
                author: "test".into(),
            },
            plugin: Box::new(crate::CodeReviewPlugin::new()),
            default_behavior: BehaviorMode::Yolo,
            default_tools: vec![],
        };
        registry.register(bundle).unwrap();
        assert!(registry.get("test").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_plugin_registry_duplicate_rejected() {
        use crate::bundle::{BehaviorMode, PluginBundle, PluginMeta};
        use crate::registry::PluginRegistry;

        let mut registry = PluginRegistry::new();
        let make_bundle = |name: &str| PluginBundle {
            meta: PluginMeta {
                name: name.into(),
                version: "0.1".into(),
                description: "".into(),
                author: "".into(),
            },
            plugin: Box::new(crate::CodeReviewPlugin::new()),
            default_behavior: BehaviorMode::Yolo,
            default_tools: vec![],
        };
        registry.register(make_bundle("dup")).unwrap();
        assert!(registry.register(make_bundle("dup")).is_err());
    }
}
