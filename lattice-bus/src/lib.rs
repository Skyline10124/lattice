//! LATTICE Micro-Agent Bus — async RPC + pub/sub hybrid communication layer.
//!
//! Bus does routing only. Agent execution (agent loop, LLM inference) is owned
//! by the caller (harness). register() returns a Registration with the request
//! channel receiver — the caller spawns their own tokio task to process requests.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, RwLock};

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
// BusError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum BusError {
    #[error("agent not found: {0}")]
    AgentNotFound(AgentId),
    #[error("agent crashed: {0}")]
    AgentCrashed(AgentId),
    #[error("RPC timeout after {0:?}")]
    Timeout(Duration),
    #[error("channel closed")]
    ChannelClosed,
    #[error("serialization error: {0}")]
    Serialize(String),
    #[error("unauthorized: agent {0} not in caller's rpc whitelist")]
    Unauthorized(AgentId),
}

// ---------------------------------------------------------------------------
// BusRequest / BusResponse / BusEvent
// ---------------------------------------------------------------------------

/// RPC request — payload + oneshot reply channel (D9).
pub struct BusRequest {
    pub payload: serde_json::Value,
    pub reply_to: oneshot::Sender<Result<BusResponse, BusError>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusResponse {
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusEvent {
    pub topic: String,
    pub source: AgentId,
    pub payload: serde_json::Value,
}

// ---------------------------------------------------------------------------
// AgentDescriptor / AgentBusConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub id: AgentId,
    pub name: String,
    pub capabilities: Vec<String>,
    pub bus_config: AgentBusConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentBusConfig {
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
    pub request_rx: mpsc::Receiver<BusRequest>,
}

// ---------------------------------------------------------------------------
// BusConfig / DeliveryPolicy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BusConfig {
    pub timeout_rpc: Duration,
    pub delivery_policy: DeliveryPolicy,
    pub subscriber_buffer: usize,
    pub max_concurrent_calls: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryPolicy {
    AtMostOnce,
    AtLeastOnce,
}

impl Default for BusConfig {
    fn default() -> Self {
        Self {
            timeout_rpc: Duration::from_secs(30),
            delivery_policy: DeliveryPolicy::AtMostOnce,
            subscriber_buffer: 1024,
            max_concurrent_calls: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Bus trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait Bus: Send + Sync {
    /// Register an agent. Returns Registration with request channel receiver.
    /// The caller owns request_rx and spawns their own agent loop task.
    async fn register(&self, agent: AgentDescriptor) -> Result<Registration, BusError>;
    async fn discover(&self, capability: &str) -> Vec<AgentDescriptor>;
    async fn deregister(&self, id: &AgentId) -> Result<(), BusError>;

    async fn subscribe(&self, topic: &str, handler: BusHandlerFn) -> Result<(), BusError>;
    async fn publish(&self, topic: &str, event: BusEvent) -> Result<(), BusError>;

    async fn call(
        &self,
        caller: &AgentId,
        target: &AgentId,
        request: serde_json::Value,
    ) -> Result<BusResponse, BusError>;
    async fn call_with_timeout(
        &self,
        caller: &AgentId,
        target: &AgentId,
        request: serde_json::Value,
        timeout: Duration,
    ) -> Result<BusResponse, BusError>;
}

// ---------------------------------------------------------------------------
// BusHandlerFn
// ---------------------------------------------------------------------------

pub type BusHandlerFn = Arc<
    dyn Fn(BusEvent) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BusError>> + Send>>
        + Send
        + Sync,
>;

/// Convenience wrapper for constructing BusHandlerFn from an async fn.
pub struct BusHandler;

impl BusHandler {
    /// Create a BusHandlerFn from an async function.
    /// Usage: `BusHandler::from_async(|event| async move { ... })`
    pub fn from_async(
        f: impl Fn(BusEvent) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BusError>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> BusHandlerFn {
        Arc::new(f)
    }
}

pub fn bus_handler(
    f: impl Fn(BusEvent)
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BusError>> + Send>>
        + Send
        + Sync
        + 'static,
) -> BusHandlerFn {
    Arc::new(f)
}

/// Macro for constructing BusHandlerFn with async block syntax.
/// Usage: `bus_handler!(|event| { /* async body */ })`
#[macro_export]
macro_rules! bus_handler {
    ($handler:expr) => {
        $crate::BusHandler::from_async($handler)
    };
}

// ---------------------------------------------------------------------------
// AgentLoop — default echo handler for testing
// ---------------------------------------------------------------------------

/// Simple agent loop that echoes request payloads back. Useful for testing.
/// Real agents (harness) will replace this with LLM-backed processing.
pub async fn echo_agent_loop(mut rx: mpsc::Receiver<BusRequest>) {
    while let Some(req) = rx.recv().await {
        let resp = BusResponse { payload: req.payload.clone() };
        let _ = req.reply_to.send(Ok(resp));
    }
}

// ---------------------------------------------------------------------------
// InMemoryBus
// ---------------------------------------------------------------------------

struct AgentEntry {
    descriptor: AgentDescriptor,
    request_tx: mpsc::Sender<BusRequest>,
}

pub struct InMemoryBus {
    config: BusConfig,
    agents: RwLock<HashMap<AgentId, AgentEntry>>,
    subscriptions: RwLock<HashMap<String, Vec<BusHandlerFn>>>,
}

impl InMemoryBus {
    pub fn new(config: BusConfig) -> Self {
        Self {
            config,
            agents: RwLock::new(HashMap::new()),
            subscriptions: RwLock::new(HashMap::new()),
        }
    }
    pub fn with_defaults() -> Self {
        Self::new(BusConfig::default())
    }
}

#[async_trait]
impl Bus for InMemoryBus {
    async fn register(&self, agent: AgentDescriptor) -> Result<Registration, BusError> {
        let id = agent.id.clone();
        let (request_tx, request_rx) = mpsc::channel(self.config.max_concurrent_calls);
        let entry = AgentEntry { descriptor: agent, request_tx };
        self.agents.write().await.insert(id.clone(), entry);
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

    async fn deregister(&self, id: &AgentId) -> Result<(), BusError> {
        if self.agents.write().await.remove(id).is_some() {
            Ok(())
        } else {
            Err(BusError::AgentNotFound(id.clone()))
        }
    }

    async fn subscribe(&self, topic: &str, handler: BusHandlerFn) -> Result<(), BusError> {
        self.subscriptions
            .write()
            .await
            .entry(topic.to_string())
            .or_default()
            .push(handler);
        Ok(())
    }

    async fn publish(&self, topic: &str, event: BusEvent) -> Result<(), BusError> {
        if let Some(handlers) = self.subscriptions.read().await.get(topic) {
            for handler in handlers {
                let h = handler.clone();
                let evt = event.clone();
                tokio::spawn(async move { h(evt).await.ok(); });
            }
        }
        Ok(())
    }

    async fn call(
        &self,
        caller: &AgentId,
        target: &AgentId,
        request: serde_json::Value,
    ) -> Result<BusResponse, BusError> {
        self.call_with_timeout(caller, target, request, self.config.timeout_rpc).await
    }

    async fn call_with_timeout(
        &self,
        caller: &AgentId,
        target: &AgentId,
        request: serde_json::Value,
        timeout: Duration,
    ) -> Result<BusResponse, BusError> {
        let agents = self.agents.read().await;

        let caller_e = agents.get(caller).ok_or(BusError::AgentNotFound(caller.clone()))?;
        if !caller_e.descriptor.bus_config.rpc.contains(target) {
            return Err(BusError::Unauthorized(target.clone()));
        }

        let target_e = agents.get(target).ok_or(BusError::AgentNotFound(target.clone()))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        target_e
            .request_tx
            .send(BusRequest { payload: request, reply_to: reply_tx })
            .await
            .map_err(|_| BusError::ChannelClosed)?;

        drop(agents);

        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(_)) => Err(BusError::ChannelClosed),
            Err(_) => Err(BusError::Timeout(timeout)),
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
    async fn register_echo(bus: &InMemoryBus, desc: AgentDescriptor) -> AgentId {
        let reg = bus.register(desc).await.unwrap();
        tokio::spawn(echo_agent_loop(reg.request_rx));
        reg.id
    }

    #[tokio::test]
    async fn test_register_and_discover() {
        let bus = InMemoryBus::with_defaults();
        let id = register_echo(&bus, AgentDescriptor {
            id: AgentId::new("reviewer"),
            name: "Code Reviewer".into(),
            capabilities: vec!["code-review".into()],
            bus_config: AgentBusConfig::default(),
        }).await;
        assert_eq!(id, AgentId::new("reviewer"));

        let found = bus.discover("code-review").await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, AgentId::new("reviewer"));
    }

    #[tokio::test]
    async fn test_discover_no_match() {
        let bus = InMemoryBus::with_defaults();
        assert!(bus.discover("nonexistent").await.is_empty());
    }

    #[tokio::test]
    async fn test_deregister() {
        let bus = InMemoryBus::with_defaults();
        let id = register_echo(&bus, AgentDescriptor {
            id: AgentId::new("temp"),
            name: "Temp".into(),
            capabilities: vec!["temp".into()],
            bus_config: AgentBusConfig::default(),
        }).await;
        bus.deregister(&id).await.unwrap();
        assert!(bus.discover("temp").await.is_empty());
    }

    #[tokio::test]
    async fn test_deregister_nonexistent() {
        let bus = InMemoryBus::with_defaults();
        let r = bus.deregister(&AgentId::new("ghost")).await;
        assert!(matches!(r, Err(BusError::AgentNotFound(_))));
    }

    #[tokio::test]
    async fn test_rpc_call_success() {
        let bus = InMemoryBus::with_defaults();

        register_echo(&bus, AgentDescriptor {
            id: AgentId::new("reviewer"),
            name: "Reviewer".into(),
            capabilities: vec!["code-review".into()],
            bus_config: AgentBusConfig {
                rpc: vec![AgentId::new("refactorer")],
                ..Default::default()
            },
        }).await;

        register_echo(&bus, AgentDescriptor {
            id: AgentId::new("refactorer"),
            name: "Refactorer".into(),
            capabilities: vec!["refactor".into()],
            bus_config: AgentBusConfig::default(),
        }).await;

        let resp = bus.call(
            &AgentId::new("reviewer"),
            &AgentId::new("refactorer"),
            serde_json::json!({"code": "fn main() {}"}),
        ).await.unwrap();

        assert_eq!(resp.payload["code"], "fn main() {}");
    }

    #[tokio::test]
    async fn test_rpc_unauthorized() {
        let bus = InMemoryBus::with_defaults();

        register_echo(&bus, AgentDescriptor {
            id: AgentId::new("reviewer"),
            name: "Reviewer".into(),
            capabilities: vec!["code-review".into()],
            bus_config: AgentBusConfig::default(),
        }).await;

        register_echo(&bus, AgentDescriptor {
            id: AgentId::new("refactorer"),
            name: "Refactorer".into(),
            capabilities: vec!["refactor".into()],
            bus_config: AgentBusConfig::default(),
        }).await;

        let r = bus.call(&AgentId::new("reviewer"), &AgentId::new("refactorer"), serde_json::json!({})).await;
        assert!(matches!(r, Err(BusError::Unauthorized(_))));
    }

    #[tokio::test]
    async fn test_rpc_target_not_found() {
        let bus = InMemoryBus::with_defaults();
        register_echo(&bus, AgentDescriptor {
            id: AgentId::new("reviewer"),
            name: "Reviewer".into(),
            capabilities: vec!["code-review".into()],
            bus_config: AgentBusConfig {
                rpc: vec![AgentId::new("ghost")],
                ..Default::default()
            },
        }).await;

        let r = bus.call(&AgentId::new("reviewer"), &AgentId::new("ghost"), serde_json::json!({})).await;
        assert!(matches!(r, Err(BusError::AgentNotFound(_))));
    }

    #[tokio::test]
    async fn test_rpc_caller_not_found() {
        let bus = InMemoryBus::with_defaults();
        let r = bus.call(&AgentId::new("ghost"), &AgentId::new("refactorer"), serde_json::json!({})).await;
        assert!(matches!(r, Err(BusError::AgentNotFound(_))));
    }

    #[tokio::test]
    async fn test_rpc_timeout() {
        let bus = InMemoryBus::new(BusConfig {
            timeout_rpc: Duration::from_millis(50),
            ..Default::default()
        });

        register_echo(&bus, AgentDescriptor {
            id: AgentId::new("caller"),
            name: "Caller".into(),
            capabilities: vec!["call".into()],
            bus_config: AgentBusConfig {
                rpc: vec![AgentId::new("slow")],
                ..Default::default()
            },
        }).await;

        // Register slow agent that never responds.
        let reg = bus.register(AgentDescriptor {
            id: AgentId::new("slow"),
            name: "Slow".into(),
            capabilities: vec!["slow".into()],
            bus_config: AgentBusConfig::default(),
        }).await.unwrap();
        // Don't spawn echo loop — request_rx drops, channel closes.
        drop(reg.request_rx);

        let r = bus.call_with_timeout(
            &AgentId::new("caller"),
            &AgentId::new("slow"),
            serde_json::json!({}),
            Duration::from_millis(50),
        ).await;
        assert!(matches!(r, Err(BusError::ChannelClosed | BusError::Timeout(_))));
    }

    #[tokio::test]
    async fn test_pub_sub_basic() {
        let bus = InMemoryBus::with_defaults();
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let rc = received.clone();

        bus.subscribe("code-changes", bus_handler(move |event: BusEvent| {
            let r = rc.clone();
            Box::pin(async move { r.lock().await.push(event.payload); Ok(()) })
        })).await.unwrap();

        bus.publish("code-changes", BusEvent {
            topic: "code-changes".into(),
            source: AgentId::new("watcher"),
            payload: serde_json::json!({"file": "main.rs"}),
        }).await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        let items = received.lock().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["file"], "main.rs");
    }

    #[tokio::test]
    async fn test_pub_sub_no_subscribers() {
        let bus = InMemoryBus::with_defaults();
        let r = bus.publish("orphan-topic", BusEvent {
            topic: "orphan-topic".into(),
            source: AgentId::new("sender"),
            payload: serde_json::json!({}),
        }).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn test_bus_config_defaults() {
        let c = BusConfig::default();
        assert_eq!(c.timeout_rpc, Duration::from_secs(30));
        assert_eq!(c.delivery_policy, DeliveryPolicy::AtMostOnce);
        assert_eq!(c.subscriber_buffer, 1024);
        assert_eq!(c.max_concurrent_calls, 1);
    }

    #[tokio::test]
    async fn test_registration_yields_request_rx() {
        let bus = InMemoryBus::with_defaults();
        let reg = bus.register(AgentDescriptor {
            id: AgentId::new("agent-a"),
            name: "A".into(),
            capabilities: vec!["test".into()],
            bus_config: AgentBusConfig::default(),
        }).await.unwrap();

        assert_eq!(reg.id, AgentId::new("agent-a"));
        // request_rx is usable — caller can spawn their own agent loop.
        tokio::spawn(echo_agent_loop(reg.request_rx));
    }

    #[tokio::test]
    async fn test_bus_handler_from_async() {
        let bus = InMemoryBus::with_defaults();
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let rc = received.clone();

        let handler = BusHandler::from_async(move |event: BusEvent| {
            let r = rc.clone();
            Box::pin(async move { r.lock().await.push(event.topic); Ok(()) })
        });

        bus.subscribe("test-topic", handler).await.unwrap();
        bus.publish("test-topic", BusEvent {
            topic: "test-topic".into(),
            source: AgentId::new("sender"),
            payload: serde_json::json!({}),
        }).await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        let items = received.lock().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], "test-topic");
    }

    #[tokio::test]
    async fn test_bus_handler_macro() {
        let bus = InMemoryBus::with_defaults();
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let rc = received.clone();

        let handler = bus_handler!(move |event: BusEvent| {
            let r = rc.clone();
            Box::pin(async move { r.lock().await.push(event.source.to_string()); Ok(()) })
        });

        bus.subscribe("macro-topic", handler).await.unwrap();
        bus.publish("macro-topic", BusEvent {
            topic: "macro-topic".into(),
            source: AgentId::new("macro-sender"),
            payload: serde_json::json!({}),
        }).await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        let items = received.lock().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], "macro-sender");
    }
}