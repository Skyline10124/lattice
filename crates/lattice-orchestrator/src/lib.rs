//! LATTICE micro-agent bus and TOML pipeline orchestration.
//!
//! This crate owns Pipeline/AgentRunner/profile assembly for inter-agent
//! orchestration, plus the async RPC + pub/sub bus types used by micro-agents.

pub mod agent_executor;
pub mod agent_spec;
pub mod assembly;
pub mod events;
pub mod handoff_rule;
pub mod json_output;
pub mod lattice_dir;
pub mod micro_agent;
pub mod pipeline;
pub mod profile;
pub mod registry;
pub mod runner;
pub mod watcher;

#[cfg(feature = "axum")]
pub mod ws;

pub use agent_executor::{AgentExecutor, ExecutionOutput};
pub use agent_spec::{
    lower_profile_to_turn_spec, AgentPreset, AgentTurnExecutionError, AgentTurnExecutor,
    AgentTurnOutput, AgentTurnRequest, AgentTurnSpec, AgentTurnStatus, ProfileLoweringError,
};
pub use assembly::{
    assemble_agent_with_options, AgentAssemblyOptions, AgentFactory, AssembledAgent,
    ToolSchemaFactory,
};
pub use events::{EventBus, PipelineEvent};
pub use handoff_rule::{HandoffCondition, HandoffRule, HandoffTarget};
pub use lattice_dir::{LatticeDir, OrchestratorToml};
pub use micro_agent::{MicroAgent, MicroAgentHandle};
pub use pipeline::{AgentError, AgentResult, DryRunReport, Pipeline, PipelineRun};
pub use profile::{
    AgentConfig, AgentProfile, BehaviorConfig, CapabilitiesConfig, DagEdgeConfig, HandoffConfig,
    MemoryConfigProfile, OrchestratorConfigProfile, PluginSlotConfig, PluginsConfig, SystemConfig,
    ToolsConfig,
};
pub use registry::AgentRegistry;
pub use runner::AgentRunner;
pub use watcher::Watcher;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, RwLock, Semaphore};

// ---------------------------------------------------------------------------
// AgentId
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// OrchestratorError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("agent not found: {0}")]
    AgentNotFound(AgentId),
    #[error("agent already registered: {0}")]
    AlreadyRegistered(AgentId),
    #[error("RPC timeout after {0:?}")]
    Timeout(Duration),
    #[error("channel closed")]
    ChannelClosed,
    #[error("serialization error: {0}")]
    Serialize(String),
    #[error("model resolution error: {0}")]
    Resolve(String),
    #[error("agent configuration error: {0}")]
    Config(String),
    #[error("unauthorized: agent {0} not in caller's rpc whitelist")]
    Unauthorized(AgentId),
    #[error("unauthorized: agent {0} not allowed to subscribe to topic {1}")]
    SubscribeUnauthorized(AgentId, String),
    #[error("unauthorized: agent {0} not allowed to publish to topic {1}")]
    PublishUnauthorized(AgentId, String),
}

// ---------------------------------------------------------------------------
// OrchestratorRequest / OrchestratorResponse / OrchestratorEvent
// ---------------------------------------------------------------------------

/// RPC request — payload + oneshot reply channel (D9).
pub struct OrchestratorRequest {
    pub(crate) payload: serde_json::Value,
    pub(crate) reply_to: oneshot::Sender<Result<OrchestratorResponse, OrchestratorError>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorResponse {
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorEvent {
    pub topic: String,
    pub source: AgentId,
    pub payload: serde_json::Value,
}

// ---------------------------------------------------------------------------
// AgentDescriptor / AgentOrchestratorConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub id: AgentId,
    pub name: String,
    pub capabilities: Vec<String>,
    pub orchestrator_config: AgentOrchestratorConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOrchestratorConfig {
    pub subscribe: Vec<String>,
    pub publish: Vec<String>,
    pub rpc: Vec<AgentId>,
}

// ---------------------------------------------------------------------------
// Registration — result of bus.register()
// ---------------------------------------------------------------------------

/// Returned by register(). Caller owns request_rx and spawns their own agent loop.
pub struct Registration {
    pub id: AgentId,
    pub request_rx: mpsc::Receiver<OrchestratorRequest>,
}

// ---------------------------------------------------------------------------
// OrchestratorConfig / DeliveryPolicy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub timeout_rpc: Duration,
    pub delivery_policy: DeliveryPolicy,
    pub subscriber_buffer: usize,
    pub max_concurrent_rpc: usize,
    pub channel_buffer_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryPolicy {
    /// Fire-and-forget: the publisher does not wait for delivery confirmation.
    /// Handler errors are silently discarded.
    AtMostOnce,
    /// Best-effort retry: handler errors are logged. Does NOT guarantee
    /// persistent retry or redelivery — use for observability, not for
    /// transactional correctness.
    AtLeastOnce,
}

const AT_LEAST_ONCE_PUBLISH_ATTEMPTS: usize = 3;
const AT_LEAST_ONCE_RETRY_DELAY: Duration = Duration::from_millis(50);

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            timeout_rpc: Duration::from_secs(30),
            delivery_policy: DeliveryPolicy::AtMostOnce,
            subscriber_buffer: 1024,
            max_concurrent_rpc: 32,
            channel_buffer_size: 1024,
        }
    }
}

// ---------------------------------------------------------------------------
// Orchestrator trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait Orchestrator: Send + Sync {
    /// Register an agent. Returns Registration with request channel receiver.
    /// The caller owns request_rx and spawns their own agent loop task.
    async fn register(&self, agent: AgentDescriptor) -> Result<Registration, OrchestratorError>;
    async fn discover(&self, capability: &str) -> Vec<AgentDescriptor>;
    async fn deregister(&self, id: &AgentId) -> Result<(), OrchestratorError>;

    async fn subscribe(
        &self,
        topic: &str,
        agent_id: Option<AgentId>,
        handler: OrchestratorHandlerFn,
    ) -> Result<(), OrchestratorError>;
    async fn unsubscribe(&self, topic: &str) -> Result<(), OrchestratorError>;
    async fn publish(&self, topic: &str, event: OrchestratorEvent)
        -> Result<(), OrchestratorError>;

    async fn call(
        &self,
        caller: &AgentId,
        target: &AgentId,
        request: serde_json::Value,
    ) -> Result<OrchestratorResponse, OrchestratorError>;
    async fn call_with_timeout(
        &self,
        caller: &AgentId,
        target: &AgentId,
        request: serde_json::Value,
        timeout: Duration,
    ) -> Result<OrchestratorResponse, OrchestratorError>;
}

// ---------------------------------------------------------------------------
// Subscriber entry
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct SubscriberEntry {
    pub handler: OrchestratorHandlerFn,
}

// ---------------------------------------------------------------------------
// OrchestratorHandlerFn
// ---------------------------------------------------------------------------

pub type OrchestratorHandlerFn = Arc<
    dyn Fn(
            OrchestratorEvent,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), OrchestratorError>> + Send>,
        > + Send
        + Sync,
>;

pub fn orchestrator_handler(
    f: impl Fn(
            OrchestratorEvent,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), OrchestratorError>> + Send>,
        > + Send
        + Sync
        + 'static,
) -> OrchestratorHandlerFn {
    Arc::new(f)
}

/// Macro for constructing OrchestratorHandlerFn with async block syntax.
/// Usage: `orchestrator_handler!(|event| { /* async body */ })`
#[macro_export]
macro_rules! orchestrator_handler {
    ($handler:expr) => {
        $crate::orchestrator_handler($handler)
    };
}

// ---------------------------------------------------------------------------
// Registry trait definitions (merged from lattice-registry per D-02)
// ---------------------------------------------------------------------------

/// Unique identifier for a registry node in the federation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

/// Metadata about a registered micro-agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetadata {
    pub id: AgentId,
    pub name: String,
    pub capabilities: Vec<String>,
    pub node: NodeId,
}

/// Trait for micro-agent registration and discovery (renamed from AgentRegistry per D-05).
#[async_trait]
pub trait RegistryService: Send + Sync {
    /// Register a micro-agent with the registry.
    async fn register(&self, agent: AgentMetadata) -> Result<(), RegistryError>;
    /// Unregister a micro-agent.
    async fn unregister(&self, id: &AgentId) -> Result<(), RegistryError>;
    /// Discover agents matching a capability query.
    async fn discover(&self, capability: &str) -> Result<Vec<AgentMetadata>, RegistryError>;
    /// List all registered agents.
    async fn list(&self) -> Result<Vec<AgentMetadata>, RegistryError>;
}

/// Trait for federation protocol — node discovery and cross-node communication.
#[async_trait]
pub trait FederationProtocol: Send + Sync {
    /// Announce this node to the federation.
    async fn announce(&self, node: NodeId) -> Result<(), RegistryError>;
    /// Discover other nodes in the federation.
    async fn discover_nodes(&self) -> Result<Vec<NodeId>, RegistryError>;
    /// Send a message to a remote node.
    async fn send(&self, to: &NodeId, message: &[u8]) -> Result<(), RegistryError>;
}

/// Trait for auto-solver — automatic dependency resolution and orchestration.
#[async_trait]
pub trait AutoSolver: Send + Sync {
    /// Resolve dependencies for a given task and return an execution plan.
    async fn solve(&self, task: &str, context: &[AgentId]) -> Result<ExecutionPlan, RegistryError>;
}

/// An execution plan produced by the auto-solver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub steps: Vec<ExecutionStep>,
}

/// A single step in an execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStep {
    pub agent: AgentId,
    pub action: String,
    pub depends_on: Vec<usize>,
}

/// Errors from registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error("registration failed: {0}")]
    RegistrationFailed(String),
    #[error("federation error: {0}")]
    Federation(String),
    #[error("solver error: {0}")]
    Solver(String),
}

// ---------------------------------------------------------------------------
// AgentLoop — default echo handler for testing
// ---------------------------------------------------------------------------

/// Simple agent loop that echoes request payloads back. Useful for testing.
/// Real agents (harness) will replace this with LLM-backed processing.
pub async fn echo_agent_loop(mut rx: mpsc::Receiver<OrchestratorRequest>) {
    while let Some(req) = rx.recv().await {
        let resp = OrchestratorResponse {
            payload: req.payload.clone(),
        };
        if req.reply_to.send(Ok(resp)).is_err() {
            tracing::warn!("echo_agent_loop: reply channel closed, caller timed out");
        }
    }
}

// ---------------------------------------------------------------------------
// InMemoryOrchestrator
// ---------------------------------------------------------------------------

struct AgentEntry {
    descriptor: AgentDescriptor,
    request_tx: mpsc::Sender<OrchestratorRequest>,
}

pub struct InMemoryOrchestrator {
    config: OrchestratorConfig,
    agents: RwLock<HashMap<AgentId, AgentEntry>>,
    subscriptions: RwLock<HashMap<String, Vec<SubscriberEntry>>>,
    rpc_limiter: Arc<Semaphore>,
}

impl InMemoryOrchestrator {
    pub fn new(config: OrchestratorConfig) -> Self {
        let max_concurrent_rpc = config.max_concurrent_rpc;
        Self {
            config,
            agents: RwLock::new(HashMap::new()),
            subscriptions: RwLock::new(HashMap::new()),
            rpc_limiter: Arc::new(Semaphore::new(max_concurrent_rpc)),
        }
    }
    pub fn with_defaults() -> Self {
        Self::new(OrchestratorConfig::default())
    }
}

#[async_trait]
impl Orchestrator for InMemoryOrchestrator {
    async fn register(&self, agent: AgentDescriptor) -> Result<Registration, OrchestratorError> {
        let id = agent.id.clone();
        let mut agents = self.agents.write().await;
        if agents.contains_key(&id) {
            return Err(OrchestratorError::AlreadyRegistered(id));
        }
        let (request_tx, request_rx) = mpsc::channel(self.config.channel_buffer_size);
        let entry = AgentEntry {
            descriptor: agent,
            request_tx,
        };
        agents.insert(id.clone(), entry);
        Ok(Registration { id, request_rx })
    }

    async fn discover(&self, capability: &str) -> Vec<AgentDescriptor> {
        self.agents
            .read()
            .await
            .values()
            .filter(|e| e.descriptor.capabilities.iter().any(|c| c == capability))
            .map(|e| e.descriptor.clone())
            .collect()
    }

    async fn deregister(&self, id: &AgentId) -> Result<(), OrchestratorError> {
        if self.agents.write().await.remove(id).is_some() {
            Ok(())
        } else {
            Err(OrchestratorError::AgentNotFound(id.clone()))
        }
    }

    async fn subscribe(
        &self,
        topic: &str,
        agent_id: Option<AgentId>,
        handler: OrchestratorHandlerFn,
    ) -> Result<(), OrchestratorError> {
        // Enforce subscribe capability: agent_id is required and must be
        // listed in its bus_config.subscribe for the requested topic.
        if let Some(ref aid) = agent_id {
            let agents = self.agents.read().await;
            if let Some(entry) = agents.get(aid) {
                let allowed = &entry.descriptor.orchestrator_config.subscribe;
                if !allowed.is_empty() && !allowed.iter().any(|t| t == topic) {
                    return Err(OrchestratorError::SubscribeUnauthorized(
                        aid.clone(),
                        topic.to_string(),
                    ));
                }
            }
        }
        self.subscriptions
            .write()
            .await
            .entry(topic.to_string())
            .or_default()
            .push(SubscriberEntry { handler });
        Ok(())
    }

    async fn unsubscribe(&self, topic: &str) -> Result<(), OrchestratorError> {
        self.subscriptions.write().await.remove(topic);
        Ok(())
    }

    async fn publish(
        &self,
        topic: &str,
        event: OrchestratorEvent,
    ) -> Result<(), OrchestratorError> {
        // Enforce publish capability: source agent must be registered and
        // the topic must appear in its bus_config.publish list.
        {
            let agents = self.agents.read().await;
            if let Some(entry) = agents.get(&event.source) {
                let allowed = &entry.descriptor.orchestrator_config.publish;
                if !allowed.is_empty() && !allowed.iter().any(|t| t == topic) {
                    return Err(OrchestratorError::PublishUnauthorized(
                        event.source.clone(),
                        topic.to_string(),
                    ));
                }
            }
        }
        if let Some(entries) = self.subscriptions.read().await.get(topic) {
            let policy = self.config.delivery_policy;
            for entry in entries {
                let h = entry.handler.clone();
                let evt = event.clone();
                tokio::spawn(async move {
                    let attempts = match policy {
                        DeliveryPolicy::AtMostOnce => 1,
                        DeliveryPolicy::AtLeastOnce => AT_LEAST_ONCE_PUBLISH_ATTEMPTS,
                    };
                    for attempt in 1..=attempts {
                        match h(evt.clone()).await {
                            Ok(()) => return,
                            Err(e) => {
                                if policy == DeliveryPolicy::AtMostOnce || attempt == attempts {
                                    tracing::warn!("orchestrator handler error: {}", e);
                                    return;
                                }
                                tracing::warn!(
                                    "orchestrator handler error on attempt {attempt}/{attempts}: {e}"
                                );
                                tokio::time::sleep(AT_LEAST_ONCE_RETRY_DELAY).await;
                            }
                        }
                    }
                });
            }
        }
        Ok(())
    }

    async fn call(
        &self,
        caller: &AgentId,
        target: &AgentId,
        request: serde_json::Value,
    ) -> Result<OrchestratorResponse, OrchestratorError> {
        self.call_with_timeout(caller, target, request, self.config.timeout_rpc)
            .await
    }

    async fn call_with_timeout(
        &self,
        caller: &AgentId,
        target: &AgentId,
        request: serde_json::Value,
        timeout: Duration,
    ) -> Result<OrchestratorResponse, OrchestratorError> {
        // Authorization check + clone sender under read lock, then release.
        let request_tx = {
            let agents = self.agents.read().await;
            let caller_e = agents
                .get(caller)
                .ok_or(OrchestratorError::AgentNotFound(caller.clone()))?;
            if !caller_e.descriptor.orchestrator_config.rpc.contains(target) {
                return Err(OrchestratorError::Unauthorized(target.clone()));
            }
            let target_e = agents
                .get(target)
                .ok_or(OrchestratorError::AgentNotFound(target.clone()))?;
            target_e.request_tx.clone()
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        // Wrap both send and reply under a single timeout budget, so
        // a saturated target channel doesn't block indefinitely.
        let limiter = self.rpc_limiter.clone();
        let send_and_reply = async move {
            let _permit = limiter
                .acquire_owned()
                .await
                .map_err(|_| OrchestratorError::ChannelClosed)?;
            request_tx
                .send(OrchestratorRequest {
                    payload: request,
                    reply_to: reply_tx,
                })
                .await
                .map_err(|_| OrchestratorError::ChannelClosed)?;
            reply_rx.await.map_err(|_| OrchestratorError::ChannelClosed)
        };

        match tokio::time::timeout(timeout, send_and_reply).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => Err(e),
            Err(_) => Err(OrchestratorError::Timeout(timeout)),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: register an agent and spawn echo loop for testing.
    async fn register_echo(bus: &InMemoryOrchestrator, desc: AgentDescriptor) -> AgentId {
        let reg = bus.register(desc).await.unwrap();
        tokio::spawn(echo_agent_loop(reg.request_rx));
        reg.id
    }

    #[tokio::test]
    async fn test_register_and_discover() {
        let bus = InMemoryOrchestrator::with_defaults();
        let id = register_echo(
            &bus,
            AgentDescriptor {
                id: AgentId::new("reviewer"),
                name: "Code Reviewer".into(),
                capabilities: vec!["code-review".into()],
                orchestrator_config: AgentOrchestratorConfig::default(),
            },
        )
        .await;
        assert_eq!(id, AgentId::new("reviewer"));

        let found = bus.discover("code-review").await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, AgentId::new("reviewer"));
    }

    #[tokio::test]
    async fn test_discover_no_match() {
        let bus = InMemoryOrchestrator::with_defaults();
        assert!(bus.discover("nonexistent").await.is_empty());
    }

    #[tokio::test]
    async fn test_deregister() {
        let bus = InMemoryOrchestrator::with_defaults();
        let id = register_echo(
            &bus,
            AgentDescriptor {
                id: AgentId::new("temp"),
                name: "Temp".into(),
                capabilities: vec!["temp".into()],
                orchestrator_config: AgentOrchestratorConfig::default(),
            },
        )
        .await;
        bus.deregister(&id).await.unwrap();
        assert!(bus.discover("temp").await.is_empty());
    }

    #[tokio::test]
    async fn test_deregister_nonexistent() {
        let bus = InMemoryOrchestrator::with_defaults();
        let r = bus.deregister(&AgentId::new("ghost")).await;
        assert!(matches!(r, Err(OrchestratorError::AgentNotFound(_))));
    }

    #[tokio::test]
    async fn test_rpc_call_success() {
        let bus = InMemoryOrchestrator::with_defaults();

        register_echo(
            &bus,
            AgentDescriptor {
                id: AgentId::new("reviewer"),
                name: "Reviewer".into(),
                capabilities: vec!["code-review".into()],
                orchestrator_config: AgentOrchestratorConfig {
                    rpc: vec![AgentId::new("refactorer")],
                    ..Default::default()
                },
            },
        )
        .await;

        register_echo(
            &bus,
            AgentDescriptor {
                id: AgentId::new("refactorer"),
                name: "Refactorer".into(),
                capabilities: vec!["refactor".into()],
                orchestrator_config: AgentOrchestratorConfig::default(),
            },
        )
        .await;

        let resp = bus
            .call(
                &AgentId::new("reviewer"),
                &AgentId::new("refactorer"),
                serde_json::json!({"code": "fn main() {}"}),
            )
            .await
            .unwrap();

        assert_eq!(resp.payload["code"], "fn main() {}");
    }

    #[tokio::test]
    async fn test_rpc_unauthorized() {
        let bus = InMemoryOrchestrator::with_defaults();

        register_echo(
            &bus,
            AgentDescriptor {
                id: AgentId::new("reviewer"),
                name: "Reviewer".into(),
                capabilities: vec!["code-review".into()],
                orchestrator_config: AgentOrchestratorConfig::default(),
            },
        )
        .await;

        register_echo(
            &bus,
            AgentDescriptor {
                id: AgentId::new("refactorer"),
                name: "Refactorer".into(),
                capabilities: vec!["refactor".into()],
                orchestrator_config: AgentOrchestratorConfig::default(),
            },
        )
        .await;

        let r = bus
            .call(
                &AgentId::new("reviewer"),
                &AgentId::new("refactorer"),
                serde_json::json!({}),
            )
            .await;
        assert!(matches!(r, Err(OrchestratorError::Unauthorized(_))));
    }

    #[tokio::test]
    async fn test_rpc_target_not_found() {
        let bus = InMemoryOrchestrator::with_defaults();
        register_echo(
            &bus,
            AgentDescriptor {
                id: AgentId::new("reviewer"),
                name: "Reviewer".into(),
                capabilities: vec!["code-review".into()],
                orchestrator_config: AgentOrchestratorConfig {
                    rpc: vec![AgentId::new("ghost")],
                    ..Default::default()
                },
            },
        )
        .await;

        let r = bus
            .call(
                &AgentId::new("reviewer"),
                &AgentId::new("ghost"),
                serde_json::json!({}),
            )
            .await;
        assert!(matches!(r, Err(OrchestratorError::AgentNotFound(_))));
    }

    #[tokio::test]
    async fn test_rpc_caller_not_found() {
        let bus = InMemoryOrchestrator::with_defaults();
        let r = bus
            .call(
                &AgentId::new("ghost"),
                &AgentId::new("refactorer"),
                serde_json::json!({}),
            )
            .await;
        assert!(matches!(r, Err(OrchestratorError::AgentNotFound(_))));
    }

    #[tokio::test]
    async fn test_rpc_timeout() {
        let bus = InMemoryOrchestrator::new(OrchestratorConfig {
            timeout_rpc: Duration::from_millis(50),
            ..Default::default()
        });

        register_echo(
            &bus,
            AgentDescriptor {
                id: AgentId::new("caller"),
                name: "Caller".into(),
                capabilities: vec!["call".into()],
                orchestrator_config: AgentOrchestratorConfig {
                    rpc: vec![AgentId::new("slow")],
                    ..Default::default()
                },
            },
        )
        .await;

        // Register slow agent that never responds.
        let reg = bus
            .register(AgentDescriptor {
                id: AgentId::new("slow"),
                name: "Slow".into(),
                capabilities: vec!["slow".into()],
                orchestrator_config: AgentOrchestratorConfig::default(),
            })
            .await
            .unwrap();
        // Don't spawn echo loop — request_rx drops, channel closes.
        drop(reg.request_rx);

        let r = bus
            .call_with_timeout(
                &AgentId::new("caller"),
                &AgentId::new("slow"),
                serde_json::json!({}),
                Duration::from_millis(50),
            )
            .await;
        assert!(matches!(
            r,
            Err(OrchestratorError::ChannelClosed | OrchestratorError::Timeout(_))
        ));
    }

    #[tokio::test]
    async fn test_pub_sub_basic() {
        let bus = InMemoryOrchestrator::with_defaults();
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let rc = received.clone();

        bus.subscribe(
            "code-changes",
            None,
            orchestrator_handler(move |event: OrchestratorEvent| {
                let r = rc.clone();
                Box::pin(async move {
                    r.lock().await.push(event.payload);
                    Ok(())
                })
            }),
        )
        .await
        .unwrap();

        bus.publish(
            "code-changes",
            OrchestratorEvent {
                topic: "code-changes".into(),
                source: AgentId::new("watcher"),
                payload: serde_json::json!({"file": "main.rs"}),
            },
        )
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        let items = received.lock().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["file"], "main.rs");
    }

    #[tokio::test]
    async fn test_pub_sub_no_subscribers() {
        let bus = InMemoryOrchestrator::with_defaults();
        let r = bus
            .publish(
                "orphan-topic",
                OrchestratorEvent {
                    topic: "orphan-topic".into(),
                    source: AgentId::new("sender"),
                    payload: serde_json::json!({}),
                },
            )
            .await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn test_orchestrator_config_defaults() {
        let c = OrchestratorConfig::default();
        assert_eq!(c.timeout_rpc, Duration::from_secs(30));
        assert_eq!(c.delivery_policy, DeliveryPolicy::AtMostOnce);
        assert_eq!(c.subscriber_buffer, 1024);
        assert_eq!(c.max_concurrent_rpc, 32);
        assert_eq!(c.channel_buffer_size, 1024);
    }

    #[tokio::test]
    async fn test_registration_yields_request_rx() {
        let bus = InMemoryOrchestrator::with_defaults();
        let reg = bus
            .register(AgentDescriptor {
                id: AgentId::new("agent-a"),
                name: "A".into(),
                capabilities: vec!["test".into()],
                orchestrator_config: AgentOrchestratorConfig::default(),
            })
            .await
            .unwrap();

        assert_eq!(reg.id, AgentId::new("agent-a"));
        // request_rx is usable — caller can spawn their own agent loop.
        tokio::spawn(echo_agent_loop(reg.request_rx));
    }

    #[tokio::test]
    async fn test_orchestrator_handler_function() {
        let bus = InMemoryOrchestrator::with_defaults();
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let rc = received.clone();

        let handler = orchestrator_handler(move |event: OrchestratorEvent| {
            let r = rc.clone();
            Box::pin(async move {
                r.lock().await.push(event.topic);
                Ok(())
            })
        });

        bus.subscribe("test-topic", None, handler).await.unwrap();
        bus.publish(
            "test-topic",
            OrchestratorEvent {
                topic: "test-topic".into(),
                source: AgentId::new("sender"),
                payload: serde_json::json!({}),
            },
        )
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        let items = received.lock().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], "test-topic");
    }

    #[tokio::test]
    async fn test_orchestrator_handler_macro() {
        let bus = InMemoryOrchestrator::with_defaults();
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let rc = received.clone();

        let handler = orchestrator_handler!(move |event: OrchestratorEvent| {
            let r = rc.clone();
            Box::pin(async move {
                r.lock().await.push(event.source.to_string());
                Ok(())
            })
        });

        bus.subscribe("macro-topic", None, handler).await.unwrap();
        bus.publish(
            "macro-topic",
            OrchestratorEvent {
                topic: "macro-topic".into(),
                source: AgentId::new("macro-sender"),
                payload: serde_json::json!({}),
            },
        )
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        let items = received.lock().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], "macro-sender");
    }

    #[tokio::test]
    async fn test_register_duplicate_rejected() {
        let bus = InMemoryOrchestrator::with_defaults();
        let desc = AgentDescriptor {
            id: AgentId::new("dup-agent"),
            name: "Dup".into(),
            capabilities: vec![],
            orchestrator_config: AgentOrchestratorConfig::default(),
        };
        bus.register(desc.clone()).await.unwrap();
        match bus.register(desc).await {
            Err(OrchestratorError::AlreadyRegistered(id)) => {
                assert_eq!(id, AgentId::new("dup-agent"))
            }
            Err(other) => panic!("expected AlreadyRegistered, got {:?}", other),
            Ok(_) => panic!("expected error for duplicate registration"),
        }
    }

    #[tokio::test]
    async fn test_subscribe_capability_enforced() {
        let bus = InMemoryOrchestrator::with_defaults();
        bus.register(AgentDescriptor {
            id: AgentId::new("sub-agent"),
            name: "Sub".into(),
            capabilities: vec![],
            orchestrator_config: AgentOrchestratorConfig {
                subscribe: vec!["allowed-topic".into()],
                ..Default::default()
            },
        })
        .await
        .unwrap();

        // Allowed topic
        bus.subscribe(
            "allowed-topic",
            Some(AgentId::new("sub-agent")),
            orchestrator_handler(|_evt| Box::pin(async { Ok(()) })),
        )
        .await
        .unwrap();

        // Disallowed topic
        let r = bus
            .subscribe(
                "forbidden-topic",
                Some(AgentId::new("sub-agent")),
                orchestrator_handler(|_evt| Box::pin(async { Ok(()) })),
            )
            .await;
        assert!(matches!(
            r,
            Err(OrchestratorError::SubscribeUnauthorized(_, _))
        ));
    }

    #[tokio::test]
    async fn test_publish_capability_enforced() {
        let bus = InMemoryOrchestrator::with_defaults();
        bus.register(AgentDescriptor {
            id: AgentId::new("pub-agent"),
            name: "Pub".into(),
            capabilities: vec![],
            orchestrator_config: AgentOrchestratorConfig {
                publish: vec!["allowed-topic".into()],
                ..Default::default()
            },
        })
        .await
        .unwrap();

        // Allowed topic
        bus.publish(
            "allowed-topic",
            OrchestratorEvent {
                topic: "allowed-topic".into(),
                source: AgentId::new("pub-agent"),
                payload: serde_json::json!({}),
            },
        )
        .await
        .unwrap();

        // Disallowed topic
        let r = bus
            .publish(
                "forbidden-topic",
                OrchestratorEvent {
                    topic: "forbidden-topic".into(),
                    source: AgentId::new("pub-agent"),
                    payload: serde_json::json!({}),
                },
            )
            .await;
        assert!(matches!(
            r,
            Err(OrchestratorError::PublishUnauthorized(_, _))
        ));
    }

    #[tokio::test]
    async fn test_subscribe_capability_empty_means_all() {
        let bus = InMemoryOrchestrator::with_defaults();
        bus.register(AgentDescriptor {
            id: AgentId::new("open-agent"),
            name: "Open".into(),
            capabilities: vec![],
            orchestrator_config: AgentOrchestratorConfig {
                subscribe: vec![], // empty = no restriction
                ..Default::default()
            },
        })
        .await
        .unwrap();

        // Any topic allowed when subscribe list is empty
        bus.subscribe(
            "any-topic",
            Some(AgentId::new("open-agent")),
            orchestrator_handler(|_evt| Box::pin(async { Ok(()) })),
        )
        .await
        .unwrap();
    }
}
