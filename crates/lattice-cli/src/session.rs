use std::path::PathBuf;

pub use lattice_runtime::messages_for_session as messages_for_agent;
pub use lattice_session::{
    finalize_session_turn, NewTurnManifest, Session, SessionStore, SqliteCasSessionStore,
};

#[cfg(test)]
pub use lattice_session::SessionMessage;

pub fn default_session_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lattice")
        .join("sessions")
}

pub fn session_manager() -> SqliteCasSessionStore {
    SqliteCasSessionStore::new(default_session_dir())
}

/// Convert an agent-side TurnManifestObject into a session-side NewTurnManifest
/// for persistence. `session_id` is the final session ID (from `finalize_session_turn`),
/// which may differ from the agent-side manifest's session_id.
#[allow(dead_code)] // runtime-owned persistence replaced frontend manifest collection.
pub fn agent_manifest_to_new(
    manifest: &lattice_agent::TurnManifestObject,
    session_id: &str,
    model: &str,
    provider: &str,
    turn_index: usize,
) -> NewTurnManifest {
    NewTurnManifest {
        session_id: session_id.to_string(),
        commit_id: None,
        turn_index,
        request_id: manifest.request_id.clone(),
        parent_manifest_ids: Vec::new(),
        model: model.to_string(),
        provider: provider.to_string(),
        prompt: lattice_session::PromptSnapshot {
            total_tokens: manifest.prompt_snapshot.token_count,
            effective_context_budget: 0,
            rendered_fragments: manifest
                .prompt_snapshot
                .rendered_fragments
                .iter()
                .map(|f| lattice_session::PromptFragmentRecord {
                    id: f.id.clone(),
                    layer: f.layer.clone(),
                    role: f.role.clone(),
                    authority: f.authority.clone(),
                    source: f.source.clone(),
                    cache_scope: f.cache_scope.clone(),
                    priority: f.priority,
                    tokens: f.tokens,
                    content_hash: f.content_hash.clone(),
                    content_preview: f.content_preview.clone(),
                    rendered: f.rendered,
                    withheld_reason: f.withheld_reason.clone(),
                })
                .collect(),
            withheld_fragments: manifest
                .prompt_snapshot
                .withheld_fragments
                .iter()
                .map(|f| lattice_session::PromptFragmentRecord {
                    id: f.id.clone(),
                    layer: f.layer.clone(),
                    role: f.role.clone(),
                    authority: f.authority.clone(),
                    source: f.source.clone(),
                    cache_scope: f.cache_scope.clone(),
                    priority: f.priority,
                    tokens: f.tokens,
                    content_hash: f.content_hash.clone(),
                    content_preview: f.content_preview.clone(),
                    rendered: f.rendered,
                    withheld_reason: f.withheld_reason.clone(),
                })
                .collect(),
        },
        hooks: manifest
            .hook_decisions
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let (decision_kind, target_id, reason) = parse_hook_decision(&h.decision);
                lattice_session::HookDecisionRecord {
                    point: String::new(),
                    hook_name: h.hook_name.clone(),
                    order: i,
                    decision_kind,
                    target_fragment_id: target_id,
                    reason,
                    before_hash: Some(h.before_hash.clone()),
                    after_hash: Some(h.after_hash.clone()),
                }
            })
            .collect(),
        tools: manifest
            .tool_decisions
            .iter()
            .map(|t| lattice_session::ToolDecisionRecord {
                tool_name: t.tool_name.clone(),
                decision_kind: "executed".to_string(),
                command_hash: Some(t.arguments.clone()),
                command_preview: None,
                result_hash: Some(t.result_hash.clone()),
                result_preview: None,
            })
            .collect(),
        metadata: manifest.metadata.clone().unwrap_or(serde_json::Value::Null),
    }
}

/// Parse a serialized hook decision string into structured fields.
/// The decision string is typically a JSON-encoded `PromptHookDecision` or a debug repr.
#[allow(dead_code)] // retained for manifest compatibility tests and legacy readers.
fn parse_hook_decision(decision: &str) -> (String, Option<String>, Option<String>) {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(decision) {
        match val {
            serde_json::Value::String(kind) => (kind, None, None),
            serde_json::Value::Object(map) => {
                if let Some((kind, payload)) = map.into_iter().next() {
                    let target_id = payload
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or_else(|| {
                            payload
                                .get("section")
                                .and_then(|section| section.get("id"))
                                .and_then(|v| v.as_str())
                                .map(String::from)
                        });
                    let reason = payload
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or_else(|| payload.as_str().map(String::from));
                    (kind, target_id, reason)
                } else {
                    ("unknown".to_string(), None, None)
                }
            }
            _ => ("unknown".to_string(), None, None),
        }
    } else {
        // Debug repr fallback: extract the variant name.
        let kind = decision
            .split('{')
            .next()
            .and_then(|s| s.split('(').next())
            .unwrap_or(decision)
            .trim()
            .to_string();
        (kind, None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_manifest_to_new_uses_final_session_id() {
        let agent_manifest = lattice_agent::TurnManifestObject {
            hook_decisions: Vec::new(),
            tool_decisions: Vec::new(),
            prompt_snapshot: lattice_agent::PromptSnapshot {
                content_hash: "abc".to_string(),
                token_count: 42,
                model: "test-model".to_string(),
                turn_index: 0,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            session_id: "agent-temp-id".to_string(),
            branch: "main".to_string(),
            parent_commit_id: String::new(),
            request_id: "req-1".to_string(),
            metadata: None,
        };

        let new_manifest = agent_manifest_to_new(
            &agent_manifest,
            "final-session-id",
            "test-model",
            "test-provider",
            0,
        );

        // Must use the final session ID, not the agent-side one
        assert_eq!(new_manifest.session_id, "final-session-id");
        assert_eq!(new_manifest.request_id, "req-1");
        assert_eq!(new_manifest.model, "test-model");
        assert_eq!(new_manifest.prompt.total_tokens, 42);
    }

    #[test]
    fn agent_manifest_to_new_preserves_metadata() {
        let task_json = serde_json::json!({
            "plan": "Fix auth bug",
            "todos_count": 3,
            "active_todo": "Add tests",
            "changed_files": ["src/auth.rs"],
            "tool_failures": [],
            "verification_failures": [],
            "unverified_work": ["cargo test auth"]
        });

        let agent_manifest = lattice_agent::TurnManifestObject {
            hook_decisions: Vec::new(),
            tool_decisions: Vec::new(),
            prompt_snapshot: lattice_agent::PromptSnapshot {
                content_hash: "abc".to_string(),
                token_count: 42,
                model: "test-model".to_string(),
                turn_index: 0,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            session_id: "agent-temp-id".to_string(),
            branch: "main".to_string(),
            parent_commit_id: String::new(),
            request_id: "req-1".to_string(),
            metadata: Some(task_json),
        };

        let new_manifest = agent_manifest_to_new(
            &agent_manifest,
            "final-session-id",
            "test-model",
            "test-provider",
            0,
        );

        // Metadata must survive conversion
        assert!(!new_manifest.metadata.is_null());
        let meta = new_manifest.metadata.as_object().unwrap();
        assert_eq!(meta["plan"], "Fix auth bug");
        assert_eq!(meta["active_todo"], "Add tests");
        assert_eq!(meta["todos_count"], 3);
        assert!(meta["changed_files"]
            .as_array()
            .unwrap()
            .contains(&serde_json::Value::String("src/auth.rs".into())));
    }

    #[test]
    fn agent_manifest_to_new_metadata_null_on_none() {
        let agent_manifest = lattice_agent::TurnManifestObject {
            hook_decisions: Vec::new(),
            tool_decisions: Vec::new(),
            prompt_snapshot: lattice_agent::PromptSnapshot {
                content_hash: "abc".to_string(),
                token_count: 0,
                model: "test-model".to_string(),
                turn_index: 0,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            session_id: "agent-temp-id".to_string(),
            branch: "main".to_string(),
            parent_commit_id: String::new(),
            request_id: "req-1".to_string(),
            metadata: None,
        };

        let new_manifest = agent_manifest_to_new(
            &agent_manifest,
            "final-session-id",
            "test-model",
            "test-provider",
            0,
        );

        // When agent manifest has no metadata, session manifest gets Null
        assert!(new_manifest.metadata.is_null());
    }

    #[test]
    fn parse_hook_decision_handles_externally_tagged_enum_json() {
        assert_eq!(
            parse_hook_decision("\"Continue\""),
            ("Continue".to_string(), None, None)
        );
        assert_eq!(
            parse_hook_decision(
                r#"{"RemoveSection":{"id":"rules.project","layer":null,"reason":"obsolete"}}"#
            ),
            (
                "RemoveSection".to_string(),
                Some("rules.project".to_string()),
                Some("obsolete".to_string())
            )
        );
        assert_eq!(
            parse_hook_decision(
                r#"{"SetBudget":{"id":"memory.runtime","layer":null,"budget":{"Fixed":256}}}"#
            ),
            (
                "SetBudget".to_string(),
                Some("memory.runtime".to_string()),
                None
            )
        );
        assert_eq!(
            parse_hook_decision(r#"{"Abort":"blocked"}"#),
            ("Abort".to_string(), None, Some("blocked".to_string()))
        );
    }
}
