use lattice_core::types::ToolCall;
use lattice_hooks::HookExecutionRecord;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{prompt, LoopEvent};

/// Session identity carried through a turn for manifest context fields.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub session_id: String,
    pub branch: String,
    pub parent_commit_id: String,
}

/// Record of a single hook's execution during a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDecisionRecord {
    pub hook_name: String,
    pub before_hash: String,
    pub after_hash: String,
    pub decision: String,
}

/// Record of a single tool call's execution during a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDecisionRecord {
    pub tool_name: String,
    pub arguments: String,
    pub result_hash: String,
}

/// Record of a prompt fragment for manifest persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentRecord {
    pub id: String,
    pub layer: String,
    pub role: String,
    pub authority: String,
    pub source: String,
    pub cache_scope: String,
    pub priority: u8,
    pub tokens: u32,
    pub content_hash: String,
    pub content_preview: String,
    pub rendered: bool,
    #[serde(default)]
    pub withheld_reason: Option<String>,
}

/// Snapshot of the compiled prompt at turn end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSnapshot {
    pub content_hash: String,
    pub token_count: u32,
    pub model: String,
    pub turn_index: u32,
    #[serde(default)]
    pub rendered_fragments: Vec<FragmentRecord>,
    #[serde(default)]
    pub withheld_fragments: Vec<FragmentRecord>,
}

/// Complete manifest for a single agent turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnManifestObject {
    pub hook_decisions: Vec<HookDecisionRecord>,
    pub tool_decisions: Vec<ToolDecisionRecord>,
    pub prompt_snapshot: PromptSnapshot,
    pub session_id: String,
    pub branch: String,
    pub parent_commit_id: String,
    pub request_id: String,
    /// Task/recovery metadata serialized from the agent's live task state.
    /// `None` when no task state was recorded during the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Per-turn collector for hook and tool decisions.
///
/// Constructed at the start of each turn in `TurnEngine::run()`,
/// populated during the turn, and consumed via `build_manifest()` at turn end.
/// NOT stored as an Agent field (D-02).
pub(super) struct ManifestCollector {
    pub(super) hook_decisions: Vec<HookDecisionRecord>,
    pub(super) tool_decisions: Vec<ToolDecisionRecord>,
    session_context: SessionContext,
    request_id: String,
    turn_index: u32,
    /// Optional task/recovery metadata set by the turn engine before manifest build.
    task_metadata: Option<serde_json::Value>,
}

impl ManifestCollector {
    pub(super) fn new(
        session_context: SessionContext,
        request_id: String,
        turn_index: u32,
    ) -> Self {
        Self {
            hook_decisions: Vec::new(),
            tool_decisions: Vec::new(),
            session_context,
            request_id,
            turn_index,
            task_metadata: None,
        }
    }

    /// Bridge hook execution records from PromptHookOutcome into the collector.
    pub(super) fn push_hook_records(&mut self, executions: &[HookExecutionRecord]) {
        for exec in executions {
            self.hook_decisions.push(HookDecisionRecord {
                hook_name: exec.hook_name.clone(),
                before_hash: exec.before_hash.clone(),
                after_hash: exec.after_hash.clone(),
                decision: serde_json::to_string(&exec.decision)
                    .unwrap_or_else(|_| format!("{:?}", exec.decision)),
            });
        }
    }

    /// Capture a tool decision at LoopEvent::ToolResult emission site.
    pub(super) fn push_tool_record(&mut self, call: &ToolCall, result: &str) {
        let arguments_hash = {
            let mut hasher = Sha256::new();
            hasher.update(call.function.arguments.as_bytes());
            hex::encode(hasher.finalize())
        };
        let result_hash = {
            let mut hasher = Sha256::new();
            hasher.update(result.as_bytes());
            hex::encode(hasher.finalize())
        };
        self.tool_decisions.push(ToolDecisionRecord {
            tool_name: call.function.name.clone(),
            arguments: arguments_hash,
            result_hash,
        });
    }

    /// Set task/recovery metadata for the manifest.
    /// Called by the turn engine after tool result enrichment,
    /// before the manifest is built.
    pub(super) fn set_task_metadata(&mut self, metadata: serde_json::Value) {
        self.task_metadata = Some(metadata);
    }

    /// Consume the collector and produce the TurnManifestObject.
    pub(super) fn build_manifest(
        self,
        rendered: &prompt::RenderedPrompt,
        model: &str,
    ) -> TurnManifestObject {
        let content_hash = {
            let mut hasher = Sha256::new();
            for c in &rendered.fragments {
                hasher.update(c.content.as_bytes());
            }
            hex::encode(hasher.finalize())
        };

        let rendered_fragments: Vec<FragmentRecord> =
            rendered.fragments.iter().map(fragment_to_record).collect();
        let withheld_fragments: Vec<FragmentRecord> = rendered
            .context_manifest
            .iter()
            .map(manifest_entry_to_record)
            .collect();

        TurnManifestObject {
            hook_decisions: self.hook_decisions,
            tool_decisions: self.tool_decisions,
            prompt_snapshot: PromptSnapshot {
                content_hash,
                token_count: rendered.total_tokens,
                model: model.to_string(),
                turn_index: self.turn_index,
                rendered_fragments,
                withheld_fragments,
            },
            session_id: self.session_context.session_id,
            branch: self.session_context.branch,
            parent_commit_id: self.session_context.parent_commit_id,
            request_id: self.request_id,
            metadata: self.task_metadata,
        }
    }
}

pub(super) fn emit_turn_manifest<F>(
    collector: ManifestCollector,
    rendered: &prompt::RenderedPrompt,
    model: &str,
    all_events: &mut Vec<LoopEvent>,
    emit: &mut F,
) where
    F: FnMut(LoopEvent),
{
    let manifest = collector.build_manifest(rendered, model);
    let manifest_event = LoopEvent::Manifest { manifest };
    emit(manifest_event.clone());
    all_events.push(manifest_event);
}

fn fragment_to_record(frag: &prompt::ContextFragment) -> FragmentRecord {
    let content_hash = {
        let mut hasher = Sha256::new();
        hasher.update(frag.content.as_bytes());
        hex::encode(hasher.finalize())
    };
    let content_preview = frag.content.chars().take(120).collect();
    FragmentRecord {
        id: frag.id.clone(),
        layer: format!("{:?}", frag.layer),
        role: format!("{:?}", frag.role),
        authority: format!("{:?}", frag.authority),
        source: format!("{:?}", frag.source),
        cache_scope: format!("{:?}", frag.cache_scope),
        priority: frag.priority,
        tokens: frag.tokens,
        content_hash,
        content_preview,
        rendered: true,
        withheld_reason: None,
    }
}

fn manifest_entry_to_record(entry: &prompt::ContextManifestEntry) -> FragmentRecord {
    let content_hash = String::new();
    let content_preview = entry.summary.chars().take(120).collect();
    FragmentRecord {
        id: entry.id.clone(),
        layer: format!("{:?}", entry.layer),
        role: format!("{:?}", entry.role),
        authority: format!("{:?}", entry.authority),
        source: format!("{:?}", entry.source),
        cache_scope: format!("{:?}", entry.cache_scope),
        priority: 0,
        tokens: entry.tokens,
        content_hash,
        content_preview,
        rendered: false,
        withheld_reason: Some("budget_exceeded".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{prompt, ManifestCollector, SessionContext};
    use lattice_core::types::{FunctionCall, Role, ToolCall};
    use lattice_hooks::{HookExecutionRecord, PromptHookDecision};

    fn session_context(session_id: &str, branch: &str, parent_commit_id: &str) -> SessionContext {
        SessionContext {
            session_id: session_id.into(),
            branch: branch.into(),
            parent_commit_id: parent_commit_id.into(),
        }
    }

    #[test]
    fn collector_push_hook_records() {
        let ctx = session_context("s1", "main", "abc123");
        let mut collector = ManifestCollector::new(ctx, "req-1".into(), 0);

        let records = vec![HookExecutionRecord {
            hook_name: "test-hook".into(),
            before_hash: "aaa".into(),
            after_hash: "bbb".into(),
            decision: PromptHookDecision::Continue,
        }];
        collector.push_hook_records(&records);

        assert_eq!(collector.hook_decisions.len(), 1);
        assert_eq!(collector.hook_decisions[0].hook_name, "test-hook");
        assert_eq!(collector.hook_decisions[0].before_hash, "aaa");
        assert_eq!(collector.hook_decisions[0].after_hash, "bbb");
    }

    #[test]
    fn collector_push_tool_record_hashes_result() {
        let ctx = session_context("s1", "main", "abc123");
        let mut collector = ManifestCollector::new(ctx, "req-1".into(), 0);

        let call = ToolCall {
            id: "tc1".into(),
            function: FunctionCall {
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        };
        collector.push_tool_record(&call, "file contents here");

        assert_eq!(collector.tool_decisions.len(), 1);
        assert_eq!(collector.tool_decisions[0].tool_name, "read_file");
        assert_eq!(collector.tool_decisions[0].arguments.len(), 64);
        assert!(!collector.tool_decisions[0].result_hash.is_empty());
        assert_eq!(collector.tool_decisions[0].result_hash.len(), 64);
    }

    #[test]
    fn collector_build_manifest_assembles_all_fields() {
        let ctx = session_context("sess-42", "feature/x", "commit-99");
        let mut collector = ManifestCollector::new(ctx, "req-7".into(), 3);

        collector.push_hook_records(&[HookExecutionRecord {
            hook_name: "h1".into(),
            before_hash: "aaa".into(),
            after_hash: "bbb".into(),
            decision: PromptHookDecision::Continue,
        }]);

        let call = ToolCall {
            id: "tc1".into(),
            function: FunctionCall {
                name: "tool_a".into(),
                arguments: "{\"x\":1}".into(),
            },
        };
        collector.push_tool_record(&call, "result data");

        let rendered = prompt::RenderedPrompt {
            messages: vec![],
            sections: vec![],
            contributions: vec![],
            fragments: vec![prompt::ContextFragment {
                id: "c1".into(),
                content: "content".into(),
                layer: prompt::Layer::System,
                role: Role::System,
                authority: prompt::PromptAuthority::System,
                source: prompt::ContextSource::Kernel,
                cache_scope: prompt::CacheScope::Static,
                markers: prompt::FragmentMarkers {
                    start: String::new(),
                    end: String::new(),
                },
                priority: 0,
                tokens: 10,
            }],
            context_manifest: vec![],
            total_tokens: 42,
        };

        let manifest = collector.build_manifest(&rendered, "gpt-4");

        assert_eq!(manifest.hook_decisions.len(), 1);
        assert_eq!(manifest.tool_decisions.len(), 1);
        assert_eq!(manifest.session_id, "sess-42");
        assert_eq!(manifest.branch, "feature/x");
        assert_eq!(manifest.parent_commit_id, "commit-99");
        assert_eq!(manifest.request_id, "req-7");
        assert_eq!(manifest.prompt_snapshot.model, "gpt-4");
        assert_eq!(manifest.prompt_snapshot.token_count, 42);
        assert_eq!(manifest.prompt_snapshot.turn_index, 3);
        assert!(!manifest.prompt_snapshot.content_hash.is_empty());
        assert_eq!(manifest.prompt_snapshot.content_hash.len(), 64);
    }

    #[test]
    fn hook_decision_record_debug_format() {
        let ctx = session_context("s1", "main", "abc");
        let mut collector = ManifestCollector::new(ctx, "r1".into(), 0);

        let records = vec![HookExecutionRecord {
            hook_name: "h1".into(),
            before_hash: "aaa".into(),
            after_hash: "bbb".into(),
            decision: PromptHookDecision::AddSection {
                section: prompt::PromptSection::new("x", prompt::Layer::Events, 0, 0),
                budget: prompt::TokenBudget::Fixed(100),
            },
        }];
        collector.push_hook_records(&records);

        assert!(collector.hook_decisions[0].decision.contains("AddSection"));
    }

    #[test]
    fn empty_collector_produces_empty_manifest() {
        let ctx = session_context("s1", "main", "abc");
        let collector = ManifestCollector::new(ctx, "r1".into(), 0);

        let rendered = prompt::RenderedPrompt {
            messages: vec![],
            sections: vec![],
            contributions: vec![],
            fragments: vec![],
            context_manifest: vec![],
            total_tokens: 0,
        };

        let manifest = collector.build_manifest(&rendered, "model");
        assert!(manifest.hook_decisions.is_empty());
        assert!(manifest.tool_decisions.is_empty());
        assert_eq!(manifest.prompt_snapshot.token_count, 0);
    }
}
