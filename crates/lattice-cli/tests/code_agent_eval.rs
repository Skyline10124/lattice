//! Deterministic eval coverage for the runtime-backed coding profile path.
//!
//! Quick command: cargo test -p lattice-cli --test code_agent_eval

use lattice_cli::test_support::{
    scripted_content_response, spawn_scripted_openai_server, CodingAgentRuntimeTurnFixture,
};
use lattice_session::SessionStore;
use lattice_tools::{resolve_capabilities, CapabilitySelection, ToolBundle};

#[tokio::test]
async fn coding_agent_run_uses_runtime_turn_completion() {
    let Some(port) =
        spawn_scripted_openai_server(vec![scripted_content_response("runtime completion text")])
            .await
    else {
        eprintln!("skipping coding runtime turn eval: loopback bind denied");
        return;
    };
    let workdir = tempfile::tempdir().unwrap();

    let result =
        lattice_cli::test_support::run_coding_agent_runtime_turn(CodingAgentRuntimeTurnFixture {
            model: "scripted-coding-runtime-turn".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            workdir: workdir.path().to_path_buf(),
            prompt: "Use the runtime turn path".into(),
            json: false,
            save_session: false,
            session_id: None,
            session_store: None,
        })
        .await
        .unwrap();

    assert_eq!(result.completion.assistant_text, "runtime completion text");
    assert_eq!(
        result.rendered["content"],
        serde_json::Value::String("runtime completion text".into())
    );
    assert!(result
        .rendered
        .get("final_report")
        .and_then(|value| value.as_str())
        .is_some_and(|report| report.contains("## Verification")));
}

#[tokio::test]
async fn coding_agent_json_uses_runtime_turn_completion_final_report() {
    let Some(port) =
        spawn_scripted_openai_server(vec![scripted_content_response("json completion text")]).await
    else {
        eprintln!("skipping coding runtime json eval: loopback bind denied");
        return;
    };
    let workdir = tempfile::tempdir().unwrap();
    let session_store = tempfile::tempdir().unwrap();

    let result =
        lattice_cli::test_support::run_coding_agent_runtime_turn(CodingAgentRuntimeTurnFixture {
            model: "scripted-coding-runtime-json".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            workdir: workdir.path().to_path_buf(),
            prompt: "Render JSON from completion".into(),
            json: true,
            save_session: true,
            session_id: Some("coding-runtime-json-session".into()),
            session_store: Some(session_store.path().to_path_buf()),
        })
        .await
        .unwrap();

    assert_eq!(result.rendered["content"], "json completion text");
    assert_eq!(
        result.rendered["final_report"],
        result.completion.final_report.clone().unwrap()
    );
    assert_eq!(
        result.completion.saved_session_id.as_deref(),
        Some("coding-runtime-json-session")
    );

    let store = lattice_session::SqliteCasSessionStore::new(session_store.path());
    let saved = store
        .load("coding-runtime-json-session")
        .unwrap()
        .expect("runtime should persist the requested session id");
    let persisted_assistant = saved
        .messages
        .iter()
        .find(|message| message.role == "assistant")
        .expect("runtime should persist the assistant response");
    assert!(persisted_assistant.content.contains("json completion text"));
    assert!(persisted_assistant.content.contains(
        result
            .completion
            .final_report
            .as_deref()
            .expect("coding completion should include a final report")
    ));
}

#[test]
fn coding_profile_tool_bundles_resolve_code_capabilities_without_runtime_preset() {
    let resolved = resolve_capabilities(&CapabilitySelection {
        bundles: vec![
            ToolBundle::Workspace,
            ToolBundle::Filesystem,
            ToolBundle::Editing,
            ToolBundle::Shell,
            ToolBundle::Planning,
            ToolBundle::Web,
        ],
        enabled: Vec::new(),
    });

    for expected in [
        "workspace_context",
        "read_file",
        "read_files",
        "grep",
        "list_directory",
        "find_files",
        "write_file",
        "patch",
        "bash",
        "todo_write",
        "web_search",
    ] {
        assert!(resolved.tool_names.contains(expected), "{expected} missing");
    }
    assert!(!resolved.tool_names.contains("multi_patch"));
}
