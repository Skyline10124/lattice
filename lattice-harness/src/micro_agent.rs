use std::sync::Arc;

use lattice_agent::Agent;
use lattice_bus::{AgentDescriptor, AgentId, AgentBusConfig, Bus, BusRequest, BusResponse, BusError};
use lattice_memory::Memory;
use tracing::{info, warn};

use crate::profile::AgentProfile;
use crate::runner::MEMORY_RT;

/// A Bus-aware micro-agent. Registers on the Bus, processes RPC requests
/// via lattice-core inference, and deregisters on exit.
pub struct MicroAgent {
    pub profile: AgentProfile,
    pub bus: Arc<dyn Bus>,
    pub memory: Option<Arc<dyn Memory>>,
}

/// Handle returned by spawn(). Owns the JoinHandle for crash detection (D5).
pub struct MicroAgentHandle {
    pub id: AgentId,
    join_handle: tokio::task::JoinHandle<()>,
}

impl MicroAgentHandle {
    /// Watch the agent task. On panic or normal exit, deregister from Bus.
    /// Call this after spawn() to enable crash recovery (D5).
    pub async fn watch_and_deregister(self, bus: Arc<dyn Bus>) {
        match self.join_handle.await {
            Ok(()) => {
                info!("MicroAgent '{}' exited normally, deregistering", self.id);
                bus.deregister(&self.id).await.ok();
            }
            Err(e) => {
                warn!("MicroAgent '{}' panicked: {}, deregistering", self.id, e);
                bus.deregister(&self.id).await.ok();
            }
        }
    }

    /// Abort the agent task (for shutdown).
    pub fn abort(&self) {
        self.join_handle.abort();
    }
}

impl MicroAgent {
    /// Create a MicroAgent from profile and bus.
    pub fn new(profile: AgentProfile, bus: Arc<dyn Bus>, memory: Option<Arc<dyn Memory>>) -> Self {
        Self { profile, bus, memory }
    }

    /// Register on Bus, resolve model, create Agent, spawn agent loop.
    /// Returns MicroAgentHandle for crash detection (D5).
    pub fn spawn(self) -> Result<MicroAgentHandle, BusError> {
        let bus_config = AgentBusConfig {
            subscribe: vec![], // Phase 2: subscribe from profile later
            publish: vec![],
            rpc: vec![],       // Phase 2: rpc whitelist from profile later
        };

        let descriptor = AgentDescriptor {
            id: AgentId::new(&self.profile.agent.name),
            name: self.profile.agent.name.clone(),
            capabilities: self.profile.agent.tags.clone(),
            bus_config,
        };

        let reg = MEMORY_RT.block_on(self.bus.register(descriptor))?;
        let request_rx = reg.request_rx;
        let id = reg.id;

        let resolved = lattice_core::resolve(&self.profile.agent.model)
            .map_err(|e| BusError::Serialize(e.to_string()))?;

        let mut agent = Agent::new(resolved);
        if let Some(ref mem) = self.memory {
            agent = agent.with_memory(mem.clone_box());
        }

        let max_turns = self.profile.handoff.max_turns.unwrap_or(10);
        let memory = self.memory.clone();
        let bus = self.bus.clone();
        let profile = self.profile;

        let join_handle = tokio::spawn(async move {
            micro_agent_loop(agent, profile, memory, bus, max_turns, request_rx).await;
        });

        Ok(MicroAgentHandle { id, join_handle })
    }
}

/// Core agent loop: receive BusRequest, run inference, send BusResponse.
async fn micro_agent_loop(
    mut agent: Agent,
    profile: AgentProfile,
    memory: Option<Arc<dyn Memory>>,
    bus: Arc<dyn Bus>,
    max_turns: u32,
    mut request_rx: tokio::sync::mpsc::Receiver<BusRequest>,
) {
    let agent_name = profile.agent.name.clone();
    info!("MicroAgent '{}' loop started", agent_name);

    while let Some(req) = request_rx.recv().await {
        let input = extract_input(&req.payload);
        let enriched = enrich_input(&input, &memory);

        let events = agent.run_async(&enriched, max_turns).await;

        let content = extract_content(&events);
        let output_json = parse_output(&content);

        let resp = BusResponse { payload: output_json };
        if req.reply_to.send(Ok(resp)).is_err() {
            warn!("MicroAgent '{}': reply channel closed, caller timed out", agent_name);
        }

        // Save to shared memory
        if let Some(ref mem) = memory {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let entry = lattice_memory::MemoryEntry {
                id: format!("{}-{}", agent_name, now_secs),
                kind: lattice_memory::EntryKind::SessionLog,
                session_id: agent_name.clone(),
                summary: format!("{}: {} chars output", agent_name, content.len()),
                content: content.clone(),
                tags: profile.agent.tags.clone(),
                created_at: now_secs.to_string(),
            };
            mem.save_entry(entry).await;
        }
    }

    info!("MicroAgent '{}' loop ended, deregistering", agent_name);
    bus.deregister(&AgentId::new(&agent_name)).await.ok();
}

/// Extract input string from BusRequest payload.
fn extract_input(payload: &serde_json::Value) -> String {
    match payload {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => {
            if let Some(input) = map.get("input") {
                input.to_string()
            } else {
                payload.to_string()
            }
        }
        _ => payload.to_string(),
    }
}

/// Enrich input with memory recall context (same logic as AgentRunner).
fn enrich_input(input: &str, memory: &Option<Arc<dyn Memory>>) -> String {
    if let Some(ref mem) = memory {
        // Use block_on since enrich_input is called in sync context before run_async
        let recall = MEMORY_RT.block_on(mem.recall(input, 5));
        if !recall.is_empty() {
            let context: String = recall
                .iter()
                .map(|e| {
                    format!(
                        "- {}: {} (session: {})",
                        match e.kind {
                            lattice_memory::EntryKind::Fact => "Fact",
                            lattice_memory::EntryKind::Decision => "Decision",
                            _ => "Log",
                        },
                        e.summary,
                        e.session_id
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            return format!(
                "[Relevant past context from memory:]\n{}\n\n[Current task:]\n{}",
                context, input
            );
        }
    }
    input.to_string()
}

/// Extract text content from LoopEvents.
fn extract_content(events: &[lattice_agent::LoopEvent]) -> String {
    let mut content = String::new();
    for event in events {
        if let lattice_agent::LoopEvent::Token { text } = event {
            content.push_str(text);
        }
    }
    content
}

/// Parse agent output as JSON. Strip markdown code fences if present.
fn parse_output(content: &str) -> serde_json::Value {
    let trimmed = content.trim();
    let json_str = if trimmed.starts_with("```") {
        trimmed
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end_matches("```")
            .trim()
            .to_string()
    } else {
        trimmed.to_string()
    };

    serde_json::from_str(&json_str)
        .unwrap_or_else(|_| serde_json::json!({"content": content}))
}