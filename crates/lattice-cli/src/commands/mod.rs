pub mod agent_inventory;
pub mod doctor;
pub mod models;
pub mod resolve;
pub mod run;
pub mod session;

use std::path::{Path, PathBuf};

/// Resolve the agents directory from override, env var, HOME, or default.
/// Validates that the resolved path is not in a system-critical location.
pub fn safe_agents_dir(override_path: Option<&str>) -> Result<PathBuf, String> {
    let path = if let Some(p) = override_path {
        PathBuf::from(p)
    } else if let Ok(dir) = std::env::var("LATTICE_AGENTS_DIR") {
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".lattice").join("agents")
    } else {
        PathBuf::from(".lattice/agents")
    };

    validate_agents_dir(&path)?;
    Ok(path)
}

/// Reject agent directories in system-critical or world-writable paths.
pub fn validate_agents_dir(path: &Path) -> Result<(), String> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path_str = canonical.to_string_lossy();

    let dangerous_prefixes = [
        "/etc", "/usr", "/var", "/sys", "/proc", "/dev", "/boot", "/root",
    ];
    for prefix in &dangerous_prefixes {
        if path_str.starts_with(prefix) {
            return Err(format!(
                "Agents directory '{}' is inside a system path ({}). \
                 Set LATTICE_AGENTS_DIR to a safe location (e.g., under $HOME or your project).",
                path.display(),
                prefix
            ));
        }
    }

    if path_str.starts_with("/tmp") {
        return Err(format!(
            "Agents directory '{}' is inside /tmp which is world-writable and unsafe. \
             Set LATTICE_AGENTS_DIR to a safe location.",
            path.display()
        ));
    }

    Ok(())
}

#[cfg(test)]
mod run_runtime {
    use lattice_session::SessionStore;

    #[tokio::test]
    async fn run_command_uses_runtime_turn_completion() {
        let Some(port) = crate::test_support::spawn_scripted_openai_server(vec![
            crate::test_support::scripted_content_response("run completion text"),
        ])
        .await
        else {
            eprintln!("skipping run runtime eval: loopback bind denied");
            return;
        };
        let workdir = tempfile::tempdir().unwrap();

        let result = crate::test_support::run_command_runtime_turn(
            crate::test_support::RunCommandRuntimeTurnFixture {
                model: "scripted-run-runtime-turn".into(),
                base_url: format!("http://127.0.0.1:{port}"),
                workdir: workdir.path().to_path_buf(),
                prompt: "Use runtime run".into(),
                json: false,
                save_session: false,
                session_id: None,
                session_store: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.completion.assistant_text, "run completion text");
        assert_eq!(result.rendered["content"], "run completion text");
    }

    #[tokio::test]
    async fn run_command_save_uses_runtime_persistence() {
        let Some(port) = crate::test_support::spawn_scripted_openai_server(vec![
            crate::test_support::scripted_content_response("persisted run text"),
        ])
        .await
        else {
            eprintln!("skipping run runtime persistence eval: loopback bind denied");
            return;
        };
        let workdir = tempfile::tempdir().unwrap();
        let session_store = tempfile::tempdir().unwrap();

        let result = crate::test_support::run_command_runtime_turn(
            crate::test_support::RunCommandRuntimeTurnFixture {
                model: "scripted-run-runtime-save".into(),
                base_url: format!("http://127.0.0.1:{port}"),
                workdir: workdir.path().to_path_buf(),
                prompt: "Persist through runtime".into(),
                json: true,
                save_session: true,
                session_id: Some("run-runtime-session".into()),
                session_store: Some(session_store.path().to_path_buf()),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            result.completion.saved_session_id.as_deref(),
            Some("run-runtime-session")
        );
        let store = lattice_session::SqliteCasSessionStore::new(session_store.path());
        let saved = store
            .load("run-runtime-session")
            .unwrap()
            .expect("runtime should save run session");
        assert!(saved
            .messages
            .iter()
            .any(|message| message.role == "assistant"
                && message.content.contains("persisted run text")));
    }

    #[tokio::test]
    async fn run_command_no_save_does_not_persist_session() {
        let Some(port) = crate::test_support::spawn_scripted_openai_server(vec![
            crate::test_support::scripted_content_response("transient run text"),
        ])
        .await
        else {
            eprintln!("skipping run no-save eval: loopback bind denied");
            return;
        };
        let workdir = tempfile::tempdir().unwrap();
        let session_store = tempfile::tempdir().unwrap();

        let result = crate::test_support::run_command_runtime_turn(
            crate::test_support::RunCommandRuntimeTurnFixture {
                model: "scripted-run-runtime-nosave".into(),
                base_url: format!("http://127.0.0.1:{port}"),
                workdir: workdir.path().to_path_buf(),
                prompt: "Run without persisting".into(),
                json: true,
                save_session: false,
                session_id: Some("run-runtime-nosave-session".into()),
                session_store: Some(session_store.path().to_path_buf()),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.completion.saved_session_id, None);
        let store = lattice_session::SqliteCasSessionStore::new(session_store.path());
        assert!(
            store.load("run-runtime-nosave-session").unwrap().is_none(),
            "no-save runtime run should not persist a session"
        );
    }

    #[tokio::test]
    async fn generic_cli_runtime_turn_uses_profile_backed_submission() {
        let Some(port) = crate::test_support::spawn_scripted_openai_server(vec![
            crate::test_support::scripted_content_response("generic cli runtime text"),
        ])
        .await
        else {
            eprintln!("skipping generic CLI runtime eval: loopback bind denied");
            return;
        };
        let workdir = tempfile::tempdir().unwrap();

        let result = crate::test_support::run_command_runtime_turn(
            crate::test_support::RunCommandRuntimeTurnFixture {
                model: "scripted-generic-cli-runtime-turn".into(),
                base_url: format!("http://127.0.0.1:{port}"),
                workdir: workdir.path().to_path_buf(),
                prompt: "Use profile-backed runtime".into(),
                json: false,
                save_session: false,
                session_id: None,
                session_store: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.completion.assistant_text, "generic cli runtime text");
        assert_eq!(
            result.completion.status,
            lattice_runtime::TurnStatus::Completed
        );
        assert_eq!(result.rendered["content"], "generic cli runtime text");
    }
}
