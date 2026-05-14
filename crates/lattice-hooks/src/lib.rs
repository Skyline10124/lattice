//! LATTICE Hooks — universal hook system for runtime lifecycle interception.
//!
//! This crate defines the trait interfaces for:
//! - Generic lifecycle hooks that can intercept any runtime point
//! - Hook registration and dispatch
//! - All hook interception points

use async_trait::async_trait;
use lattice_core::types::ToolCall;
use lattice_prompt::{
    ContextManifestEntry, Layer, PromptContribution, PromptSection, RenderedPrompt, TokenBudget,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// All interception points where hooks can be triggered.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookPoint {
    /// Before an agent turn starts.
    PreAgentTurn,
    /// After an agent turn completes.
    PostAgentTurn,
    /// Before a tool call is executed.
    PreToolCall,
    /// After a tool call returns.
    PostToolCall,
    /// Before a plugin is invoked.
    PrePluginInvoke,
    /// After a plugin returns.
    PostPluginInvoke,
    /// Before an orchestrator event is emitted.
    PreOrchestratorEvent,
    /// After an orchestrator event is processed.
    PostOrchestratorEvent,
    /// Before model inference.
    PreInference,
    /// After model inference.
    PostInference,
    /// At session start.
    SessionStart,
    /// At session end.
    SessionEnd,
    /// Custom hook point for user-defined interception.
    Custom(String),
}

/// Prompt-aware lifecycle points for typed runtime interception.
///
/// These points are intentionally narrower than [`HookPoint::Custom`]: they
/// operate on prompt/context objects before those objects are rendered into
/// model-visible messages. Hooks should transform typed contributions and
/// policies, not patch final prompt strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PromptHookPoint {
    /// A user turn has started, before context providers are collected.
    TurnStart,
    /// Context providers have produced sections, before policy validation.
    PostContextCollect,
    /// Immediately before prompt compilation.
    PrePromptCompile,
    /// Immediately after prompt compilation.
    PostPromptCompile,
    /// Before a model-selected tool call is executed.
    PreToolCall,
    /// After a tool call returns and before its output is added to history.
    PostToolResult,
}

/// Typed state exposed to prompt lifecycle hooks.
#[derive(Debug, Clone)]
pub struct PromptHookContext {
    pub point: PromptHookPoint,
    pub request_id: String,
    pub user_input: String,
    pub contributions: Vec<PromptContribution>,
    pub sections: Vec<PromptSection>,
    pub budgets: Vec<TokenBudget>,
    pub rendered: Option<RenderedPrompt>,
    pub tool_call: Option<ToolCall>,
    pub tool_result: Option<String>,
    pub metadata: serde_json::Value,
}

impl PromptHookContext {
    pub fn new(point: PromptHookPoint, request_id: impl Into<String>, user_input: &str) -> Self {
        Self {
            point,
            request_id: request_id.into(),
            user_input: user_input.to_string(),
            contributions: Vec::new(),
            sections: Vec::new(),
            budgets: Vec::new(),
            rendered: None,
            tool_call: None,
            tool_result: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[deprecated(note = "Use with_contributions() instead")]
    pub fn with_sections(
        mut self,
        sections: Vec<PromptSection>,
        budgets: Vec<TokenBudget>,
    ) -> Self {
        self.contributions = sections
            .iter()
            .zip(budgets.iter())
            .map(|(section, budget)| {
                PromptContribution::from_section(section.clone(), *budget, "hook-context")
            })
            .collect();
        self.sections = sections;
        self.budgets = budgets;
        self
    }

    pub fn with_contributions(mut self, contributions: Vec<PromptContribution>) -> Self {
        self.sections = contributions
            .iter()
            .map(PromptContribution::section)
            .collect();
        self.budgets = contributions
            .iter()
            .map(|contribution| contribution.budget)
            .collect();
        self.contributions = contributions;
        self
    }

    pub fn with_rendered(mut self, rendered: RenderedPrompt) -> Self {
        self.rendered = Some(rendered);
        self
    }

    pub fn with_tool(mut self, call: ToolCall, result: Option<String>) -> Self {
        self.tool_call = Some(call);
        self.tool_result = result;
        self
    }

    pub fn prompt_manifest(&self) -> Vec<ContextManifestEntry> {
        self.rendered
            .as_ref()
            .map(|rendered| {
                rendered
                    .fragments
                    .iter()
                    .map(|fragment| fragment.manifest_entry())
                    .chain(rendered.context_manifest.iter().cloned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Controlled mutations a prompt lifecycle hook may request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PromptHookDecision {
    Continue,
    AddSection {
        section: PromptSection,
        budget: TokenBudget,
    },
    RemoveSection {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        layer: Option<Layer>,
        reason: String,
    },
    ReplaceSection {
        id: String,
        section: PromptSection,
        budget: TokenBudget,
    },
    SetBudget {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        layer: Option<Layer>,
        budget: TokenBudget,
    },
    Abort(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecutionRecord {
    pub hook_name: String,
    pub before_hash: String,
    pub after_hash: String,
    pub decision: PromptHookDecision,
}

#[derive(Debug, Clone)]
pub struct PromptHookOutcome {
    pub contributions: Vec<PromptContribution>,
    pub decisions: Vec<(String, PromptHookDecision)>,
    pub executions: Vec<HookExecutionRecord>,
}

fn content_hash(contributions: &[PromptContribution]) -> String {
    let mut hasher = Sha256::new();
    for c in contributions {
        hasher.update(c.content.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn parse_hook_point(hook_id: &str, s: &str) -> Result<PromptHookPoint, HookError> {
    match s {
        "turn_start" => Ok(PromptHookPoint::TurnStart),
        "post_context_collect" => Ok(PromptHookPoint::PostContextCollect),
        "pre_prompt_compile" => Ok(PromptHookPoint::PrePromptCompile),
        "post_prompt_compile" => Ok(PromptHookPoint::PostPromptCompile),
        "pre_tool_call" => Ok(PromptHookPoint::PreToolCall),
        "post_tool_result" => Ok(PromptHookPoint::PostToolResult),
        _ => Err(HookError::InvalidSpec {
            hook_id: hook_id.to_string(),
            reason: format!("unknown hook point: '{}'", s),
        }),
    }
}

/// Adds a new contribution to a specified layer.
pub struct AddSectionHook {
    hook_id: String,
    point: PromptHookPoint,
    layer: Layer,
    content: String,
    budget: TokenBudget,
}

#[async_trait]
impl PromptHook for AddSectionHook {
    fn name(&self) -> &str {
        &self.hook_id
    }
    fn points(&self) -> Vec<PromptHookPoint> {
        vec![self.point]
    }
    async fn execute(&self, _context: &PromptHookContext) -> Result<PromptHookDecision, HookError> {
        let tokens = lattice_core::tokens::TokenEstimator::estimate_text(&self.content);
        Ok(PromptHookDecision::AddSection {
            section: PromptSection::new(&self.content, self.layer, 0, tokens),
            budget: self.budget,
        })
    }
}

/// Removes a contribution by ID or by layer.
pub struct RemoveSectionHook {
    hook_id: String,
    point: PromptHookPoint,
    target_id: Option<String>,
    target_layer: Option<Layer>,
    reason: String,
}

#[async_trait]
impl PromptHook for RemoveSectionHook {
    fn name(&self) -> &str {
        &self.hook_id
    }
    fn points(&self) -> Vec<PromptHookPoint> {
        vec![self.point]
    }
    async fn execute(&self, _context: &PromptHookContext) -> Result<PromptHookDecision, HookError> {
        Ok(PromptHookDecision::RemoveSection {
            id: self.target_id.clone(),
            layer: self.target_layer,
            reason: self.reason.clone(),
        })
    }
}

/// Modifies the token budget for a contribution by ID or by layer.
pub struct SetBudgetHook {
    hook_id: String,
    point: PromptHookPoint,
    target_id: Option<String>,
    target_layer: Option<Layer>,
    budget: TokenBudget,
}

#[async_trait]
impl PromptHook for SetBudgetHook {
    fn name(&self) -> &str {
        &self.hook_id
    }
    fn points(&self) -> Vec<PromptHookPoint> {
        vec![self.point]
    }
    async fn execute(&self, _context: &PromptHookContext) -> Result<PromptHookDecision, HookError> {
        Ok(PromptHookDecision::SetBudget {
            id: self.target_id.clone(),
            layer: self.target_layer,
            budget: self.budget,
        })
    }
}

/// Halts prompt compilation with a reason.
pub struct AbortHook {
    hook_id: String,
    point: PromptHookPoint,
    reason: String,
}

#[async_trait]
impl PromptHook for AbortHook {
    fn name(&self) -> &str {
        &self.hook_id
    }
    fn points(&self) -> Vec<PromptHookPoint> {
        vec![self.point]
    }
    async fn execute(&self, _context: &PromptHookContext) -> Result<PromptHookDecision, HookError> {
        Ok(PromptHookDecision::Abort(self.reason.clone()))
    }
}

/// Compile PromptHookSpec DTOs into executable PromptHook trait objects.
///
/// Each spec maps to exactly one hook point (D-16). Unknown action or point
/// strings return Err, failing agent assembly (D-06, D-07).
pub fn build_prompt_hooks(
    specs: &[lattice_prompt::spec::PromptHookSpec],
) -> Result<Vec<Box<dyn PromptHook>>, HookError> {
    let mut hooks: Vec<Box<dyn PromptHook>> = Vec::with_capacity(specs.len());
    for spec in specs {
        let point = parse_hook_point(&spec.id, &spec.point)?;
        let hook: Box<dyn PromptHook> = match spec.action.as_str() {
            "add_section" => {
                let target = spec.target.as_ref().ok_or_else(|| HookError::InvalidSpec {
                    hook_id: spec.id.clone(),
                    reason: "add_section requires target with layer".to_string(),
                })?;
                let layer = target.layer.ok_or_else(|| HookError::InvalidSpec {
                    hook_id: spec.id.clone(),
                    reason: "add_section requires target.layer".to_string(),
                })?;
                let content = spec.content.clone().ok_or_else(|| HookError::InvalidSpec {
                    hook_id: spec.id.clone(),
                    reason: "add_section requires content".to_string(),
                })?;
                let budget = spec.budget.unwrap_or_default().to_token_budget(512);
                Box::new(AddSectionHook {
                    hook_id: spec.id.clone(),
                    point,
                    layer,
                    content,
                    budget,
                })
            }
            "remove_section" => {
                let target = spec.target.as_ref().ok_or_else(|| HookError::InvalidSpec {
                    hook_id: spec.id.clone(),
                    reason: "remove_section requires target with id or layer".to_string(),
                })?;
                if target.id.is_none() && target.layer.is_none() {
                    return Err(HookError::InvalidSpec {
                        hook_id: spec.id.clone(),
                        reason: "remove_section requires target.id or target.layer".to_string(),
                    });
                }
                Box::new(RemoveSectionHook {
                    hook_id: spec.id.clone(),
                    point,
                    target_id: target.id.clone(),
                    target_layer: target.layer,
                    reason: format!("removed by hook '{}'", spec.id),
                })
            }
            "set_budget" => {
                let target = spec.target.as_ref().ok_or_else(|| HookError::InvalidSpec {
                    hook_id: spec.id.clone(),
                    reason: "set_budget requires target with id or layer".to_string(),
                })?;
                if target.id.is_none() && target.layer.is_none() {
                    return Err(HookError::InvalidSpec {
                        hook_id: spec.id.clone(),
                        reason: "set_budget requires target.id or target.layer".to_string(),
                    });
                }
                let budget_spec = spec.budget.ok_or_else(|| HookError::InvalidSpec {
                    hook_id: spec.id.clone(),
                    reason: "set_budget requires budget".to_string(),
                })?;
                let budget = budget_spec.to_token_budget(512);
                Box::new(SetBudgetHook {
                    hook_id: spec.id.clone(),
                    point,
                    target_id: target.id.clone(),
                    target_layer: target.layer,
                    budget,
                })
            }
            "abort" => {
                let reason = spec
                    .content
                    .clone()
                    .unwrap_or_else(|| format!("aborted by hook '{}'", spec.id));
                Box::new(AbortHook {
                    hook_id: spec.id.clone(),
                    point,
                    reason,
                })
            }
            other => {
                return Err(HookError::InvalidSpec {
                    hook_id: spec.id.clone(),
                    reason: format!("unknown action: '{}'", other),
                });
            }
        };
        hooks.push(hook);
    }
    Ok(hooks)
}

/// Typed hook for prompt/context lifecycle interception.
#[async_trait]
pub trait PromptHook: Send + Sync {
    fn name(&self) -> &str;

    fn points(&self) -> Vec<PromptHookPoint>;

    async fn execute(&self, context: &PromptHookContext) -> Result<PromptHookDecision, HookError>;
}

/// In-process prompt hook chain.
///
/// Hooks run in registration order. Mutating decisions update a working copy of
/// prompt sections and budgets so later hooks observe earlier decisions.
#[derive(Default)]
pub struct PromptHookChain {
    hooks: Vec<Box<dyn PromptHook>>,
}

impl PromptHookChain {
    pub fn new(hooks: Vec<Box<dyn PromptHook>>) -> Self {
        Self { hooks }
    }

    pub fn push(&mut self, hook: Box<dyn PromptHook>) {
        self.hooks.push(hook);
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub async fn dispatch(
        &self,
        context: PromptHookContext,
    ) -> Result<PromptHookOutcome, HookError> {
        let mut contributions = if context.contributions.is_empty() {
            sections_to_contributions(&context.sections, &context.budgets, "hook-dispatch")
        } else {
            context.contributions.clone()
        };
        let mut decisions = Vec::new();
        let mut executions = Vec::new();

        for hook in &self.hooks {
            if !hook.points().contains(&context.point) {
                continue;
            }

            let before_hash = content_hash(&contributions);

            let mut hook_context = context.clone();
            hook_context = hook_context.with_contributions(contributions.clone());

            let decision = hook.execute(&hook_context).await?;
            match &decision {
                PromptHookDecision::Continue => {}
                PromptHookDecision::AddSection { section, budget } => {
                    contributions.push(PromptContribution::from_section(
                        section.clone(),
                        *budget,
                        hook.name(),
                    ));
                }
                PromptHookDecision::RemoveSection { id, layer, .. } => {
                    remove_contribution_by_target(&mut contributions, id.as_deref(), *layer);
                }
                PromptHookDecision::ReplaceSection {
                    id,
                    section,
                    budget,
                } => {
                    if let Some(index) = contributions
                        .iter()
                        .position(|candidate| candidate.id == *id)
                    {
                        contributions[index] =
                            PromptContribution::from_section(section.clone(), *budget, hook.name());
                    }
                }
                PromptHookDecision::SetBudget { id, layer, budget } => {
                    for contribution in contributions.iter_mut() {
                        let id_match = id.as_ref().is_some_and(|tid| contribution.id == *tid);
                        let layer_match = layer.is_some_and(|l| contribution.layer == l);
                        if id_match || layer_match {
                            contribution.budget = *budget;
                        }
                    }
                }
                PromptHookDecision::Abort(reason) => {
                    return Err(HookError::Aborted(format!(
                        "{} aborted {:?}: {}",
                        hook.name(),
                        context.point,
                        reason
                    )));
                }
            }

            let after_hash = content_hash(&contributions);
            executions.push(HookExecutionRecord {
                hook_name: hook.name().to_string(),
                before_hash,
                after_hash,
                decision: decision.clone(),
            });

            decisions.push((hook.name().to_string(), decision));
        }

        Ok(PromptHookOutcome {
            contributions,
            decisions,
            executions,
        })
    }
}

fn sections_to_contributions(
    sections: &[PromptSection],
    budgets: &[TokenBudget],
    provider: &str,
) -> Vec<PromptContribution> {
    sections
        .iter()
        .zip(budgets.iter())
        .map(|(section, budget)| {
            PromptContribution::from_section(section.clone(), *budget, provider)
        })
        .collect()
}

fn remove_contribution_by_target(
    contributions: &mut Vec<PromptContribution>,
    id: Option<&str>,
    layer: Option<Layer>,
) {
    let mut index = 0;
    while index < contributions.len() {
        let id_match = id.is_some_and(|tid| contributions[index].id == tid);
        let layer_match = layer.is_some_and(|l| contributions[index].layer == l);
        if id_match || layer_match {
            contributions.remove(index);
        } else {
            index += 1;
        }
    }
}

/// Context passed to a hook when it fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    /// Which hook point triggered this hook.
    pub point: HookPoint,
    /// The name of the entity being intercepted (e.g., tool name, plugin name).
    pub target: String,
    /// Arbitrary key-value data for the hook to inspect or modify.
    pub data: serde_json::Value,
}

/// Return value from a hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HookResult {
    /// Continue normal execution.
    Continue,
    /// Modify the context data before proceeding.
    Modify(serde_json::Value),
    /// Abort execution with an error message.
    Abort(String),
}

/// Trait for a lifecycle hook.
#[async_trait]
pub trait Hook: Send + Sync {
    /// The name of this hook (for logging and identification).
    fn name(&self) -> &str;

    /// Which hook points this hook is interested in.
    fn points(&self) -> Vec<HookPoint>;

    /// Execute the hook. Called when any of the registered hook points fire.
    async fn execute(&self, context: &HookContext) -> Result<HookResult, HookError>;
}

/// Trait for hook registration and dispatch.
#[async_trait]
pub trait HookRegistry: Send + Sync {
    /// Register a hook.
    async fn register(&self, hook: Box<dyn Hook>) -> Result<(), HookError>;

    /// Unregister a hook by name.
    async fn unregister(&self, name: &str) -> Result<(), HookError>;

    /// Dispatch to all hooks registered for the given hook point.
    /// Returns HookResult::Continue if all hooks pass, or the first
    /// non-Continue result.
    async fn dispatch(&self, context: &HookContext) -> Result<HookResult, HookError>;

    /// List all registered hook names.
    fn list(&self) -> Vec<String>;
}

/// Errors from hook operations.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("hook not found: {0}")]
    NotFound(String),
    #[error("hook execution failed: {0}")]
    ExecutionFailed(String),
    #[error("hook aborted execution: {0}")]
    Aborted(String),
    #[error("invalid hook spec '{hook_id}': {reason}")]
    InvalidSpec { hook_id: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_prompt::{Layer, PromptSection};

    struct StaticPromptHook {
        name: &'static str,
        points: Vec<PromptHookPoint>,
        decision: PromptHookDecision,
    }

    #[async_trait]
    impl PromptHook for StaticPromptHook {
        fn name(&self) -> &str {
            self.name
        }

        fn points(&self) -> Vec<PromptHookPoint> {
            self.points.clone()
        }

        async fn execute(
            &self,
            _context: &PromptHookContext,
        ) -> Result<PromptHookDecision, HookError> {
            Ok(self.decision.clone())
        }
    }

    fn section(content: &str) -> PromptSection {
        PromptSection::new(content, Layer::Events, 5, 1)
    }

    #[tokio::test]
    async fn prompt_hook_chain_adds_sections() {
        let added = section("hook context");
        let chain = PromptHookChain::new(vec![Box::new(StaticPromptHook {
            name: "add-context",
            points: vec![PromptHookPoint::PostContextCollect],
            decision: PromptHookDecision::AddSection {
                section: added.clone(),
                budget: TokenBudget::Fixed(1),
            },
        })]);

        let outcome = chain
            .dispatch(PromptHookContext::new(
                PromptHookPoint::PostContextCollect,
                "test",
                "hello",
            ))
            .await
            .unwrap();

        assert_eq!(outcome.contributions.len(), 1);
        assert_eq!(outcome.contributions[0].content, added.content);
        assert_eq!(outcome.contributions[0].layer, added.layer);
        assert_eq!(outcome.contributions[0].budget, TokenBudget::Fixed(1));
        assert_eq!(outcome.decisions.len(), 1);
    }

    #[tokio::test]
    async fn prompt_hook_chain_removes_sections_by_fragment_id() {
        let keep = section("keep");
        let remove = section("remove");
        let remove_id = remove.to_fragment().id;
        let chain = PromptHookChain::new(vec![Box::new(StaticPromptHook {
            name: "remove-context",
            points: vec![PromptHookPoint::PrePromptCompile],
            decision: PromptHookDecision::RemoveSection {
                id: Some(remove_id),
                layer: None,
                reason: "policy".to_string(),
            },
        })]);

        let contributions = vec![
            PromptContribution::from_section(keep.clone(), TokenBudget::Dynamic, "test"),
            PromptContribution::from_section(remove, TokenBudget::Dynamic, "test"),
        ];
        let context = PromptHookContext::new(PromptHookPoint::PrePromptCompile, "test", "hello")
            .with_contributions(contributions);
        let outcome = chain.dispatch(context).await.unwrap();

        assert_eq!(outcome.contributions.len(), 1);
        assert_eq!(outcome.contributions[0].content, keep.content);
        assert_eq!(outcome.contributions[0].layer, keep.layer);
        assert_eq!(outcome.contributions[0].budget, TokenBudget::Dynamic);
    }

    #[tokio::test]
    async fn prompt_hook_chain_removes_sections_by_layer() {
        let keep = PromptSection::new("keep", Layer::Rules, 5, 1);
        let remove = PromptSection::new("remove", Layer::Events, 5, 1);
        let chain = PromptHookChain::new(vec![Box::new(StaticPromptHook {
            name: "remove-by-layer",
            points: vec![PromptHookPoint::PrePromptCompile],
            decision: PromptHookDecision::RemoveSection {
                id: None,
                layer: Some(Layer::Events),
                reason: "layer policy".to_string(),
            },
        })]);

        let contributions = vec![
            PromptContribution::from_section(keep.clone(), TokenBudget::Dynamic, "test"),
            PromptContribution::from_section(remove, TokenBudget::Dynamic, "test"),
        ];
        let context = PromptHookContext::new(PromptHookPoint::PrePromptCompile, "test", "hello")
            .with_contributions(contributions);
        let outcome = chain.dispatch(context).await.unwrap();

        assert_eq!(outcome.contributions.len(), 1);
        assert_eq!(outcome.contributions[0].content, keep.content);
        assert_eq!(outcome.contributions[0].layer, Layer::Rules);
    }

    #[tokio::test]
    async fn prompt_hook_chain_aborts_execution() {
        let chain = PromptHookChain::new(vec![Box::new(StaticPromptHook {
            name: "deny",
            points: vec![PromptHookPoint::PreToolCall],
            decision: PromptHookDecision::Abort("blocked".to_string()),
        })]);

        let err = chain
            .dispatch(PromptHookContext::new(
                PromptHookPoint::PreToolCall,
                "test",
                "hello",
            ))
            .await
            .unwrap_err();

        assert!(matches!(err, HookError::Aborted(message) if message.contains("blocked")));
    }

    use lattice_prompt::spec::{BudgetSpec, PromptHookSpec, PromptTargetSpec};

    #[tokio::test]
    async fn build_add_section_hook() {
        let specs = vec![PromptHookSpec {
            id: "test-add".into(),
            point: "post_context_collect".into(),
            action: "add_section".into(),
            target: Some(PromptTargetSpec {
                id: None,
                layer: Some(Layer::Events),
            }),
            budget: Some(BudgetSpec::Fixed { tokens: Some(100) }),
            content: Some("injected content".into()),
        }];
        let hooks = build_prompt_hooks(&specs).unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name(), "test-add");
        assert!(hooks[0]
            .points()
            .contains(&PromptHookPoint::PostContextCollect));

        let ctx = PromptHookContext::new(PromptHookPoint::PostContextCollect, "req-1", "hello");
        let decision = hooks[0].execute(&ctx).await.unwrap();
        match decision {
            PromptHookDecision::AddSection { section, budget } => {
                assert_eq!(section.content, "injected content");
                assert_eq!(section.layer, Layer::Events);
                assert_eq!(budget, TokenBudget::Fixed(100));
            }
            other => panic!("expected AddSection, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn build_remove_section_hook() {
        let specs = vec![PromptHookSpec {
            id: "test-remove".into(),
            point: "pre_prompt_compile".into(),
            action: "remove_section".into(),
            target: Some(PromptTargetSpec {
                id: Some("target-id".into()),
                layer: None,
            }),
            budget: None,
            content: None,
        }];
        let hooks = build_prompt_hooks(&specs).unwrap();
        let ctx = PromptHookContext::new(PromptHookPoint::PrePromptCompile, "req-1", "hello");
        let decision = hooks[0].execute(&ctx).await.unwrap();
        match decision {
            PromptHookDecision::RemoveSection { id, layer, .. } => {
                assert_eq!(id, Some("target-id".into()));
                assert!(layer.is_none());
            }
            other => panic!("expected RemoveSection, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn build_remove_section_hook_by_layer() {
        let specs = vec![PromptHookSpec {
            id: "test-remove-layer".into(),
            point: "pre_prompt_compile".into(),
            action: "remove_section".into(),
            target: Some(PromptTargetSpec {
                id: None,
                layer: Some(Layer::Events),
            }),
            budget: None,
            content: None,
        }];
        let hooks = build_prompt_hooks(&specs).unwrap();
        let ctx = PromptHookContext::new(PromptHookPoint::PrePromptCompile, "req-1", "hello");
        let decision = hooks[0].execute(&ctx).await.unwrap();
        match decision {
            PromptHookDecision::RemoveSection { id, layer, .. } => {
                assert!(id.is_none());
                assert_eq!(layer, Some(Layer::Events));
            }
            other => panic!("expected RemoveSection, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn build_set_budget_hook() {
        let specs = vec![PromptHookSpec {
            id: "test-budget".into(),
            point: "pre_prompt_compile".into(),
            action: "set_budget".into(),
            target: Some(PromptTargetSpec {
                id: Some("budget-id".into()),
                layer: None,
            }),
            budget: Some(BudgetSpec::Ratio { value: 0.5 }),
            content: None,
        }];
        let hooks = build_prompt_hooks(&specs).unwrap();
        let ctx = PromptHookContext::new(PromptHookPoint::PrePromptCompile, "req-1", "hello");
        let decision = hooks[0].execute(&ctx).await.unwrap();
        match decision {
            PromptHookDecision::SetBudget { id, layer, budget } => {
                assert_eq!(id, Some("budget-id".into()));
                assert!(layer.is_none());
                assert_eq!(budget, TokenBudget::Ratio(0.5));
            }
            other => panic!("expected SetBudget, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn build_set_budget_hook_by_layer() {
        let specs = vec![PromptHookSpec {
            id: "test-budget-layer".into(),
            point: "pre_prompt_compile".into(),
            action: "set_budget".into(),
            target: Some(PromptTargetSpec {
                id: None,
                layer: Some(Layer::Memory),
            }),
            budget: Some(BudgetSpec::Ratio { value: 0.3 }),
            content: None,
        }];
        let hooks = build_prompt_hooks(&specs).unwrap();
        let ctx = PromptHookContext::new(PromptHookPoint::PrePromptCompile, "req-1", "hello");
        let decision = hooks[0].execute(&ctx).await.unwrap();
        match decision {
            PromptHookDecision::SetBudget { id, layer, budget } => {
                assert!(id.is_none());
                assert_eq!(layer, Some(Layer::Memory));
                assert_eq!(budget, TokenBudget::Ratio(0.3));
            }
            other => panic!("expected SetBudget, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn build_abort_hook() {
        let specs = vec![PromptHookSpec {
            id: "test-abort".into(),
            point: "pre_tool_call".into(),
            action: "abort".into(),
            target: None,
            budget: None,
            content: Some("not allowed".into()),
        }];
        let hooks = build_prompt_hooks(&specs).unwrap();
        let ctx = PromptHookContext::new(PromptHookPoint::PreToolCall, "req-1", "hello");
        let decision = hooks[0].execute(&ctx).await.unwrap();
        match decision {
            PromptHookDecision::Abort(reason) => assert_eq!(reason, "not allowed"),
            other => panic!("expected Abort, got {:?}", other),
        }
    }

    #[test]
    fn build_rejects_unknown_action() {
        let specs = vec![PromptHookSpec {
            id: "bad".into(),
            point: "turn_start".into(),
            action: "unknown_action".into(),
            target: None,
            budget: None,
            content: None,
        }];
        let err = match build_prompt_hooks(&specs) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(matches!(err, HookError::InvalidSpec { hook_id, .. } if hook_id == "bad"));
    }

    #[test]
    fn build_rejects_unknown_point() {
        let specs = vec![PromptHookSpec {
            id: "bad-point".into(),
            point: "nonexistent_point".into(),
            action: "abort".into(),
            target: None,
            budget: None,
            content: None,
        }];
        let err = match build_prompt_hooks(&specs) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(matches!(err, HookError::InvalidSpec { hook_id, .. } if hook_id == "bad-point"));
    }

    #[test]
    fn build_rejects_add_section_without_target() {
        let specs = vec![PromptHookSpec {
            id: "no-target".into(),
            point: "turn_start".into(),
            action: "add_section".into(),
            target: None,
            budget: None,
            content: Some("x".into()),
        }];
        let err = match build_prompt_hooks(&specs) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(matches!(err, HookError::InvalidSpec { hook_id, .. } if hook_id == "no-target"));
    }

    #[test]
    fn build_rejects_add_section_without_content() {
        let specs = vec![PromptHookSpec {
            id: "no-content".into(),
            point: "turn_start".into(),
            action: "add_section".into(),
            target: Some(PromptTargetSpec {
                id: None,
                layer: Some(Layer::Events),
            }),
            budget: None,
            content: None,
        }];
        let err = match build_prompt_hooks(&specs) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(matches!(err, HookError::InvalidSpec { hook_id, .. } if hook_id == "no-content"));
    }

    #[test]
    fn build_rejects_remove_section_without_target_id_or_layer() {
        let specs = vec![PromptHookSpec {
            id: "no-id".into(),
            point: "turn_start".into(),
            action: "remove_section".into(),
            target: Some(PromptTargetSpec {
                id: None,
                layer: None,
            }),
            budget: None,
            content: None,
        }];
        let err = match build_prompt_hooks(&specs) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(matches!(err, HookError::InvalidSpec { hook_id, .. } if hook_id == "no-id"));
    }

    #[test]
    fn build_rejects_set_budget_without_budget() {
        let specs = vec![PromptHookSpec {
            id: "no-budget".into(),
            point: "turn_start".into(),
            action: "set_budget".into(),
            target: Some(PromptTargetSpec {
                id: Some("x".into()),
                layer: None,
            }),
            budget: None,
            content: None,
        }];
        let err = match build_prompt_hooks(&specs) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(matches!(err, HookError::InvalidSpec { hook_id, .. } if hook_id == "no-budget"));
    }

    #[tokio::test]
    async fn dispatch_records_execution_hashes() {
        let chain = PromptHookChain::new(vec![Box::new(StaticPromptHook {
            name: "test-hook",
            points: vec![PromptHookPoint::PostContextCollect],
            decision: PromptHookDecision::AddSection {
                section: section("added"),
                budget: TokenBudget::Fixed(10),
            },
        })]);

        let ctx = PromptHookContext::new(PromptHookPoint::PostContextCollect, "req-1", "hello");
        let outcome = chain.dispatch(ctx).await.unwrap();

        assert_eq!(outcome.executions.len(), 1);
        assert_eq!(outcome.executions[0].hook_name, "test-hook");
        assert!(!outcome.executions[0].before_hash.is_empty());
        assert!(!outcome.executions[0].after_hash.is_empty());
        // after_hash differs from before_hash because a section was added
        assert_ne!(
            outcome.executions[0].before_hash,
            outcome.executions[0].after_hash
        );
    }

    #[tokio::test]
    async fn dispatch_empty_executions_for_no_matching_hooks() {
        let chain = PromptHookChain::new(vec![Box::new(StaticPromptHook {
            name: "wrong-point",
            points: vec![PromptHookPoint::PreToolCall],
            decision: PromptHookDecision::Continue,
        })]);

        let ctx = PromptHookContext::new(PromptHookPoint::PostContextCollect, "req-1", "hello");
        let outcome = chain.dispatch(ctx).await.unwrap();

        assert_eq!(outcome.executions.len(), 0);
    }
}
