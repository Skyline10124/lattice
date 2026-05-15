#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry};
use lattice_profile::ProfileSource;
use lattice_runtime::{Runtime, RuntimeAgentSession, TurnCompletion, TurnRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::coding_agent;
use crate::profile_runtime::{prepare_profile_runtime_session, ProfileRuntimeSessionRequest};

pub struct CodingAgentRuntimeTurnFixture {
    pub model: String,
    pub base_url: String,
    pub workdir: PathBuf,
    pub prompt: String,
    pub json: bool,
    pub save_session: bool,
    pub session_id: Option<String>,
    pub session_store: Option<PathBuf>,
}

impl CodingAgentRuntimeTurnFixture {
    pub fn with_session_store(mut self, path: PathBuf) -> Self {
        self.session_store = Some(path);
        self
    }
}

pub struct CodingAgentRuntimeTurnResult {
    pub completion: TurnCompletion,
    pub rendered: serde_json::Value,
}

pub struct RunCommandRuntimeTurnFixture {
    pub model: String,
    pub base_url: String,
    pub workdir: PathBuf,
    pub prompt: String,
    pub json: bool,
    pub save_session: bool,
    pub session_id: Option<String>,
    pub session_store: Option<PathBuf>,
}

pub struct RunCommandRuntimeTurnResult {
    pub completion: TurnCompletion,
    pub rendered: serde_json::Value,
}

pub struct DebugRuntimeTurnFixture {
    pub model: String,
    pub base_url: String,
    pub workdir: PathBuf,
    pub prompt: String,
}

pub struct DebugRuntimeTurnResult {
    pub completion: TurnCompletion,
    pub response_len: usize,
    pub tool_names: Vec<String>,
    pub model: String,
    pub provider: String,
}

pub struct GenericRuntimeTurnFixture {
    pub model: String,
    pub base_url: String,
    pub workdir: PathBuf,
    pub prompt: String,
    pub json: bool,
    pub save_session: bool,
    pub session_id: Option<String>,
    pub session_store: Option<PathBuf>,
    pub profile_name: String,
    pub tool_bundles: Vec<String>,
}

impl GenericRuntimeTurnFixture {
    pub fn with_session_store(mut self, path: PathBuf) -> Self {
        self.session_store = Some(path);
        self
    }
}

pub struct GenericRuntimeTurnResult {
    pub runtime: Runtime,
    pub session: RuntimeAgentSession,
    pub completion: TurnCompletion,
}

pub async fn run_coding_agent_runtime_turn(
    fixture: CodingAgentRuntimeTurnFixture,
) -> Result<CodingAgentRuntimeTurnResult> {
    let json = fixture.json;
    let generic = run_generic_runtime_turn(GenericRuntimeTurnFixture {
        model: fixture.model,
        base_url: fixture.base_url,
        workdir: fixture.workdir,
        prompt: fixture.prompt,
        json,
        save_session: fixture.save_session,
        session_id: fixture.session_id,
        session_store: fixture.session_store,
        profile_name: "coding".into(),
        tool_bundles: vec![
            "workspace".into(),
            "filesystem".into(),
            "editing".into(),
            "shell".into(),
            "planning".into(),
            "web".into(),
        ],
    })
    .await?;
    let completion = coding_agent::require_successful_runtime_completion(generic.completion)?;
    let rendered = coding_agent::render_completion_value(&completion);

    if json {
        assert!(
            rendered.is_object(),
            "JSON rendering should produce an object"
        );
    }

    Ok(CodingAgentRuntimeTurnResult {
        completion,
        rendered,
    })
}

pub async fn run_command_runtime_turn(
    fixture: RunCommandRuntimeTurnFixture,
) -> Result<RunCommandRuntimeTurnResult> {
    let json = fixture.json;
    let generic = run_generic_runtime_turn(GenericRuntimeTurnFixture {
        model: fixture.model,
        base_url: fixture.base_url,
        workdir: fixture.workdir,
        prompt: fixture.prompt,
        json,
        save_session: fixture.save_session,
        session_id: fixture.session_id,
        session_store: fixture.session_store,
        profile_name: "run".into(),
        tool_bundles: vec!["workspace".into(), "filesystem".into()],
    })
    .await?;
    let completion =
        crate::coding_agent::require_successful_runtime_completion(generic.completion)?;
    let rendered = crate::commands::run::run_completion_value(&completion);

    if json {
        assert!(
            rendered.is_object(),
            "JSON rendering should produce an object"
        );
    }

    Ok(RunCommandRuntimeTurnResult {
        completion,
        rendered,
    })
}

pub async fn run_generic_runtime_turn(
    fixture: GenericRuntimeTurnFixture,
) -> Result<GenericRuntimeTurnResult> {
    write_profile_package(
        &fixture.workdir,
        &fixture.profile_name,
        &fixture.model,
        "You are a deterministic generic runtime test agent.",
        &fixture.tool_bundles,
    )?;
    let mut builder = Runtime::builder();
    let default_session_store = std::env::temp_dir().join(format!(
        "lattice-cli-generic-session-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let session_store_path = fixture
        .session_store
        .unwrap_or_else(|| default_session_store.clone());
    std::fs::create_dir_all(&session_store_path)?;
    builder = builder.session_store(Arc::new(lattice_session::SqliteCasSessionStore::new(
        &session_store_path,
    )));
    let mut runtime = builder.build();
    register_scripted_model(&mut runtime, &fixture.model, fixture.base_url);
    let session = prepare_profile_runtime_session(
        &runtime,
        ProfileRuntimeSessionRequest {
            source: ProfileSource::Name(fixture.profile_name),
            project_root: fixture.workdir.clone(),
            global_agents_dir: fixture.workdir.join(".unused-global-agents"),
            security_config: crate::config::SecurityConfig::default(),
            provider_override: None,
            session_id: fixture.session_id.clone(),
            task_state_seed: None,
        },
    )?;
    let submitted = runtime
        .submit_session_turn(
            session.clone(),
            TurnRequest {
                input: fixture.prompt,
                max_turns: 2,
                persist_session: fixture.save_session,
                session_id: fixture.session_id,
                cancellation_id: None,
            },
        )
        .await?;
    let mut events = submitted.events;
    while events.next().await.is_some() {}
    let completion = submitted.completion.await?;

    Ok(GenericRuntimeTurnResult {
        runtime,
        session,
        completion,
    })
}

pub async fn submit_profile_runtime_turn(
    runtime: &Runtime,
    session: RuntimeAgentSession,
    request: TurnRequest,
    mut renderer: Option<&mut crate::commands::run::RuntimeEventRenderer>,
) -> Result<TurnCompletion> {
    let submitted = runtime.submit_session_turn(session, request).await?;
    let mut events = submitted.events;
    while let Some(event) = events.next().await {
        if let Some(renderer) = renderer.as_deref_mut() {
            renderer.render(&event)?;
        }
    }
    Ok(submitted.completion.await?)
}

pub async fn submit_coding_profile_runtime_turn(
    runtime: &Runtime,
    session: RuntimeAgentSession,
    request: TurnRequest,
    mut renderer: Option<&mut crate::coding_agent::CodingEventRenderer>,
) -> Result<TurnCompletion> {
    let submitted = runtime.submit_session_turn(session, request).await?;
    let mut events = submitted.events;
    while let Some(event) = events.next().await {
        if let Some(renderer) = renderer.as_deref_mut() {
            renderer.render_runtime(&event)?;
        }
    }
    Ok(submitted.completion.await?)
}

fn write_profile_package(
    project_root: &PathBuf,
    name: &str,
    model: &str,
    system_prompt: &str,
    bundles: &[String],
) -> Result<()> {
    let dir = project_root.join(".lattice").join("agents").join(name);
    std::fs::create_dir_all(&dir)?;
    let bundles = bundles
        .iter()
        .map(|bundle| format!("\"{}\"", bundle))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        dir.join("agent.toml"),
        format!(
            r#"
schema_version = 1

[agent]
name = "{name}"
model = "{model}"

[prompt]
system = "{system_prompt}"

[tools]
bundles = [{bundles}]
enabled = []

[security]
sandbox_mode = "project"

[context]
project_instructions = false
workspace_context = false

[behavior]
max_turns = 2
"#
        ),
    )?;
    Ok(())
}

pub async fn debug_chat_runtime_turn(
    fixture: DebugRuntimeTurnFixture,
) -> Result<DebugRuntimeTurnResult> {
    let generic = run_generic_runtime_turn(GenericRuntimeTurnFixture {
        model: fixture.model,
        base_url: fixture.base_url,
        workdir: fixture.workdir,
        prompt: fixture.prompt,
        json: false,
        save_session: false,
        session_id: None,
        session_store: None,
        profile_name: "debug".into(),
        tool_bundles: vec!["workspace".into(), "filesystem".into()],
    })
    .await?;
    let completion =
        crate::coding_agent::require_successful_runtime_completion(generic.completion)?;
    let response_len = completion.assistant_text.len();
    let model = completion.model.clone();
    let provider = completion.provider.clone();

    Ok(DebugRuntimeTurnResult {
        completion,
        response_len,
        tool_names: Vec::new(),
        model,
        provider,
    })
}

pub async fn spawn_scripted_openai_server(responses: Vec<String>) -> Option<u16> {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return None,
        Err(error) => panic!("failed to bind scripted OpenAI server: {error}"),
    };
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        for body in responses {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let _request = read_http_request(&mut stream).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });

    Some(port)
}

pub fn scripted_content_response(text: &str) -> String {
    let mut body = String::new();
    body.push_str(&content_chunk(text));
    body.push_str(&stop_chunk());
    body.push_str("data: [DONE]\n\n");
    body
}

fn register_scripted_model(runtime: &mut Runtime, name: &str, base_url: String) {
    runtime
        .register_model(ModelCatalogEntry {
            canonical_id: name.to_string(),
            context_length: 8192,
            providers: vec![CatalogProviderEntry {
                provider_id: "ollama".into(),
                api_model_id: name.to_string(),
                priority: 1,
                credential_keys: HashMap::new(),
                base_url: Some(base_url),
                api_protocol: ApiProtocol::OpenAiChat,
                provider_specific: HashMap::new(),
            }],
            aliases: Vec::new(),
        })
        .unwrap();
}

async fn read_http_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = stream.read(&mut buf).await {
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if request_complete(&data) || data.len() > 2 * 1024 * 1024 {
            break;
        }
    }
    String::from_utf8_lossy(&data).to_string()
}

fn request_complete(data: &[u8]) -> bool {
    let Some(header_end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&data[..header_end]);
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    match content_length {
        Some(len) => data.len() >= header_end + 4 + len,
        None => true,
    }
}

fn content_chunk(text: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": text },
            "finish_reason": null
        }]
    });
    format!("data: {}\n\n", chunk)
}

fn stop_chunk() -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }]
    });
    format!("data: {}\n\n", chunk)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn generic_runtime_turn_fixture_submits_profile_backed_turn() {
        let Some(port) =
            super::spawn_scripted_openai_server(vec![super::scripted_content_response(
                "generic fixture text",
            )])
            .await
        else {
            eprintln!("skipping generic runtime fixture test: loopback bind denied");
            return;
        };
        let workdir = tempfile::tempdir().unwrap();
        let session_store = tempfile::tempdir().unwrap();
        let result = super::run_generic_runtime_turn(super::GenericRuntimeTurnFixture {
            model: "scripted-generic-runtime-fixture".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            workdir: workdir.path().to_path_buf(),
            prompt: "Use generic runtime fixture".into(),
            json: false,
            save_session: true,
            session_id: Some("generic-runtime-fixture-session".into()),
            session_store: Some(session_store.path().to_path_buf()),
            profile_name: "fixture".into(),
            tool_bundles: vec!["workspace".into(), "filesystem".into()],
        })
        .await
        .unwrap();

        assert_eq!(result.completion.assistant_text, "generic fixture text");
        assert_eq!(
            result.completion.status,
            lattice_runtime::TurnStatus::Completed
        );
        assert_eq!(
            result.completion.saved_session_id.as_deref(),
            Some("generic-runtime-fixture-session")
        );
        assert_eq!(result.session.spec.profile_name, "fixture");
        assert!(result
            .session
            .profile_snapshot
            .source
            .source_fingerprint
            .as_deref()
            .is_some_and(|value| value.starts_with("sha256:")));
    }
}
