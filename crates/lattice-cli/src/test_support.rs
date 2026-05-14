#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry};
use lattice_runtime::{Runtime, TurnCompletion, TurnRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::coding_agent::{self, CodingAgentBuildOptions};

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

pub async fn run_coding_agent_runtime_turn(
    fixture: CodingAgentRuntimeTurnFixture,
) -> Result<CodingAgentRuntimeTurnResult> {
    let mut builder = Runtime::builder();
    if let Some(path) = fixture.session_store.as_ref() {
        builder = builder.session_store(Arc::new(lattice_session::FileSessionStore::new(path)));
    }
    let mut runtime = builder.build();
    runtime.register_model(ModelCatalogEntry {
        canonical_id: fixture.model.clone(),
        context_length: 8192,
        providers: vec![CatalogProviderEntry {
            provider_id: "ollama".into(),
            api_model_id: fixture.model.clone(),
            priority: 1,
            credential_keys: HashMap::new(),
            base_url: Some(fixture.base_url),
            api_protocol: ApiProtocol::OpenAiChat,
            provider_specific: HashMap::new(),
        }],
        aliases: Vec::new(),
    })?;

    let security = lattice_runtime::build_runtime_security(
        &lattice_runtime::RuntimeSecurityConfig::default(),
        &fixture.workdir,
    )?;
    let built = coding_agent::build_coding_agent_with_runtime(
        runtime,
        CodingAgentBuildOptions {
            model: fixture.model,
            provider_override: None,
            workdir: fixture.workdir,
            credentials: HashMap::new(),
            previous_session: None,
            prior_messages: Vec::new(),
            thinking_effort: None,
            security,
        },
    )?;
    let completion = coding_agent::submit_coding_runtime_turn(
        &built.runtime,
        &built.handle_id,
        TurnRequest {
            input: fixture.prompt,
            max_turns: 2,
            persist_session: fixture.save_session,
            session_id: fixture.session_id,
            cancellation_id: None,
        },
        None,
    )
    .await?;
    let rendered = coding_agent::render_completion_value(&completion);

    if fixture.json {
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
    let mut builder = Runtime::builder();
    if let Some(path) = fixture.session_store.as_ref() {
        builder = builder.session_store(Arc::new(lattice_session::FileSessionStore::new(path)));
    }
    let mut runtime = builder.build();
    register_scripted_model(&mut runtime, &fixture.model, fixture.base_url);
    let security = lattice_runtime::build_runtime_security(
        &lattice_runtime::RuntimeSecurityConfig::default(),
        &fixture.workdir,
    )?;
    let workspace = fixture.workdir.to_string_lossy().to_string();
    let spec =
        crate::commands::run::run_turn_spec(fixture.model.clone(), None, None, workspace.clone());
    let handle_id = runtime.get_or_create_agent_handle_for_session(
        spec,
        security,
        fixture.session_id.clone(),
    )?;
    let completion = crate::commands::run::submit_run_runtime_turn(
        &runtime,
        &handle_id,
        TurnRequest {
            input: fixture.prompt,
            max_turns: 2,
            persist_session: fixture.save_session,
            session_id: fixture.session_id,
            cancellation_id: None,
        },
        None,
    )
    .await?;
    let rendered = crate::commands::run::run_completion_value(&completion);

    if fixture.json {
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

pub async fn debug_chat_runtime_turn(
    fixture: DebugRuntimeTurnFixture,
) -> Result<DebugRuntimeTurnResult> {
    let mut runtime = Runtime::builder().build();
    register_scripted_model(&mut runtime, &fixture.model, fixture.base_url);
    let security = lattice_runtime::build_runtime_security(
        &lattice_runtime::RuntimeSecurityConfig::default(),
        &fixture.workdir,
    )?;
    let workspace = fixture.workdir.to_string_lossy().to_string();
    let spec = crate::commands::debug::debug_turn_spec(fixture.model, None, workspace);
    let handle_id = runtime.get_or_create_agent_handle_for_session(spec, security, None)?;
    let summary = crate::commands::debug::execute_debug_runtime_turn(
        &runtime,
        &handle_id,
        TurnRequest {
            input: fixture.prompt,
            max_turns: 2,
            persist_session: false,
            session_id: None,
            cancellation_id: None,
        },
        std::time::Instant::now(),
    )
    .await?;
    let response_len = summary.completion.assistant_text.len();
    let model = summary.completion.model.clone();
    let provider = summary.completion.provider.clone();

    Ok(DebugRuntimeTurnResult {
        completion: summary.completion,
        response_len,
        tool_names: summary.tool_names,
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
