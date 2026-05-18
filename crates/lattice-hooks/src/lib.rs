//! Lightweight lifecycle hook vocabulary for the runtime.
//!
//! This crate intentionally owns only hook points, hook payloads, dispatch, and
//! registration. Hook payloads are local typed wrappers around
//! `serde_json::Value`; prompt compilation, token estimation, and provider
//! internals live outside this crate.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Lifecycle points where runtime code may call hooks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    AgentTurnStart,
    AgentTurnEnd,
    ToolCallStart,
    ToolCallEnd,
    InferenceStart,
    InferenceEnd,
    SessionStart,
    SessionEnd,
    Custom(String),
}

/// Context passed to a hook when it fires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookContext {
    pub point: HookPoint,
    pub target: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl HookContext {
    pub fn new(point: HookPoint, target: impl Into<String>) -> Self {
        Self {
            point,
            target: target.into(),
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Decision returned by a hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", content = "payload", rename_all = "snake_case")]
pub enum HookDecision {
    Continue,
    Modify(serde_json::Value),
    Abort(String),
}

/// A single hook execution record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookExecutionRecord {
    pub hook_name: String,
    pub point: HookPoint,
    pub decision: HookDecision,
}

/// Result of dispatching a context through a hook chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookDispatch {
    pub context: HookContext,
    pub executions: Vec<HookExecutionRecord>,
}

/// Trait for a lifecycle hook.
#[async_trait]
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;

    fn points(&self) -> Vec<HookPoint>;

    async fn execute(&self, context: &HookContext) -> Result<HookDecision, HookError>;
}

/// Ordered in-process hook chain.
#[derive(Default, Clone)]
pub struct HookChain {
    hooks: Vec<Arc<dyn Hook>>,
}

impl HookChain {
    pub fn new(hooks: Vec<Box<dyn Hook>>) -> Self {
        Self {
            hooks: hooks.into_iter().map(Arc::from).collect(),
        }
    }

    pub fn from_shared(hooks: Vec<Arc<dyn Hook>>) -> Self {
        Self { hooks }
    }

    pub fn push(&mut self, hook: Box<dyn Hook>) {
        self.hooks.push(Arc::from(hook));
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub fn list(&self) -> Vec<String> {
        self.hooks
            .iter()
            .map(|hook| hook.name().to_string())
            .collect()
    }

    pub async fn dispatch(&self, context: HookContext) -> Result<HookDispatch, HookError> {
        let mut context = context;
        let mut executions = Vec::new();

        for hook in &self.hooks {
            if !hook.points().contains(&context.point) {
                continue;
            }

            let decision = hook.execute(&context).await?;
            executions.push(HookExecutionRecord {
                hook_name: hook.name().to_string(),
                point: context.point.clone(),
                decision: decision.clone(),
            });

            match decision {
                HookDecision::Continue => {}
                HookDecision::Modify(metadata) => {
                    context.metadata = metadata;
                }
                HookDecision::Abort(reason) => {
                    return Err(HookError::Aborted {
                        hook: hook.name().to_string(),
                        point: context.point,
                        reason,
                    });
                }
            }
        }

        Ok(HookDispatch {
            context,
            executions,
        })
    }
}

/// Trait for hook registration and dispatch.
#[async_trait]
pub trait HookRegistry: Send + Sync {
    async fn register(&self, hook: Box<dyn Hook>) -> Result<(), HookError>;

    async fn unregister(&self, name: &str) -> Result<(), HookError>;

    async fn dispatch(&self, context: HookContext) -> Result<HookDispatch, HookError>;

    fn list(&self) -> Vec<String>;
}

/// Simple thread-safe in-memory registry for embedded runtime use.
#[derive(Default)]
pub struct InMemoryHookRegistry {
    hooks: RwLock<Vec<Arc<dyn Hook>>>,
}

impl InMemoryHookRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl HookRegistry for InMemoryHookRegistry {
    async fn register(&self, hook: Box<dyn Hook>) -> Result<(), HookError> {
        let name = hook.name().to_string();
        let mut hooks = self
            .hooks
            .write()
            .map_err(|_| HookError::ExecutionFailed("hook registry lock poisoned".into()))?;

        if hooks.iter().any(|existing| existing.name() == name) {
            return Err(HookError::Duplicate(name));
        }

        hooks.push(Arc::from(hook));
        Ok(())
    }

    async fn unregister(&self, name: &str) -> Result<(), HookError> {
        let mut hooks = self
            .hooks
            .write()
            .map_err(|_| HookError::ExecutionFailed("hook registry lock poisoned".into()))?;
        let index = hooks
            .iter()
            .position(|hook| hook.name() == name)
            .ok_or_else(|| HookError::NotFound(name.to_string()))?;
        hooks.remove(index);
        Ok(())
    }

    async fn dispatch(&self, context: HookContext) -> Result<HookDispatch, HookError> {
        let hooks = self
            .hooks
            .read()
            .map_err(|_| HookError::ExecutionFailed("hook registry lock poisoned".into()))?
            .clone();
        HookChain::from_shared(hooks).dispatch(context).await
    }

    fn list(&self) -> Vec<String> {
        self.hooks
            .read()
            .map(|hooks| hooks.iter().map(|hook| hook.name().to_string()).collect())
            .unwrap_or_default()
    }
}

/// Errors from hook operations.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("hook not found: {0}")]
    NotFound(String),
    #[error("duplicate hook: {0}")]
    Duplicate(String),
    #[error("hook execution failed: {0}")]
    ExecutionFailed(String),
    #[error("hook '{hook}' aborted {point:?}: {reason}")]
    Aborted {
        hook: String,
        point: HookPoint,
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticHook {
        name: &'static str,
        points: Vec<HookPoint>,
        decision: HookDecision,
    }

    #[async_trait]
    impl Hook for StaticHook {
        fn name(&self) -> &str {
            self.name
        }

        fn points(&self) -> Vec<HookPoint> {
            self.points.clone()
        }

        async fn execute(&self, _context: &HookContext) -> Result<HookDecision, HookError> {
            Ok(self.decision.clone())
        }
    }

    struct ObservingHook {
        name: &'static str,
    }

    #[async_trait]
    impl Hook for ObservingHook {
        fn name(&self) -> &str {
            self.name
        }

        fn points(&self) -> Vec<HookPoint> {
            vec![HookPoint::ToolCallStart]
        }

        async fn execute(&self, context: &HookContext) -> Result<HookDecision, HookError> {
            let previous = context.metadata["value"].as_i64().unwrap_or(0);
            Ok(HookDecision::Modify(serde_json::json!({
                "value": previous + 1,
                "target": context.target
            })))
        }
    }

    #[tokio::test]
    async fn dispatch_runs_matching_hooks_in_registration_order() {
        let chain = HookChain::new(vec![
            Box::new(StaticHook {
                name: "first",
                points: vec![HookPoint::ToolCallStart],
                decision: HookDecision::Modify(serde_json::json!({"value": 1})),
            }),
            Box::new(ObservingHook { name: "second" }),
        ]);

        let dispatch = chain
            .dispatch(HookContext::new(HookPoint::ToolCallStart, "read_file"))
            .await
            .unwrap();

        assert_eq!(dispatch.executions.len(), 2);
        assert_eq!(dispatch.executions[0].hook_name, "first");
        assert_eq!(dispatch.executions[1].hook_name, "second");
        assert_eq!(dispatch.context.metadata["value"], 2);
        assert_eq!(dispatch.context.metadata["target"], "read_file");
    }

    #[tokio::test]
    async fn dispatch_skips_non_matching_hooks() {
        let chain = HookChain::new(vec![Box::new(StaticHook {
            name: "wrong-point",
            points: vec![HookPoint::SessionStart],
            decision: HookDecision::Abort("should not run".into()),
        })]);

        let dispatch = chain
            .dispatch(HookContext::new(HookPoint::ToolCallStart, "grep"))
            .await
            .unwrap();

        assert!(dispatch.executions.is_empty());
        assert_eq!(dispatch.context.target, "grep");
    }

    #[tokio::test]
    async fn dispatch_returns_abort_error() {
        let chain = HookChain::new(vec![Box::new(StaticHook {
            name: "deny-shell",
            points: vec![HookPoint::ToolCallStart],
            decision: HookDecision::Abort("shell blocked".into()),
        })]);

        let err = chain
            .dispatch(HookContext::new(HookPoint::ToolCallStart, "bash"))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            HookError::Aborted {
                hook,
                reason,
                ..
            } if hook == "deny-shell" && reason == "shell blocked"
        ));
    }

    #[tokio::test]
    async fn registry_registers_dispatches_lists_and_unregisters() {
        let registry = InMemoryHookRegistry::new();
        registry
            .register(Box::new(StaticHook {
                name: "session-audit",
                points: vec![HookPoint::SessionStart],
                decision: HookDecision::Modify(serde_json::json!({"seen": true})),
            }))
            .await
            .unwrap();

        assert_eq!(registry.list(), vec!["session-audit".to_string()]);

        let dispatch = registry
            .dispatch(HookContext::new(HookPoint::SessionStart, "session-1"))
            .await
            .unwrap();
        assert_eq!(dispatch.context.metadata["seen"], true);

        registry.unregister("session-audit").await.unwrap();
        assert!(registry.list().is_empty());
    }

    #[tokio::test]
    async fn registry_rejects_duplicate_and_missing_hooks() {
        let registry = InMemoryHookRegistry::new();
        registry
            .register(Box::new(StaticHook {
                name: "dupe",
                points: vec![HookPoint::AgentTurnStart],
                decision: HookDecision::Continue,
            }))
            .await
            .unwrap();

        let duplicate = registry
            .register(Box::new(StaticHook {
                name: "dupe",
                points: vec![HookPoint::AgentTurnEnd],
                decision: HookDecision::Continue,
            }))
            .await
            .unwrap_err();
        assert!(matches!(duplicate, HookError::Duplicate(name) if name == "dupe"));

        let missing = registry.unregister("missing").await.unwrap_err();
        assert!(matches!(missing, HookError::NotFound(name) if name == "missing"));
    }
}
