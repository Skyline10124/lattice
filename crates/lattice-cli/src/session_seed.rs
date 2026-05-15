/// Load the latest turn manifest metadata from a previous session
/// and deserialize it into a TaskState seed. Returns `None` when
/// the session has no manifests, no metadata, or deserialization fails.
pub(crate) fn load_latest_manifest_metadata(
    runtime: &lattice_runtime::Runtime,
    previous_session: Option<&crate::session::Session>,
) -> Option<lattice_agent::TaskState> {
    let session = previous_session?;
    let store = runtime.session_store()?;
    let manifest_ids = store.latest_turn_manifest_ids(&session.id).ok()?;
    let latest_id = match manifest_ids.last() {
        Some(id) => id.clone(),
        None => {
            let summaries = store.list_turn_manifests(&session.id).ok()?;
            summaries.last()?.id.clone()
        }
    };
    let manifest = store.get_turn_manifest(&latest_id).ok()??;
    if manifest.metadata.is_null() {
        return None;
    }
    serde_json::from_value::<lattice_agent::TaskState>(manifest.metadata).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_session::SessionStore;

    #[test]
    fn load_latest_manifest_metadata_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "lattice-session-seed-metadata-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = std::sync::Arc::new(lattice_session::SqliteCasSessionStore::new(&dir));

        let session = lattice_session::Session::new("test-model".into(), "test-provider".into());
        let metadata = serde_json::json!({
            "todos": [
                {"id": 1, "content": "Fix auth bug", "status": "in_progress"},
                {"id": 2, "content": "Run tests", "status": "pending"}
            ],
            "changed_files": [
                {"path": "src/auth.rs", "operation": "update", "diagnostic": "line 42"}
            ],
            "tool_failures": [
                {"call_id": "tc-1", "tool_name": "grep", "message": "no results", "timestamp": "2026-05-12T00:00:00Z"}
            ],
            "verifications": [
                {"command": "cargo test", "output_trimmed": "2 failures", "timestamp": "2026-05-12T00:00:01Z"}
            ],
            "unverified_work": ["cargo test"]
        });
        let manifest = lattice_session::NewTurnManifest {
            session_id: session.id.clone(),
            commit_id: None,
            turn_index: 0,
            request_id: "req-test".to_string(),
            parent_manifest_ids: Vec::new(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            prompt: lattice_session::PromptSnapshot {
                total_tokens: 100,
                effective_context_budget: 4096,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            hooks: Vec::new(),
            tools: Vec::new(),
            metadata,
        };

        store
            .save_with_manifests(&session, vec![manifest])
            .expect("save_with_manifests should succeed");
        let runtime = lattice_runtime::Runtime::builder()
            .session_store(store)
            .build();

        let task_state =
            load_latest_manifest_metadata(&runtime, Some(&session)).expect("metadata seed");
        assert_eq!(task_state.todos.len(), 2);
        assert_eq!(task_state.todos[0].content, "Fix auth bug");
        assert_eq!(
            task_state.todos[0].status,
            lattice_agent::TodoStatus::InProgress
        );
        assert_eq!(task_state.changed_files[0].path, "src/auth.rs");
        assert_eq!(task_state.tool_failures[0].tool_name, "grep");
        assert_eq!(task_state.verifications[0].command, "cargo test");
        assert_eq!(task_state.unverified_work[0], "cargo test");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
