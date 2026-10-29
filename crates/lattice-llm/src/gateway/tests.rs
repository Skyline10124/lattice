// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeMap, VecDeque},
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::StreamExt;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;
use tracing::instrument::WithSubscriber;

use super::RequestGateway;
use crate::test_tls::TLS;
use crate::{ChatRequest, LlmEvent, ModelTarget, ReplyOutcome, StreamControl};

const DONE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
const MESSAGES_DONE: &str = "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"ok\"}}\n\nevent: content_block_stop\ndata: {\"index\":0}\n\nevent: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\nevent: message_stop\ndata: {}\n\n";
const CONTENT_DONE: &str = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}\n\n";

struct Received {
    connection: usize,
    path: String,
    headers: BTreeMap<String, String>,
    body: serde_json::Value,
}

struct Response {
    message: String,
    close: bool,
    tail: Option<(oneshot::Receiver<()>, String)>,
}

struct Server {
    url: String,
    received: mpsc::UnboundedReceiver<Received>,
    task: JoinHandle<()>,
    responses: Arc<Mutex<VecDeque<Response>>>,
}

impl Server {
    async fn start(tls: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, received) = mpsc::unbounded_channel();
        let responses = Arc::new(Mutex::new(VecDeque::new()));
        let queued = responses.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            let mut id = 0;
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        id += 1;
                        let tx = tx.clone();
                        let queued = queued.clone();
                        connections.spawn(async move {
                            if tls {
                                let acceptor = TlsAcceptor::from(TLS.server.clone());
                                if let Ok(socket) = acceptor.accept(socket).await {
                                    serve(socket, id, tx, queued).await;
                                }
                            } else {
                                serve(socket, id, tx, queued).await;
                            }
                        });
                    }
                    Some(result) = connections.join_next(), if !connections.is_empty() => result.unwrap(),
                }
            }
        });
        Self {
            url: format!(
                "{}://localhost:{}/v1",
                if tls { "https" } else { "http" },
                address.port()
            ),
            received,
            task,
            responses,
        }
    }

    async fn next(&mut self) -> Received {
        tokio::time::timeout(Duration::from_secs(5), self.received.recv())
            .await
            .unwrap()
            .unwrap()
    }

    fn respond(&self, response: String, close: bool) {
        self.responses.lock().unwrap().push_back(Response {
            message: response,
            close,
            tail: None,
        });
    }

    fn respond_with_tail(&self, response: String, tail: String) -> oneshot::Sender<()> {
        let (release, released) = oneshot::channel();
        self.responses.lock().unwrap().push_back(Response {
            message: response,
            close: false,
            tail: Some((released, tail)),
        });
        release
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut socket: impl AsyncRead + AsyncWrite + Unpin,
    connection: usize,
    tx: mpsc::UnboundedSender<Received>,
    responses: Arc<Mutex<VecDeque<Response>>>,
) {
    loop {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if socket.read_exact(&mut byte).await.is_err() {
                return;
            }
            head.push(byte[0]);
            assert!(head.len() < 32 * 1024);
        }
        let head = String::from_utf8(head).unwrap();
        let mut lines = head.lines();
        let path = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned();
        let headers: BTreeMap<_, _> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let length = headers["content-length"].parse().unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        let body = serde_json::from_slice(&body).unwrap();
        let response_body = if path.ends_with("/messages") {
            MESSAGES_DONE
        } else if path.contains(":streamGenerateContent") {
            CONTENT_DONE
        } else {
            DONE
        };
        tx.send(Received {
            connection,
            path,
            headers,
            body,
        })
        .unwrap();
        let response = responses.lock().unwrap().pop_front().unwrap_or_else(|| Response {
            message: format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{response_body}", response_body.len()),
            close: false, tail: None,
        });
        if socket.write_all(response.message.as_bytes()).await.is_err() {
            return;
        }
        if let Some((released, tail)) = response.tail
            && (released.await.is_err() || socket.write_all(tail.as_bytes()).await.is_err())
        {
            return;
        }
        if response.close {
            let _ = socket.shutdown().await;
            return;
        }
    }
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // 临时目录权限取决于 umask，这里固定为 0700，避免结果随环境变化。
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn configure(dir: &std::path::Path, url: &str, protocol: &str, auth: &str) {
    std::fs::write(
        dir.join("providers.toml"),
        format!("[providers.test]\nprotocol = '{protocol}'\nbase_url = '{url}'\nauth = '{auth}'\n"),
    )
    .unwrap();
}

fn request(provider: &str) -> ChatRequest {
    let mut request = ChatRequest::new(
        Vec::new(),
        ModelTarget {
            model: "model-id".into(),
            provider: Some(provider.into()),
        },
    );
    request.client_request_id = Some("session-id".into());
    request
}

async fn complete(stream: crate::LlmStream) {
    let events = tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
        .await
        .unwrap();
    assert!(
        matches!(
            events.last(),
            Some(LlmEvent::Finished {
                outcome: ReplyOutcome::Complete { .. },
                ..
            })
        ),
        "{events:?}"
    );
}

#[derive(Clone, Default)]
struct LogOutput(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn capture_logs<T>(future: impl std::future::Future<Output = T>) -> (T, String) {
    let output = LogOutput::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("lattice_llm=debug")
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let subscriber = tracing::Dispatch::new(subscriber);
    // 同时保留空 subscriber，使并行测试首次注册调用点时按全部 subscriber 计算兴趣。
    let _other_threads = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let result = future.with_subscriber(subscriber).await;
    let logs = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    (result, logs)
}

#[tokio::test]
async fn none_sends_protocol_request_without_credentials() {
    let mut server = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "none");
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let turn = gateway.open_turn();
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    let received = server.next().await;
    assert_eq!(received.connection, 1);
    assert_eq!(received.path, "/v1/chat/completions");
    assert_eq!(received.body["model"], "model-id");
    assert_eq!(received.headers["x-client-request-id"], "session-id");
    assert!(!received.headers.contains_key("authorization"));
    assert!(!received.headers.contains_key("x-api-key"));
    assert!(!received.headers.contains_key("x-goog-api-key"));
}

#[tokio::test]
async fn protocol_and_auth_independently_select_wire_requests() {
    let mut server = Server::start(true).await;
    let directory = private_dir();
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let turn = gateway.open_turn();
    for protocol in ["chat-completions", "anthropic-messages", "generate-content"] {
        for auth in ["bearer", "x-api-key", "x-goog-api-key", "none"] {
            configure(directory.path(), &server.url, protocol, auth);
            if auth != "none" {
                crate::auth::AuthStore::new(directory.path().into())
                    .set("test", "test-credential")
                    .unwrap();
            }
            let mut request = request("test");
            request.target.model = "family/model id".into();
            request.temperature = Some(0.5);
            request.max_tokens = Some(1);
            request.max_completion_tokens = Some(2);
            request
                .messages
                .push(lattice_conversation::ConversationItem::Message(
                    lattice_conversation::NativeMessage::new(
                        lattice_conversation::MessageRole::User,
                        None,
                        vec![lattice_conversation::MessagePart::semantic_only(
                            lattice_conversation::MessageSemantic::Text {
                                text: "hello".into(),
                            },
                        )],
                    ),
                ));
            request.tools.push(crate::ToolDefinition::new(
                "tool_name",
                "description",
                serde_json::json!({"type":"object"}),
            ));
            complete(
                turn.stream_chat(request, StreamControl::default())
                    .await
                    .unwrap(),
            )
            .await;
            let received = server.next().await;
            assert!(received.body.get("temperature").is_none());
            assert!(received.body.get("max_completion_tokens").is_none());
            if protocol != "anthropic-messages" {
                assert!(received.body.get("max_tokens").is_none());
                assert!(received.body.get("generationConfig").is_none());
            }
            let expected_path = match protocol {
                "chat-completions" => "/v1/chat/completions",
                "anthropic-messages" => "/v1/v1/messages",
                _ => "/v1/models/family%2Fmodel%20id:streamGenerateContent?alt=sse",
            };
            assert_eq!(received.path, expected_path, "{protocol} {auth}");
            for (header, expected_auth) in [
                ("authorization", "bearer"),
                ("x-api-key", "x-api-key"),
                ("x-goog-api-key", "x-goog-api-key"),
            ] {
                let expected = (auth == expected_auth).then_some(if auth == "bearer" {
                    "Bearer test-credential"
                } else {
                    "test-credential"
                });
                assert_eq!(
                    received.headers.get(header).map(String::as_str),
                    expected,
                    "{protocol} {auth} {header}"
                );
            }
            assert_eq!(
                received
                    .headers
                    .get("anthropic-version")
                    .map(String::as_str),
                (protocol == "anthropic-messages").then_some("2023-06-01")
            );
            assert_eq!(
                received.headers["accept"],
                "text/event-stream, application/json"
            );
            assert!(received.headers["user-agent"].starts_with("lattice/"));
            assert_eq!(received.headers["x-client-request-id"], "session-id");
            for header in received.headers.keys() {
                assert!(
                    [
                        "host",
                        "content-length",
                        "content-type",
                        "authorization",
                        "x-api-key",
                        "x-goog-api-key",
                        "anthropic-version",
                        "user-agent",
                        "accept",
                        "x-client-request-id"
                    ]
                    .contains(&header.as_str()),
                    "unexpected header {header}"
                );
            }
            match protocol {
                "chat-completions" => {
                    assert_eq!(received.body["model"], "family/model id");
                    assert_eq!(received.body["messages"][0]["content"], "hello");
                    assert_eq!(received.body["tools"][0]["function"]["name"], "tool_name");
                    assert_eq!(received.body["stream"], true);
                    assert_eq!(received.body["stream_options"]["include_usage"], true);
                }
                "anthropic-messages" => {
                    assert_eq!(received.body["model"], "family/model id");
                    assert_eq!(received.body["messages"][0]["content"][0]["text"], "hello");
                    assert_eq!(received.body["tools"][0]["name"], "tool_name");
                    assert_eq!(received.body["max_tokens"], 32_768);
                    assert_eq!(received.body["stream"], true);
                }
                _ => {
                    assert_eq!(received.body["contents"][0]["parts"][0]["text"], "hello");
                    assert_eq!(
                        received.body["tools"][0]["functionDeclarations"][0]["name"],
                        "tool_name"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn frozen_connection_preserves_sampling_output_limits_and_usage_request() {
    let mut server = Server::start(true).await;
    for (provider, model) in [
        (crate::ProviderKind::OpenAi, "gpt-model"),
        (crate::ProviderKind::OpenAiCompatible, "compatible-model"),
        (crate::ProviderKind::Anthropic, "claude-model"),
        (crate::ProviderKind::Gemini, "gemini-model"),
    ] {
        let connection = crate::LlmConnection::new(
            crate::ResolvedModel {
                alias: model.into(),
                provider: provider.clone(),
                provider_model: model.into(),
                base_url: server.url.clone(),
                api_key_env: String::new(),
                credential_status: crate::CredentialStatus::Present,
            },
            "frozen-key".into(),
            vec![TLS.ca_der.clone()],
        )
        .unwrap();
        for max_completion_tokens in [None, Some(123)] {
            let mut request = request("test");
            request.temperature = Some(0.25);
            request.max_tokens = Some(456);
            request.max_completion_tokens = max_completion_tokens;
            complete(
                connection
                    .stream(request, StreamControl::default())
                    .await
                    .unwrap(),
            )
            .await;
            let received = server.next().await;
            match provider {
                crate::ProviderKind::OpenAi | crate::ProviderKind::OpenAiCompatible => {
                    assert_eq!(received.body["model"], model);
                    assert_eq!(received.body["temperature"], 0.25);
                    assert_eq!(received.body["stream"], true);
                    assert_eq!(received.body["stream_options"]["include_usage"], true);
                    if let Some(limit) = max_completion_tokens {
                        assert_eq!(received.body["max_completion_tokens"], limit);
                        assert!(received.body.get("max_tokens").is_none());
                    } else {
                        assert_eq!(received.body["max_tokens"], 456);
                        assert!(received.body.get("max_completion_tokens").is_none());
                    }
                }
                crate::ProviderKind::Anthropic => {
                    assert_eq!(received.body["model"], model);
                    assert_eq!(received.body["temperature"], 0.25);
                    assert_eq!(received.body["max_tokens"], 456);
                    assert!(received.body.get("max_completion_tokens").is_none());
                }
                crate::ProviderKind::Gemini => {
                    assert!(
                        received
                            .path
                            .contains("/models/gemini-model:streamGenerateContent")
                    );
                    assert_eq!(received.body["generationConfig"]["temperature"], 0.25);
                    assert_eq!(received.body["generationConfig"]["maxOutputTokens"], 456);
                }
            }
        }
    }
}

#[tokio::test]
async fn next_request_uses_current_endpoint_protocol_auth_and_key() {
    let mut first = Server::start(true).await;
    let mut second = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &first.url, "chat-completions", "bearer");
    let auth = crate::auth::AuthStore::new(directory.path().into());
    auth.set("test", "first-key").unwrap();
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let turn = gateway.open_turn();
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        first.next().await.headers["authorization"],
        "Bearer first-key"
    );
    auth.set("test", "second-key").unwrap();
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    let changed_key = first.next().await;
    assert_eq!(changed_key.headers["authorization"], "Bearer second-key");
    assert_eq!(changed_key.connection, 1);
    configure(
        directory.path(),
        &second.url,
        "anthropic-messages",
        "x-goog-api-key",
    );
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    let changed_endpoint = second.next().await;
    assert_eq!(changed_endpoint.path, "/v1/v1/messages");
    assert_eq!(changed_endpoint.headers["x-goog-api-key"], "second-key");
    assert!(!changed_endpoint.headers.contains_key("authorization"));
    configure(directory.path(), &first.url, "generate-content", "none");
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    let no_auth = first.next().await;
    assert!(no_auth.path.contains(":streamGenerateContent"));
    assert!(!no_auth.headers.contains_key("authorization"));
    assert!(!no_auth.headers.contains_key("x-goog-api-key"));
}

async fn configuration_failure(
    turn: &super::GatewayTurn,
    provider: &str,
    contains: &str,
) -> crate::LlmError {
    let error = match turn
        .stream_chat(request(provider), StreamControl::default())
        .await
    {
        Ok(_) => panic!("invalid configuration opened a stream"),
        Err(error) => error,
    };
    assert!(matches!(error, crate::LlmError::Config { .. }), "{error}");
    assert!(!error.retryable());
    assert!(error.to_string().contains("configuration error"));
    assert!(error.to_string().contains(contains), "{error}");
    error
}

#[tokio::test]
async fn invalid_configuration_only_blocks_requests_that_depend_on_it() {
    let mut server = Server::start(false).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "bearer");
    let auth = crate::auth::AuthStore::new(directory.path().into());
    auth.set("test", "test-key").unwrap();
    let gateway = RequestGateway::new(directory.path().into(), Vec::new()).unwrap();
    let turn = gateway.open_turn();
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    server.next().await;

    std::fs::write(
        directory.path().join("providers.toml"),
        "[providers.test]\nprotocol = 'broken'\n",
    )
    .unwrap();
    configuration_failure(&turn, "test", "providers.toml:2:").await;
    std::fs::write(directory.path().join("providers.toml"), "").unwrap();
    configuration_failure(&turn, "test", "not registered").await;
    configure(directory.path(), &server.url, "chat-completions", "bearer");
    auth.remove("test").unwrap();
    configuration_failure(&turn, "test", "credentials are missing").await;
    auth.set("test", "restored-key").unwrap();
    std::fs::write(directory.path().join("auth.json"), "{invalid").unwrap();
    configuration_failure(&turn, "test", "auth.json").await;
    assert!(server.received.try_recv().is_err());
    configure(directory.path(), &server.url, "chat-completions", "none");
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    let no_auth = server.next().await;
    assert!(!no_auth.headers.contains_key("authorization"));
    assert!(!no_auth.headers.contains_key("x-api-key"));
    assert!(!no_auth.headers.contains_key("x-goog-api-key"));
    std::fs::write(
        directory.path().join("providers.toml"),
        "[providers.test]\nprotocol = 'untrusted-value'\n",
    )
    .unwrap();
    let error = configuration_failure(&turn, "test", "invalid provider configuration").await;
    assert!(error.to_string().contains("providers.toml:2:"));
    assert!(error.to_string().contains("auth.json"));
    assert!(!error.to_string().contains("untrusted-value"));
    configure(directory.path(), &server.url, "chat-completions", "bearer");
    std::fs::remove_file(directory.path().join("auth.json")).unwrap();
    configuration_failure(&turn, "test", "credentials are missing").await;
    assert!(server.received.try_recv().is_err());
}

#[tokio::test]
async fn unimplemented_protocols_and_auth_are_configuration_errors() {
    let directory = private_dir();
    let gateway = RequestGateway::new(directory.path().into(), Vec::new()).unwrap();
    let turn = gateway.open_turn();
    for protocol in ["responses", "interactions"] {
        for auth in ["none", "bearer"] {
            configure(directory.path(), "http://localhost:1", protocol, auth);
            configuration_failure(
                &turn,
                "test",
                &format!("protocol {protocol} is not implemented"),
            )
            .await;
        }
    }
    configuration_failure(&turn, "chatgpt", "auth oauth:chatgpt is not implemented").await;
}

#[tokio::test]
async fn clients_are_reused_only_within_the_same_turn_provider_and_base_url() {
    let mut server = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "none");
    let mut config = std::fs::read_to_string(directory.path().join("providers.toml")).unwrap();
    config.push_str(&format!(
        "\n[providers.other]\nprotocol = 'chat-completions'\nbase_url = '{}'\nauth = 'none'\n",
        server.url
    ));
    std::fs::write(directory.path().join("providers.toml"), config).unwrap();
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let turn = gateway.open_turn();
    let mut connections = Vec::new();
    for provider in ["test", "test", "other", "test", "other"] {
        complete(
            turn.stream_chat(request(provider), StreamControl::default())
                .await
                .unwrap(),
        )
        .await;
        connections.push(server.next().await.connection);
    }
    assert_eq!(connections, [1, 1, 2, 1, 2]);
    let another_turn = gateway.open_turn();
    complete(
        another_turn
            .stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(server.next().await.connection, 3);
    configure(
        directory.path(),
        &server.url.replace("/v1", "/changed"),
        "chat-completions",
        "none",
    );
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    let changed = server.next().await;
    assert_eq!(changed.connection, 4);
    assert_eq!(changed.path, "/changed/chat/completions");
}

#[tokio::test]
async fn unfinished_transport_discards_pinned_client_before_the_next_request() {
    let mut server = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "none");
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let turn = gateway.open_turn();
    let partial = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
    let cases = [
        ("no headers", String::new(), false),
        (
            "error body interrupted",
            "HTTP/1.1 503 Unavailable\r\nContent-Length: 100\r\n\r\n{\"error\"".into(),
            false,
        ),
        (
            "error body exceeds read limit",
            format!(
                "HTTP/1.1 503 Unavailable\r\nContent-Length: 65537\r\n\r\n{}",
                "x".repeat(65_537)
            ),
            false,
        ),
        (
            "SSE interrupted",
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{partial}",
                partial.len() + 64
            ),
            false,
        ),
        (
            "EOF before Done",
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{partial}",
                partial.len()
            ),
            false,
        ),
        (
            "body interrupted after Done",
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{DONE}",
                DONE.len() + 64
            ),
            true,
        ),
        (
            "body tail timeout after Done",
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{DONE}",
                DONE.len() + 64
            ),
            true,
        ),
    ];
    for (name, response, completes) in cases {
        complete(
            turn.stream_chat(request("test"), StreamControl::default())
                .await
                .unwrap(),
        )
        .await;
        let previous = server.next().await.connection;
        server.respond(response, name != "body tail timeout after Done");
        let result = turn
            .stream_chat(request("test"), StreamControl::default())
            .await;
        match result {
            Err(error) => assert!(error.retryable(), "{name}: {error}"),
            Ok(stream) => {
                let events = stream.collect::<Vec<_>>().await;
                assert_eq!(
                    matches!(
                        events.last(),
                        Some(LlmEvent::Finished {
                            outcome: ReplyOutcome::Complete { .. },
                            ..
                        })
                    ),
                    completes,
                    "{name}: {events:?}"
                );
            }
        }
        server.next().await;
        // loopback 服务无法观察公网 DNS，直接核对地址固定所用的 client 已被丢弃。
        let slot = turn
            .clients
            .lock()
            .unwrap()
            .get(&("test".into(), server.url.clone()))
            .unwrap()
            .clone();
        assert!(!slot.is_cached(), "{name}");
        complete(
            turn.stream_chat(request("test"), StreamControl::default())
                .await
                .unwrap(),
        )
        .await;
        assert_ne!(server.next().await.connection, previous, "{name}");
        assert!(slot.is_cached(), "{name}");
    }
}

#[tokio::test]
async fn done_tail_above_64_kib_discards_the_pinned_client() {
    let mut server = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "none");
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let turn = gateway.open_turn();
    let release = server.respond_with_tail(
        format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{DONE}\r\n",
            DONE.len()
        ),
        format!("10001\r\n{}\r\n0\r\n\r\n", "x".repeat(65_537)),
    );
    let mut stream = turn
        .stream_chat(request("test"), StreamControl::default())
        .await
        .unwrap();
    assert_eq!(stream.next().await, Some(LlmEvent::Started));
    assert_eq!(
        stream.next().await,
        Some(LlmEvent::Delta(crate::LiveDelta::Text("ok".into())))
    );
    // 正文增量已交付后才释放尾部，保证尾部进入收尾读取的 64 KiB 计数。
    release.send(()).unwrap();
    complete(stream).await;
    let previous = server.next().await.connection;
    let slot = turn
        .clients
        .lock()
        .unwrap()
        .get(&("test".into(), server.url.clone()))
        .unwrap()
        .clone();
    assert!(!slot.is_cached());
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    assert_ne!(server.next().await.connection, previous);
}

#[tokio::test]
async fn dropping_an_opening_request_discards_its_pinned_client() {
    let mut server = Server::start(false).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "none");
    let gateway = RequestGateway::new(directory.path().into(), Vec::new()).unwrap();
    let turn = gateway.open_turn();
    server.respond(String::new(), false);
    {
        let opening = turn.stream_chat(request("test"), StreamControl::default());
        tokio::pin!(opening);
        tokio::select! {
            _ = server.next() => {},
            _ = &mut opening => panic!("provider has not sent response headers"),
        }
    }
    let slot = turn
        .clients
        .lock()
        .unwrap()
        .get(&("test".into(), server.url.clone()))
        .unwrap()
        .clone();
    assert!(!slot.is_cached());
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(server.next().await.connection, 2);
}

#[tokio::test]
async fn client_setup_errors_redact_credentials_in_the_hostname() {
    let directory = private_dir();
    let key = "misplaced-test-key";
    let url = format!("https://{key}.{}.invalid", "x".repeat(64));
    configure(directory.path(), &url, "chat-completions", "bearer");
    crate::auth::AuthStore::new(directory.path().into())
        .set("test", key)
        .unwrap();
    let gateway = RequestGateway::new(directory.path().into(), Vec::new()).unwrap();
    let turn = gateway.open_turn();
    let error = match tokio::time::timeout(
        Duration::from_secs(5),
        turn.stream_chat(request("test"), StreamControl::default()),
    )
    .await
    .unwrap()
    {
        Ok(_) => panic!("invalid hostname opened a stream"),
        Err(error) => error,
    };
    assert!(matches!(error, crate::LlmError::Dns { .. }));
    assert!(!error.retryable());
    assert!(!error.to_string().contains(key));
    assert!(error.to_string().contains("[REDACTED]"));
}

#[tokio::test]
async fn diagnostics_redact_all_current_credentials_before_bounding() {
    let server = Server::start(false).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "bearer");
    let auth = crate::auth::AuthStore::new(directory.path().into());
    let long_key = "other-secret-".repeat(100);
    auth.set("test", "current-key").unwrap();
    auth.set("deepseek", &long_key).unwrap();
    let gateway = RequestGateway::new(directory.path().into(), Vec::new()).unwrap();
    let turn = gateway.open_turn();
    let body = format!("current-key {long_key}");
    server.respond(format!("HTTP/1.1 401 Unauthorized\r\nx-request-id: req-current-key-123\r\nContent-Length: {}\r\n\r\n{body}", body.len()), false);
    let (result, logs) =
        capture_logs(turn.stream_chat(request("test"), StreamControl::default())).await;
    let error = match result {
        Ok(_) => panic!("HTTP error opened a stream"),
        Err(error) => error,
    };
    assert!(!error.to_string().contains("current-key"));
    assert!(!error.to_string().contains("other-secret"));
    assert!(error.to_string().contains("[REDACTED]"));
    assert!(logs.contains("provider responded"));
    assert!(logs.contains("provider returned an error status"));
    assert!(logs.contains("request_id=\"req-[REDACTED]-123\""));
    assert!(!logs.contains("current-key"));
    assert!(!logs.contains("other-secret"));
    let body = format!("data: {{\"error\":{{\"message\":\"{long_key} current-key\"}}}}\n\n");
    server.respond(
        format!(
            "HTTP/1.1 200 OK\r\nx-request-id: req-current-key-456\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
        false,
    );
    let (events, logs) = capture_logs(async {
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
    })
    .await;
    let Some(LlmEvent::Finished {
        outcome:
            ReplyOutcome::Interrupted {
                reason: crate::ReplyStop::Stream(crate::StreamStop::Provider(failure)),
            },
        ..
    }) = events.last()
    else {
        panic!("{events:?}");
    };
    assert!(!failure.message.contains("current-key"));
    assert!(!failure.message.contains("other-secret"));
    assert!(failure.message.contains("[REDACTED]"));
    assert!(failure.message.chars().count() <= crate::MAX_DIAGNOSTIC_CHARS);
    assert!(logs.contains("provider stream error"));
    assert!(logs.contains("request_id=\"req-[REDACTED]-456\""));
    assert!(!logs.contains("current-key"));
    assert!(!logs.contains("other-secret"));
    configure(directory.path(), &server.url, "chat-completions", "bearer");
    std::fs::write(
        directory.path().join("providers.toml"),
        "[providers.test]\nprotocol = 'current-key'\n",
    )
    .unwrap();
    let error = match turn
        .stream_chat(request("test"), StreamControl::default())
        .await
    {
        Ok(_) => panic!("invalid provider configuration"),
        Err(error) => error,
    };
    assert!(!error.to_string().contains("current-key"));
    assert!(error.to_string().contains("providers.toml:2:"));
}

#[tokio::test]
async fn stream_control_reaches_the_shared_reply_assembler() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let server = Server::start(false).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "none");
    let gateway = RequestGateway::new(directory.path().into(), Vec::new()).unwrap();
    let turn = gateway.open_turn();
    let cancel = Arc::new(AtomicBool::new(false));
    let stream = turn
        .stream_chat(
            request("test"),
            StreamControl {
                cancel: Some(cancel.clone()),
                deadline: None,
            },
        )
        .await
        .unwrap();
    cancel.store(true, Ordering::Release);
    let events = stream.collect::<Vec<_>>().await;
    assert!(matches!(
        events.as_slice(),
        [
            LlmEvent::Started,
            LlmEvent::Finished {
                outcome: ReplyOutcome::Interrupted {
                    reason: crate::ReplyStop::Stream(crate::StreamStop::Cancelled)
                },
                ..
            }
        ]
    ));
    let slot = turn
        .clients
        .lock()
        .unwrap()
        .get(&("test".into(), server.url.clone()))
        .unwrap()
        .clone();
    assert!(!slot.is_cached());
}
