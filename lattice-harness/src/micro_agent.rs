use std::sync::Arc;
use std::sync::LazyLock;

use crate::memory::SharedMemory;
use lattice_agent::memory::{Memory, MemoryEntry, PartitionAccess, SharedPartition};
use lattice_agent::Agent;
use lattice_bus::{
    AgentBusConfig, AgentDescriptor, AgentId, Bus, BusError, BusEvent, BusHandler, BusRequest,
    BusResponse,
};
use tracing::{info, warn};

use crate::profile::{AgentProfile, BusConfigProfile, MemoryConfigProfile};

static BUS_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("micro_agent bus runtime")
});

/// Convert profile [bus] section into AgentBusConfig.
fn bus_config_from_profile(bus: &BusConfigProfile) -> AgentBusConfig {
    AgentBusConfig {
        subscribe: bus.subscribe.clone(),
        publish: bus.publish.clone(),
        rpc: bus.rpc.iter().map(AgentId::new).collect(),
    }
}

/// Convert profile [memory] section into PartitionAccess for SharedMemory calls.
fn partition_access_from_profile(memory: &MemoryConfigProfile) -> PartitionAccess {
    let read: Vec<SharedPartition> = memory
        .shared_read
        .iter()
        .map(|s| SharedPartition::Named(s.clone()))
        .collect();
    let write: Vec<SharedPartition> = memory
        .shared_write
        .iter()
        .map(|s| SharedPartition::Named(s.clone()))
        .collect();
    PartitionAccess::new(read, write)
}
/// A Bus-aware micro-agent. Registers on the Bus, processes RPC requests
/// via lattice-core inference, and deregisters on exit.
pub struct MicroAgent {
    pub profile: AgentProfile,
    pub bus: Arc<dyn Bus>,
    pub memory: Option<Arc<dyn Memory>>,
    pub shared_memory: Option<Arc<dyn SharedMemory>>,
    pub partition_access: PartitionAccess,
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
    pub fn new(
        profile: AgentProfile,
        bus: Arc<dyn Bus>,
        memory: Option<Arc<dyn Memory>>,
        shared_memory: Option<Arc<dyn SharedMemory>>,
    ) -> Self {
        let partition_access = partition_access_from_profile(&profile.memory);
        Self {
            profile,
            bus,
            memory,
            shared_memory,
            partition_access,
        }
    }

    /// Register on Bus, resolve model, create Agent, spawn agent loop.
    /// Returns MicroAgentHandle for crash detection (D5).
    #[allow(deprecated)]
    pub fn spawn(self) -> Result<MicroAgentHandle, BusError> {
        let bus_config = bus_config_from_profile(&self.profile.bus);

        let descriptor = AgentDescriptor {
            id: AgentId::new(&self.profile.agent.name),
            name: self.profile.agent.name.clone(),
            capabilities: self.profile.agent.tags.clone(),
            bus_config,
        };

        let reg = BUS_RT.block_on(self.bus.register(descriptor))?;
        let request_rx = reg.request_rx;
        let id = reg.id;

        let resolved = lattice_core::resolve(&self.profile.agent.model)
            .map_err(|e| BusError::Resolve(e.to_string()))?;

        let mut agent = Agent::new(resolved);
        if let Some(ref mem) = self.memory {
            agent = agent.with_memory(mem.clone_box());
        }

        let max_turns = self.profile.handoff.max_turns.unwrap_or(10);
        let ctx = AgentLoopContext {
            memory: self.memory.clone(),
            shared_memory: self.shared_memory.clone(),
            partition_access: self.partition_access.clone(),
            bus: self.bus.clone(),
            profile: self.profile,
        };

        // Subscribe to topics from profile [bus] section.
        // Events are stored in private memory for recall during next RPC request.
        let memory_for_sub = self.memory.clone();
        let sub_agent_name = ctx.profile.agent.name.clone();
        for topic in &ctx.profile.bus.subscribe {
            let mem = memory_for_sub.clone();
            let name = sub_agent_name.clone();
            let handler = BusHandler::from_async(move |event: BusEvent| {
                let mem = mem.clone();
                let name = name.clone();
                Box::pin(async move {
                    if let Some(ref m) = mem {
                        let ms = lattice_agent::memory::now_ms();
                        m.save_entry(MemoryEntry {
                            id: format!("{}-event-{}", name, ms),
                            kind: lattice_agent::memory::EntryKind::SessionLog,
                            session_id: name.clone(),
                            summary: format!("Event on {}", event.topic),
                            content: event.payload.to_string(),
                            tags: vec![event.topic.clone()],
                            created_at: ms.to_string(),
                        });
                    }
                    Ok(())
                })
            });
            // Use MEMORY_RT since spawn() is called in sync context
            BUS_RT.block_on(self.bus.subscribe(topic, handler))?;
        }

        let join_handle = tokio::spawn(async move {
            micro_agent_loop(agent, ctx, max_turns, request_rx).await;
        });

        Ok(MicroAgentHandle { id, join_handle })
    }
}

/// Context passed into the agent loop — bundles shared resources.
struct AgentLoopContext {
    memory: Option<Arc<dyn Memory>>,
    shared_memory: Option<Arc<dyn SharedMemory>>,
    partition_access: PartitionAccess,
    bus: Arc<dyn Bus>,
    profile: AgentProfile,
}

/// Core agent loop: receive BusRequest, run inference, send BusResponse.
async fn micro_agent_loop(
    mut agent: Agent,
    ctx: AgentLoopContext,
    max_turns: u32,
    mut request_rx: tokio::sync::mpsc::Receiver<BusRequest>,
) {
    let agent_name = ctx.profile.agent.name.clone();
    info!("MicroAgent '{}' loop started", agent_name);

    while let Some(req) = request_rx.recv().await {
        let input = extract_input(&req.payload);
        let enriched = enrich_input(&input, &ctx.memory);

        let events = agent.run_async(&enriched, max_turns).await;

        let content = extract_content(&events);
        let output_json = parse_output(&content);

        let resp = BusResponse {
            payload: output_json,
        };
        if req.reply_to.send(Ok(resp)).is_err() {
            warn!(
                "MicroAgent '{}': reply channel closed, caller timed out",
                agent_name
            );
        }

        // Save to private memory
        if let Some(ref mem) = ctx.memory {
            let ms = lattice_agent::memory::now_ms();
            let entry = MemoryEntry {
                id: format!("{}-{}", agent_name, ms),
                kind: lattice_agent::memory::EntryKind::SessionLog,
                session_id: agent_name.clone(),
                summary: format!("{}: {} chars output", agent_name, content.len()),
                content: content.clone(),
                tags: ctx.profile.agent.tags.clone(),
                created_at: ms.to_string(),
            };
            mem.save_entry(entry);
        }

        // Save to shared memory partitions (write to each configured partition)
        if let Some(ref smem) = ctx.shared_memory {
            for write_partition in &ctx.partition_access.write {
                let ms = lattice_agent::memory::now_ms();
                let entry = MemoryEntry {
                    id: format!("{}-shared-{}", agent_name, ms),
                    kind: lattice_agent::memory::EntryKind::SessionLog,
                    session_id: agent_name.clone(),
                    summary: format!("{}: shared output", agent_name),
                    content: content.clone(),
                    tags: ctx.profile.agent.tags.clone(),
                    created_at: ms.to_string(),
                };
                if let Err(e) = smem
                    .save_shared(entry, write_partition.clone(), &ctx.partition_access)
                    .await
                {
                    warn!(
                        "MicroAgent '{}': shared memory write to {:?} failed: {:?}",
                        agent_name, write_partition, e
                    );
                }
            }
        }
        // Publish output to configured topics
        for topic in &ctx.profile.bus.publish {
            let event = BusEvent {
                topic: topic.clone(),
                source: AgentId::new(&agent_name),
                payload: serde_json::json!({"output": content.clone()}),
            };
            if let Err(e) = ctx.bus.publish(topic, event).await {
                warn!(
                    "MicroAgent '{}': publish to '{}' failed: {:?}",
                    agent_name, topic, e
                );
            }
        }
    }

    info!("MicroAgent '{}' loop ended", agent_name);
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

/// Enrich input with memory recall context.
fn enrich_input(input: &str, memory: &Option<Arc<dyn Memory>>) -> String {
    if let Some(ref mem) = memory {
        let recall = mem.recall(input, 5);
        if !recall.is_empty() {
            let context: String = recall
                .iter()
                .map(|e| {
                    format!(
                        "- {}: {} (session: {})",
                        match e.kind {
                            lattice_agent::memory::EntryKind::Fact => "Fact",
                            lattice_agent::memory::EntryKind::Decision => "Decision",
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

    serde_json::from_str(&json_str).unwrap_or_else(|_| serde_json::json!({"content": content}))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_agent::LoopEvent;

    // --- extract_input tests ---

    #[test]
    fn test_extract_input_string_payload() {
        let payload = serde_json::json!("hello world");
        assert_eq!(extract_input(&payload), "hello world");
    }

    #[test]
    fn test_extract_input_object_with_input_key() {
        let payload = serde_json::json!({"input": "do something", "extra": 42});
        assert_eq!(extract_input(&payload), "\"do something\"");
    }

    #[test]
    fn test_extract_input_object_without_input_key() {
        let payload = serde_json::json!({"task": "review", "priority": "high"});
        // Falls back to payload.to_string() which is the full JSON representation
        assert_eq!(extract_input(&payload), payload.to_string());
    }

    #[test]
    fn test_extract_input_null_payload() {
        let payload = serde_json::json!(null);
        assert_eq!(extract_input(&payload), "null");
    }

    #[test]
    fn test_extract_input_number_payload() {
        let payload = serde_json::json!(42);
        assert_eq!(extract_input(&payload), "42");
    }

    // --- parse_output tests ---

    #[test]
    fn test_parse_output_valid_json() {
        let content = "{\"result\": \"ok\", \"score\": 0.9}";
        let parsed = parse_output(content);
        assert_eq!(parsed["result"], "ok");
        assert_eq!(parsed["score"], 0.9);
    }

    #[test]
    fn test_parse_output_markdown_fenced_json() {
        let content = "```json\n{\"result\": \"ok\"}\n```";
        let parsed = parse_output(content);
        assert_eq!(parsed["result"], "ok");
    }

    #[test]
    fn test_parse_output_non_json_fallback() {
        let content = "This is just plain text, not JSON.";
        let parsed = parse_output(content);
        // Non-JSON falls back to {"content": original_string}
        assert_eq!(parsed["content"], content);
    }

    #[test]
    fn test_parse_output_empty_string() {
        let content = "";
        let parsed = parse_output(content);
        // Empty string is not valid JSON → falls back to {"content": ""}
        assert_eq!(parsed["content"], "");
    }

    // --- extract_content tests ---

    #[test]
    fn test_extract_content_empty_events() {
        let events: Vec<LoopEvent> = vec![];
        assert_eq!(extract_content(&events), "");
    }

    #[test]
    fn test_extract_content_token_events_mixed() {
        let events = vec![
            LoopEvent::Token {
                text: "Hello".into(),
            },
            LoopEvent::Reasoning {
                text: "thinking...".into(),
            },
            LoopEvent::Token {
                text: " world".into(),
            },
            LoopEvent::Done { usage: None },
        ];
        assert_eq!(extract_content(&events), "Hello world");
    }

    #[test]
    fn test_extract_content_no_tokens() {
        let events = vec![
            LoopEvent::Reasoning {
                text: "deep thought".into(),
            },
            LoopEvent::Done { usage: None },
            LoopEvent::Error {
                message: "oops".into(),
            },
        ];
        assert_eq!(extract_content(&events), "");
    }
}
