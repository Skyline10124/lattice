// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::StreamExt;
use std::os::unix::fs::PermissionsExt;
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

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // 临时目录权限取决于 umask，这里固定为 0700，避免结果随环境变化。
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

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
    address: std::net::SocketAddr,
    peers: mpsc::UnboundedReceiver<std::net::SocketAddr>,
    task: JoinHandle<()>,
    responses: Arc<Mutex<VecDeque<Response>>>,
}

impl Server {
    async fn start(tls: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, received) = mpsc::unbounded_channel();
        let (peers_tx, peers) = mpsc::unbounded_channel();
        let responses = Arc::new(Mutex::new(VecDeque::new()));
        let queued = responses.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            let mut id = 0;
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, peer) = accepted.unwrap();
                        let _ = peers_tx.send(peer);
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
            address,
            peers,
            task,
            responses,
        }
    }

    /// 被测请求返回后再连一次探针：内核按到达顺序交出已完成握手的连接，
    /// 所以探针之前被接受的连接数就是被测代码对这个服务器发起的连接数。
    ///
    /// 前提：只统计被测代码在探针之前已完成（connect 已 await 返回）的连接。
    /// 被测代码若 spawn 一个不等待的连接，它可能在探针之后才连上而被漏计，计数偏少；
    /// 探针之前已完成的连接仍会计入。所以被测代码不能这样发起连接。
    async fn connections_before_probe(&mut self) -> usize {
        let probe = tokio::net::TcpStream::connect(self.address).await.unwrap();
        let probe_addr = probe.local_addr().unwrap();
        let mut before = 0;
        loop {
            let peer = tokio::time::timeout(Duration::from_secs(5), self.peers.recv())
                .await
                .expect("probe connection was accepted")
                .expect("server is still running");
            if peer == probe_addr {
                return before;
            }
            before += 1;
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
        let length = headers
            .get("content-length")
            .map_or(0, |length| length.parse().unwrap());
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        let body = if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        };
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
            provider: provider.into(),
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
            let cell = format!("{protocol}/{auth}");
            let received = server.next().await;
            assert!(
                received.body.get("temperature").is_none(),
                "{cell}: temperature"
            );
            assert!(
                received.body.get("max_completion_tokens").is_none(),
                "{cell}: max_completion_tokens"
            );
            if protocol != "anthropic-messages" {
                assert!(
                    received.body.get("max_tokens").is_none(),
                    "{cell}: max_tokens"
                );
                assert!(
                    received.body.get("generationConfig").is_none(),
                    "{cell}: generationConfig"
                );
            }
            let expected_path = match protocol {
                "chat-completions" => "/v1/chat/completions",
                "anthropic-messages" => "/v1/v1/messages",
                _ => "/v1/models/family%2Fmodel%20id:streamGenerateContent?alt=sse",
            };
            assert_eq!(received.path, expected_path, "{cell}: path");
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
                    "{cell}: {header}"
                );
            }
            assert_eq!(
                received
                    .headers
                    .get("anthropic-version")
                    .map(String::as_str),
                (protocol == "anthropic-messages").then_some("2023-06-01"),
                "{cell}: anthropic-version"
            );
            assert_eq!(
                received.headers["accept"], "text/event-stream, application/json",
                "{cell}: accept"
            );
            assert!(
                received.headers["user-agent"].starts_with("lattice/"),
                "{cell}: user-agent"
            );
            assert_eq!(
                received.headers["x-client-request-id"], "session-id",
                "{cell}: client request id"
            );
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
                    "{cell}: unexpected header {header}"
                );
            }
            match protocol {
                "chat-completions" => {
                    assert_eq!(received.body["model"], "family/model id", "{cell}: model");
                    assert_eq!(
                        received.body["messages"][0]["content"], "hello",
                        "{cell}: message"
                    );
                    assert_eq!(
                        received.body["tools"][0]["function"]["name"], "tool_name",
                        "{cell}: tool"
                    );
                    assert_eq!(received.body["stream"], true, "{cell}: stream");
                    assert_eq!(
                        received.body["stream_options"]["include_usage"], true,
                        "{cell}: usage"
                    );
                }
                "anthropic-messages" => {
                    assert_eq!(received.body["model"], "family/model id", "{cell}: model");
                    assert_eq!(
                        received.body["messages"][0]["content"][0]["text"], "hello",
                        "{cell}: message"
                    );
                    assert_eq!(
                        received.body["tools"][0]["name"], "tool_name",
                        "{cell}: tool"
                    );
                    assert_eq!(received.body["max_tokens"], 32_768, "{cell}: max_tokens");
                    assert_eq!(received.body["stream"], true, "{cell}: stream");
                }
                _ => {
                    assert_eq!(
                        received.body["contents"][0]["parts"][0]["text"], "hello",
                        "{cell}: message"
                    );
                    assert_eq!(
                        received.body["tools"][0]["functionDeclarations"][0]["name"], "tool_name",
                        "{cell}: tool"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn resolved_request_keeps_its_protocol_endpoint_and_credentials_until_sent() {
    let mut first = Server::start(true).await;
    let mut second = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &first.url, "chat-completions", "bearer");
    let auth = crate::auth::AuthStore::new(directory.path().into());
    auth.set("test", "first-key").unwrap();
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let turn = gateway.open_turn();
    let request = request("test");
    let resolved = turn.resolve(&request.target).await.unwrap();
    assert_eq!(
        resolved.protocol(),
        crate::protocol::Protocol::ChatCompletions
    );
    configure(
        directory.path(),
        &second.url,
        "generate-content",
        "x-goog-api-key",
    );
    auth.set("test", "second-key").unwrap();
    complete(
        resolved
            .stream_chat(request.clone(), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    let received = first.next().await;
    assert_eq!(received.path, "/v1/chat/completions");
    assert_eq!(received.headers["authorization"], "Bearer first-key");
    let resolved = turn.resolve(&request.target).await.unwrap();
    assert_eq!(
        resolved.protocol(),
        crate::protocol::Protocol::GenerateContent
    );
    complete(
        resolved
            .stream_chat(request, StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    let received = second.next().await;
    assert!(received.path.contains(":streamGenerateContent"));
    assert_eq!(received.headers["x-goog-api-key"], "second-key");
}

#[tokio::test]
async fn chat_replay_defaults_to_no_reasoning() {
    use lattice_conversation::{ConversationItem, Protocol, StepContent};
    let mut server = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "none");
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let mut request = request("test");
    let assistant = crate::test_reply::wire_to_native(
        Protocol::ChatCompletions,
        &serde_json::json!({"choices":[{"message":{
            "content":"answer", "reasoning_content":"private reasoning"
        }}]}),
    )
    .unwrap()
    .bind_zero_tool()
    .unwrap();
    request.messages.push(ConversationItem::Step(StepContent {
        assistant,
        results: vec![],
    }));
    complete(
        gateway
            .open_turn()
            .stream_chat(request, StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        server.next().await.body["messages"][0],
        serde_json::json!({"role":"assistant", "content":"answer"})
    );

    let base = std::fs::read_to_string(directory.path().join("providers.toml")).unwrap();
    for (provider, model, expected) in [
        (
            "reasoning-content",
            None,
            serde_json::json!({"role":"assistant", "content":[{"type":"text","text":"answer","cache_control":{"type":"ephemeral"}}], "reasoning_content":"private reasoning"}),
        ),
        (
            "content-chunks",
            None,
            serde_json::json!({"role":"assistant", "content":[{"type":"thinking", "thinking":[{"type":"text", "text":"private reasoning"}]}, {"type":"text", "text":"answer","cache_control":{"type":"ephemeral"}}]}),
        ),
        (
            "reasoning-content",
            Some("none"),
            serde_json::json!({"role":"assistant", "content":"answer"}),
        ),
        (
            "none",
            Some("reasoning-content"),
            serde_json::json!({"role":"assistant", "content":[{"type":"text","text":"answer","cache_control":{"type":"ephemeral"}}], "reasoning_content":"private reasoning"}),
        ),
    ] {
        let overlay = model.map_or(String::new(), |value| {
            format!("[providers.test.models.model-id.features]\nreasoning_replay = '{value}'\ncache_markers = {}\n", value != "none")
        });
        std::fs::write(
            directory.path().join("providers.toml"),
            format!("{base}[providers.test.features]\nreasoning_replay = '{provider}'\ncache_markers = {}\n{overlay}", provider != "none"),
        )
        .unwrap();
        let mut request = self::request("test");
        request.messages.push(ConversationItem::Step(StepContent {
            assistant: crate::test_reply::wire_to_native(Protocol::ChatCompletions,
                &serde_json::json!({"choices":[{"message":{"content":"answer", "reasoning_content":"private reasoning"}}]})).unwrap().bind_zero_tool().unwrap(),
            results: vec![],
        }));
        let resolved = gateway.open_turn().resolve(&request.target).await.unwrap();
        std::fs::write(directory.path().join("providers.toml"), &base).unwrap();
        complete(
            resolved
                .stream_chat(request, StreamControl::default())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(server.next().await.body["messages"][0], expected);
    }
}

#[tokio::test]
async fn replay_source_matrix_reaches_each_protocol_request_body() {
    use crate::protocol::projection::{ProjectionError, wire_to_native};
    use lattice_conversation::{ConversationItem, Protocol, ReplayOrigin, StepContent};

    // 期望来自夹具里的原文，不经过 project_cross_provider / native_to_wire 再算一遍。
    let cases = [
        (
            Protocol::ChatCompletions,
            include_str!("../../tests/fixtures/projection/openai_assistant_tool_calls.json"),
            &["call_9f8e7d6c5b", "call_1a2b3c4d5e"][..],
            &["The user wants the weather; call get_weather first, then get_time."][..],
        ),
        (
            Protocol::Messages,
            include_str!("../../tests/fixtures/projection/anthropic_thinking_tool_use.json"),
            &["toolu_01AbCdEfGh", "toolu_02BcDeFgHi"][..],
            &[
                "Need the weather first, then the time; both are independent calls.",
                "ErUBCkYIAxgCIkD3c2FmZGVmNTQ2Ny0xMjM0LWFiY2QtZXhhbXBsZXNpZ25hdHVyZQ==",
            ][..],
        ),
        (
            Protocol::GenerateContent,
            include_str!("../../tests/fixtures/projection/gemini_thought_signature.json"),
            &["fc-2026-08-13-001"][..],
            &[
                "Plan: check weather, then time.",
                "CigIARABGkh4dGh5YnJpZC1zaWduYXR1cmUtb24tdGhvdWdodA==",
                "CigIARABGkh4ZW1wdHktdGV4dC1wYXJ0LXNpZ25hdHVyZQ==",
                "CigIARABGkh4c2lnbmF0dXJlLW9uLWZ1bmN0aW9uLWNhbGw=",
            ][..],
        ),
    ];
    let protocols = [
        Protocol::ChatCompletions,
        Protocol::Messages,
        Protocol::GenerateContent,
    ];
    let mut server = Server::start(true).await;
    let directory = private_dir();
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    for (protocol, fixture, call_ids, replay_material) in cases {
        let origin = ReplayOrigin {
            protocol,
            provider_id: "test".into(),
        };
        let decoded = wire_to_native(&origin, &serde_json::from_str(fixture).unwrap()).unwrap();
        let (assistant, results) = crate::test_reply::bind_replay_step(decoded);
        for target_protocol in protocols {
            // 第三格在写过 other 的配置后回到同一 provider，标签带序号以免重名。
            for (round, provider) in ["test", "other", "test"].into_iter().enumerate() {
                let cell = format!("{protocol} -> {target_protocol}/{provider}#{round}");
                let same_provider = target_protocol == protocol && provider == "test";
                let same_protocol = target_protocol == protocol;
                // Chat Completions 与 Messages 没有 call ID 就无法编码工具结果。
                // generateContent 按工具名关联，请求仍会发出，但不得带上来源 call ID。
                let sends = same_protocol || target_protocol == Protocol::GenerateContent;
                let feature = if target_protocol == Protocol::ChatCompletions {
                    format!(
                        "[providers.{provider}.features]\nreasoning_replay='reasoning-content'\n"
                    )
                } else {
                    String::new()
                };
                std::fs::write(directory.path().join("providers.toml"), format!(
                    "[providers.{provider}]\nprotocol='{target_protocol}'\nbase_url='{}'\nauth='none'\n{feature}", server.url)).unwrap();
                if sends {
                    let body = match target_protocol {
                        Protocol::ChatCompletions => DONE,
                        Protocol::Messages => MESSAGES_DONE,
                        Protocol::GenerateContent => CONTENT_DONE,
                        _ => unreachable!(),
                    };
                    server.respond(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}", body.len()), false);
                }
                let mut request = self::request(provider);
                request.messages.push(ConversationItem::Step(StepContent {
                    assistant: assistant.clone(),
                    results: results.clone(),
                }));
                match gateway
                    .open_turn()
                    .stream_chat(request, StreamControl::default())
                    .await
                {
                    Ok(stream) => {
                        assert!(
                            sends,
                            "{cell}: sent a request that should have been rejected"
                        );
                        complete(stream).await;
                        let serialized = server.next().await.body.to_string();
                        for id in call_ids {
                            assert_eq!(
                                serialized.contains(id),
                                same_protocol,
                                "{cell}: provider call id {id}"
                            );
                        }
                        for material in replay_material {
                            assert_eq!(
                                serialized.contains(material),
                                same_provider,
                                "{cell}: replay material"
                            );
                        }
                    }
                    Err(error) => {
                        assert!(!sends, "{cell}: {error}");
                        assert!(
                            matches!(
                                error,
                                crate::LlmError::Projection(
                                    ProjectionError::ToolResultWithoutProviderCallId { .. }
                                )
                            ),
                            "{cell}: {error}"
                        );
                        assert!(
                            server.received.try_recv().is_err(),
                            "{cell}: rejected projection still reached the server"
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn messages_use_merged_output_limits_at_resolution_and_reload_on_the_next_request() {
    use lattice_conversation::{
        ConversationItem, MessagePart, MessageRole, MessageSemantic, NativeMessage,
    };
    let mut server = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "anthropic-messages", "none");
    let config = directory.path().join("providers.toml");
    let base = std::fs::read_to_string(&config).unwrap();
    std::fs::create_dir(directory.path().join("cache")).unwrap();
    let cache = directory.path().join("cache/models.json");
    let write_cache = |output| {
        std::fs::write(&cache, serde_json::to_vec(&serde_json::json!({"custom":{"models":{"model-id":{"id":"model-id","limit":{"output":output}}}}})).unwrap()).unwrap();
    };
    std::fs::write(&config, format!("{base}catalog = 'custom'\n")).unwrap();
    write_cache(8192);
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let turn = gateway.open_turn();
    let mut request = request("test");
    request
        .messages
        .push(ConversationItem::Message(NativeMessage::new(
            MessageRole::System,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: "system".into(),
            })],
        )));
    let old = turn.resolve(&request.target).await.unwrap();
    write_cache(65536);
    complete(
        old.stream_chat(request.clone(), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(server.next().await.body["max_tokens"], 8192);
    for (handwritten, provider_cache, model_cache, expected, cache_enabled) in [
        (None, "automatic", None, 65536_u64, true),
        (Some(4096), "automatic", Some("none"), 4096, false),
        (Some(4096), "none", Some("automatic"), 4096, true),
        (None, "none", None, 65536, false),
    ] {
        let overlay = handwritten.map_or(String::new(), |value| {
            format!("[providers.test.models.model-id]\noutput = {value}\n")
        });
        let model_features = model_cache.map_or(String::new(), |value| {
            format!("[providers.test.models.model-id.features]\ncache = '{value}'\n")
        });
        std::fs::write(&config, format!("{base}catalog = 'custom'\n[providers.test.features]\ncache = '{provider_cache}'\n{overlay}{model_features}")).unwrap();
        complete(
            turn.stream_chat(request.clone(), StreamControl::default())
                .await
                .unwrap(),
        )
        .await;
        let body = server.next().await.body;
        assert_eq!(body["max_tokens"], expected);
        assert_eq!(
            crate::test_reply::cache_controls(&body),
            if cache_enabled {
                vec![
                    (
                        "/cache_control".into(),
                        serde_json::json!({"type":"ephemeral"}),
                    ),
                    (
                        "/system/0/cache_control".into(),
                        serde_json::json!({"type":"ephemeral"}),
                    ),
                ]
            } else {
                vec![]
            }
        );
        assert_eq!(
            body["system"],
            if cache_enabled {
                serde_json::json!([{"type":"text","text":"system","cache_control":{"type":"ephemeral"}}])
            } else {
                serde_json::json!("system")
            }
        );
        assert_eq!(
            crate::model_candidates(directory.path(), Some("test"))
                .unwrap()
                .models[0]
                .output,
            Some(expected)
        );
    }
    std::fs::write(&config, &base).unwrap();
    complete(
        turn.stream_chat(request, StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(server.next().await.body["max_tokens"], 32768);
}

/// 以给定的模型限制发送一次 Messages 请求，返回请求体里的 `max_tokens`。
/// 网关在发送前拒绝时返回错误，并确认 provider 没有收到请求。
async fn messages_max_tokens(
    limit: serde_json::Value,
    text: Option<&str>,
) -> Result<u64, crate::LlmError> {
    let mut server = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "anthropic-messages", "none");
    let config = directory.path().join("providers.toml");
    let base = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("{base}catalog = 'custom'\n")).unwrap();
    std::fs::create_dir(directory.path().join("cache")).unwrap();
    std::fs::write(
        directory.path().join("cache/models.json"),
        serde_json::to_vec(
            &serde_json::json!({"custom":{"models":{"model-id":{"id":"model-id","limit":limit}}}}),
        )
        .unwrap(),
    )
    .unwrap();
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let mut request = request("test");
    if let Some(text) = text {
        request.messages.push(user_message(text));
    }
    match gateway
        .open_turn()
        .stream_chat(request, StreamControl::default())
        .await
    {
        Ok(stream) => {
            complete(stream).await;
            Ok(server.next().await.body["max_tokens"].as_u64().unwrap())
        }
        Err(error) => {
            assert!(
                matches!(
                    server.received.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ),
                "a request rejected before sending must not reach the provider: the mock server must have seen zero requests"
            );
            Err(error)
        }
    }
}

#[tokio::test]
async fn messages_zero_output_limit_without_a_window_sends_the_default() {
    let max_tokens = messages_max_tokens(serde_json::json!({"output":0}), Some("hi"))
        .await
        .unwrap();
    assert_eq!(
        max_tokens, 32_768,
        "output limit 0 must fall back to 32,768"
    );
}

#[tokio::test]
async fn messages_output_limit_that_fits_the_window_is_sent_unchanged() {
    let max_tokens = messages_max_tokens(
        serde_json::json!({"context":200_000,"output":64_000}),
        Some("hi"),
    )
    .await
    .unwrap();
    assert_eq!(
        max_tokens, 64_000,
        "a short prompt must keep the full output limit"
    );
}

#[tokio::test]
async fn messages_max_tokens_is_clamped_into_the_remaining_window() {
    // 420_000 字节英文正文，加上 Messages 用户消息里的 "user" 与 "text"，
    // 字符串共 420_008 字节（与下面中文用例的 450_008 字节不同）。
    // 按 3 字节一个 token 向上取整是 140_003；上下文 200_000，输出上限 64_000，
    // 能放下的输出是 59_997。
    let max_tokens = messages_max_tokens(
        serde_json::json!({"context":200_000,"output":64_000}),
        Some(&"a".repeat(420_000)),
    )
    .await
    .unwrap();
    assert_eq!(
        max_tokens, 59_997,
        "max_tokens must leave the estimated prompt inside the context window"
    );
}

#[tokio::test]
async fn messages_max_tokens_leaves_room_for_a_chinese_prompt() {
    // 150_000 个汉字，每字 3 字节、约 1 个 token。加上 "user" 与 "text"
    // 共 450_008 字节，估算 150_003 token，夹到 49_997。按 4 字节一个 token
    // 只估 112_502，会发送 64_000，与实际约 150_000 的提示相加超出窗口。
    let max_tokens = messages_max_tokens(
        serde_json::json!({"context":200_000,"output":64_000}),
        Some(&"中".repeat(150_000)),
    )
    .await
    .unwrap();
    assert_eq!(
        max_tokens, 49_997,
        "Chinese prompt must be clamped by the 3 bytes/token estimate"
    );
}

#[tokio::test]
async fn messages_english_prompt_between_the_estimates_is_sent_with_the_floor() {
    // 640_000 字节英文加 "user"、"text" 共 640_008 字节，约 160_000 个实际 token。
    // 按 3 字节估 213_336，超过 200_000 的窗口；按 4 字节估 160_002，还剩 39_998。
    // 这种提示要发送，max_tokens 取下限 1_024，由对端判断，不在发送前拒绝。
    let max_tokens = messages_max_tokens(
        serde_json::json!({"context":200_000,"output":64_000}),
        Some(&"a".repeat(640_000)),
    )
    .await
    .expect("a prompt that fits at 4 bytes/token must be sent, not rejected");
    assert_eq!(
        max_tokens, 1_024,
        "between the estimates max_tokens must be the 1,024 floor"
    );
}

#[tokio::test]
async fn messages_output_floor_is_capped_by_the_model_output_limit() {
    // 与上一个用例同样的 640_008 字节英文、200_000 窗口，但模型输出上限只有 500：
    // 下限 1_024 也不能超过它，发送 500。
    let max_tokens = messages_max_tokens(
        serde_json::json!({"context":200_000,"output":500}),
        Some(&"a".repeat(640_000)),
    )
    .await
    .unwrap();
    assert_eq!(
        max_tokens, 500,
        "the floor must not exceed the 500-token model output limit"
    );
}

#[tokio::test]
async fn messages_one_token_of_conservative_room_is_sent_without_the_floor() {
    // 289 字节正文加 "user"、"text" 共 297 字节：按 3 字节估 99，窗口 100 剩 1；
    // 按 4 字节估 75，剩 25。保守估算还剩 1 就发 1，不改用下限（那会是 25）。
    let max_tokens = messages_max_tokens(
        serde_json::json!({"context":100,"output":64_000}),
        Some(&"a".repeat(289)),
    )
    .await
    .unwrap();
    assert_eq!(
        max_tokens, 1,
        "one token of conservative room must be sent as 1, not raised to the floor"
    );
}

#[tokio::test]
async fn messages_output_floor_is_capped_to_a_lower_room_below_the_floor() {
    // 7_192 字节正文加 "user"、"text" 共 7_200 字节：按 3 字节估 2_400，正好占满
    // 2_400 的窗口；按 4 字节估 1_800，剩 600。600 小于 1_024，下限夹到 600。
    let max_tokens = messages_max_tokens(
        serde_json::json!({"context":2_400,"output":64_000}),
        Some(&"a".repeat(7_192)),
    )
    .await
    .unwrap();
    assert_eq!(
        max_tokens, 600,
        "the floor must be capped to the 600 tokens left by the 4 bytes/token estimate"
    );
}

#[tokio::test]
async fn messages_chinese_prompt_keeps_a_conservative_room_below_the_floor() {
    // 1_000 个汉字共 3_000 字节，加 "user"、"text" 共 3_008 字节：按 3 字节估 1_003，
    // 窗口 1_503 剩 500；按 4 字节估 752，剩 751。保守估算还有空间，就发 500，
    // 不抬到下限：中文的保守估算基本准确，发 751 或 1_024 会超窗。
    let max_tokens = messages_max_tokens(
        serde_json::json!({"context":1_503,"output":64_000}),
        Some(&"中".repeat(1_000)),
    )
    .await
    .unwrap();
    assert_eq!(
        max_tokens, 500,
        "a positive conservative room below the floor must be sent unchanged"
    );
}

#[tokio::test]
async fn messages_prompt_at_the_lower_estimate_boundary_is_sent() {
    // 388 字节正文加 "user"、"text" 共 396 字节：按 4 字节估 99，窗口 100 还剩 1，
    // 照常发送；按 3 字节估 132 已无空间，下限夹到 1。
    let max_tokens = messages_max_tokens(
        serde_json::json!({"context":100,"output":64_000}),
        Some(&"a".repeat(388)),
    )
    .await
    .expect("a prompt that leaves one token at 4 bytes/token must be sent");
    assert_eq!(
        max_tokens, 1,
        "boundary prompt must get the single remaining token"
    );
}

#[tokio::test]
async fn messages_one_byte_past_the_lower_estimate_boundary_is_rejected() {
    // 比上一个用例多 1 字节，共 397 字节：按 4 字节估 100，占满 100 的窗口，发送前拒绝。
    // messages_max_tokens 断言被拒绝时 provider 没有收到请求。
    let error = messages_max_tokens(
        serde_json::json!({"context":100,"output":64_000}),
        Some(&"a".repeat(389)),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.kind(),
        "context_window_exceeded",
        "one byte past the boundary must fail as context_window_exceeded, got {error:?}"
    );
    assert!(
        !error.retryable(),
        "context window exceeded must not be retried"
    );
    assert!(
        error.to_string().contains("at least 100 tokens")
            && error.to_string().contains("100-token"),
        "diagnostic must name the 100-token lower estimate and the 100-token window, got {error}"
    );
}

fn user_message(text: &str) -> lattice_conversation::ConversationItem {
    use lattice_conversation::{
        BoundToolCall, ConversationItem, MessagePart, MessageRole, MessageSemantic, NativeMessage,
    };
    ConversationItem::Message(NativeMessage::<BoundToolCall>::new(
        MessageRole::User,
        None,
        vec![MessagePart::semantic_only(MessageSemantic::Text {
            text: text.to_owned(),
        })],
    ))
}

#[tokio::test]
async fn metadata_refresh_with_no_roots_fails_before_network_or_directory_creation() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".lattice");
    let gateway = RequestGateway::new(directory.clone(), Vec::new())
        .unwrap()
        .with_metadata_url(format!(
            "https://{}/api.json",
            listener.local_addr().unwrap()
        ));
    assert!(matches!(
        gateway
            .refresh_metadata(Arc::new(crate::MetadataRefreshControl::default()))
            .await,
        Err(crate::MetadataError::Configuration(_))
    ));
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    assert!(!directory.exists());
}

#[tokio::test]
async fn refresh_metadata_uses_startup_roots_and_updates_candidates_and_messages() {
    let mut source = Server::start(true).await;
    let mut messages = Server::start(true).await;
    let directory = private_dir();
    configure(
        directory.path(),
        &messages.url,
        "anthropic-messages",
        "none",
    );
    let config = directory.path().join("providers.toml");
    let provider = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        config,
        format!("[metadata]\nauto_refresh = false\n{provider}catalog = 'refresh-entry'\n"),
    )
    .unwrap();
    let before = crate::model_candidates(directory.path(), Some("test")).unwrap();
    assert!(before.models.is_empty());
    let metadata = serde_json::json!({"refresh-entry":{"models":{"model-id":{
        "id":"model-id","limit":{"output":65536}
    }}}});
    let body = serde_json::to_string(&metadata).unwrap();
    source.respond(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
        true,
    );
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()])
        .unwrap()
        .with_metadata_url(format!("{}/api.json", source.url));
    gateway
        .refresh_metadata(Arc::new(crate::MetadataRefreshControl::default()))
        .await
        .unwrap();
    assert_eq!(source.next().await.path, "/v1/api.json");
    let cached: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("cache/models.json")).unwrap())
            .unwrap();
    assert_eq!(cached, metadata);
    let after = crate::model_candidates(directory.path(), Some("test")).unwrap();
    assert_eq!(after.models.len(), 1);
    assert_eq!(after.models[0].model, "model-id");
    assert_eq!(after.models[0].output, Some(65536));
    complete(
        gateway
            .open_turn()
            .stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(messages.next().await.body["max_tokens"], 65536);
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
    stage: &str,
) -> crate::LlmError {
    let error = match turn.resolve(&request(provider).target).await {
        Ok(_) => panic!("{stage}: invalid configuration resolved a request"),
        Err(error) => error,
    };
    assert!(
        matches!(error, crate::LlmError::Config { .. }),
        "{stage}: {error}"
    );
    assert!(!error.retryable(), "{stage}: {error}");
    assert!(
        error.to_string().contains("configuration error"),
        "{stage}: {error}"
    );
    assert!(error.to_string().contains(contains), "{stage}: {error}");
    error
}

/// 以测试 CA 为模板生成彼此不同的真实根：把主题与颁发者中的 "LATTICE test CA"
/// 换成等长的 "LATTICE NNNN CA"。DER 结构不变，仍能通过 rustls 信任锚解析；
/// 主题不同，所以不能为测试服务器的证书链作信任锚。
fn renamed_test_ca(index: u16) -> Vec<u8> {
    const NAME: &[u8] = b"LATTICE test CA";
    let renamed = format!("LATTICE {index:04} CA");
    assert_eq!(
        renamed.len(),
        NAME.len(),
        "renamed subject must keep the DER length"
    );
    let mut der = TLS.ca_der.clone();
    let mut replaced = 0;
    let mut at = 0;
    while let Some(offset) = der[at..].windows(NAME.len()).position(|w| w == NAME) {
        let start = at + offset;
        der[start..start + NAME.len()].copy_from_slice(renamed.as_bytes());
        at = start + NAME.len();
        replaced += 1;
    }
    assert_eq!(
        replaced, 2,
        "test CA subject and issuer must both be renamed"
    );
    der
}

#[tokio::test]
async fn invalid_tls_root_near_the_count_limit_is_skipped_without_using_a_slot() {
    // 1_023 个彼此不同的有效根之后是一张无法解析的根，再接真正的测试 CA 与另一张有效根。
    // 无效根不占名额时保留 1_023 个改名根与测试 CA（按原顺序），截掉最后一张；
    // 它若占名额，测试 CA 会被截掉，HTTPS 请求无法建立。改名根的主题不同，
    // 不能替测试 CA 完成验证，所以请求成功说明恰好保留了测试 CA。
    let mut server = Server::start(true).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "none");
    let mut roots: Vec<Vec<u8>> = (0..1_023).map(renamed_test_ca).collect();
    roots.push(vec![1, 2, 3]);
    roots.push(TLS.ca_der.clone());
    roots.push(renamed_test_ca(1_023));
    let mut expected = roots[..1_023].to_vec();
    expected.push(TLS.ca_der.clone());
    let gateway = RequestGateway::new(directory.path().into(), roots).unwrap();
    let kept = gateway.tls_roots_der();
    assert_eq!(kept.len(), 1_024, "kept root count");
    assert!(
        kept == expected.as_slice(),
        "kept roots must be the 1_023 renamed roots then the test CA, in original order; first mismatch at {:?}",
        kept.iter().zip(&expected).position(|(a, b)| a != b)
    );
    let stream = gateway
        .open_turn()
        .stream_chat(request("test"), StreamControl::default())
        .await
        .expect("the test CA kept after the invalid root must verify the TLS server");
    complete(stream).await;
    server.next().await;
}

/// 没有可用根证书时，无法解析的端点不能被当成 http 放行（失败即关闭）。provider 载入已用
/// `validate_base_url_static` 保证地址能解析，经 `providers.toml` 构造不出这种地址，所以直接检查
/// `resolve` 在建立客户端之前调用的守卫：它返回错误时 `resolve` 不会构造客户端，也不会发出请求。
#[test]
fn without_usable_tls_roots_an_unparseable_endpoint_is_a_configuration_error() {
    for base_url in [
        "",
        "not a url",
        "https://[::1",
        "http://exa mple.invalid",
        "//localhost/v1",
    ] {
        let error = super::require_endpoint_without_tls_roots(base_url).expect_err(&format!(
            "unparseable endpoint {base_url:?} must be rejected"
        ));
        assert!(
            matches!(error, crate::LlmError::Config { .. }),
            "{base_url:?} is a configuration error: {error}"
        );
        assert!(!error.retryable(), "{base_url:?} is not retryable");
        assert!(
            error.to_string().contains("invalid provider endpoint URL"),
            "{base_url:?} says the endpoint URL is invalid: {error}"
        );
    }
    let other_scheme = super::require_endpoint_without_tls_roots("ftp://localhost/v1")
        .expect_err("a non-http scheme needs TLS roots too");
    assert!(
        other_scheme
            .to_string()
            .contains("no usable TLS root certificates"),
        "non-http scheme names the missing roots: {other_scheme}"
    );
    super::require_endpoint_without_tls_roots("http://localhost:1/v1")
        .expect("a parseable http endpoint is still allowed");
}

#[tokio::test]
async fn without_usable_tls_roots_https_is_a_configuration_error_and_http_is_served() {
    // 选根经过真实的 parse_root：超过 64 KiB 的根先被跳过，无法解析的根被丢弃。
    let mut oversized = TLS.ca_der.clone();
    oversized.resize(65_537, 0);
    let cases = [
        (
            "all roots larger than 64 KiB",
            vec![oversized.clone(), oversized],
        ),
        (
            "all roots invalid",
            vec![vec![1, 2, 3], b"not a certificate".to_vec()],
        ),
        ("empty root set", Vec::new()),
    ];
    for (case, roots) in cases {
        let mut https = Server::start(true).await;
        let directory = private_dir();
        configure(directory.path(), &https.url, "chat-completions", "none");
        let gateway = RequestGateway::new(directory.path().into(), roots)
            .unwrap_or_else(|error| panic!("{case}: gateway must still be constructed: {error}"));
        assert!(
            !gateway.has_usable_tls_roots(),
            "{case}: gateway must record that no TLS roots are usable"
        );
        assert!(gateway.tls_roots_der().is_empty(), "{case}: no roots kept");
        let turn = gateway.open_turn();
        configuration_failure(&turn, "test", "no usable TLS root certificates", case).await;
        let error = match turn
            .stream_chat(request("test"), StreamControl::default())
            .await
        {
            Ok(_) => panic!("{case}: https endpoint without usable roots was dispatched"),
            Err(error) => error,
        };
        assert!(
            matches!(error, crate::LlmError::Config { .. }),
            "{case}: https without roots must be a configuration error: {error}"
        );
        assert!(
            !error.retryable(),
            "{case}: configuration error must not be retried"
        );
        assert!(
            matches!(
                https.received.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "{case}: the https mock provider must have received zero requests"
        );
        assert_eq!(
            https.connections_before_probe().await,
            0,
            "{case}: the https mock provider must have accepted zero TCP connections"
        );

        let mut http = Server::start(false).await;
        configure(directory.path(), &http.url, "chat-completions", "none");
        complete(
            turn.stream_chat(request("test"), StreamControl::default())
                .await
                .unwrap_or_else(|error| panic!("{case}: http endpoint must be served: {error}")),
        )
        .await;
        assert_eq!(
            http.next().await.path,
            "/v1/chat/completions",
            "{case}: http request reached the provider"
        );
    }
}

#[tokio::test]
async fn oversized_tls_roots_still_serve_plain_http() {
    let mut server = Server::start(false).await;
    let directory = private_dir();
    configure(directory.path(), &server.url, "chat-completions", "none");
    let mut roots = vec![vec![0_u8; 65_537]];
    for _ in 0..1025 {
        roots.push(TLS.ca_der.clone());
    }
    let gateway = RequestGateway::new(directory.path().into(), roots).unwrap();
    assert_eq!(
        gateway.tls_roots_der().len(),
        1024,
        "an oversized root set must be truncated to 1024 certificates"
    );
    assert!(
        gateway
            .tls_roots_der()
            .iter()
            .all(|der| der.len() <= 65_536),
        "a root larger than 65536 bytes must not be loaded"
    );
    complete(
        gateway
            .open_turn()
            .stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(server.next().await.path, "/v1/chat/completions");
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
    configuration_failure(&turn, "test", "providers.toml:2:", "broken protocol").await;
    std::fs::write(directory.path().join("providers.toml"), "").unwrap();
    configuration_failure(&turn, "test", "not registered", "empty registry").await;
    configure(directory.path(), &server.url, "chat-completions", "bearer");
    auth.remove("test").unwrap();
    configuration_failure(&turn, "test", "credentials are missing", "removed key").await;
    auth.set("test", "restored-key").unwrap();
    std::fs::write(directory.path().join("auth.json"), "{invalid").unwrap();
    configuration_failure(&turn, "test", "auth.json", "malformed auth file").await;
    assert!(
        server.received.try_recv().is_err(),
        "malformed auth file: request reached the server"
    );
    configure(directory.path(), &server.url, "chat-completions", "none");
    complete(
        turn.stream_chat(request("test"), StreamControl::default())
            .await
            .unwrap(),
    )
    .await;
    let no_auth = server.next().await;
    assert!(
        !no_auth.headers.contains_key("authorization"),
        "restored none auth: authorization"
    );
    assert!(
        !no_auth.headers.contains_key("x-api-key"),
        "restored none auth: x-api-key"
    );
    assert!(
        !no_auth.headers.contains_key("x-goog-api-key"),
        "restored none auth: x-goog-api-key"
    );
    std::fs::write(
        directory.path().join("providers.toml"),
        "[providers.test]\nprotocol = 'untrusted-value'\n",
    )
    .unwrap();
    let error = configuration_failure(
        &turn,
        "test",
        "providers.toml:2:",
        "untrusted protocol value",
    )
    .await;
    assert!(
        error.to_string().contains("providers.toml:2:"),
        "untrusted protocol value: {error}"
    );
    assert!(
        error.to_string().contains("untrusted-value"),
        "untrusted protocol value: {error}"
    );
    configure(directory.path(), &server.url, "chat-completions", "bearer");
    std::fs::remove_file(directory.path().join("auth.json")).unwrap();
    configuration_failure(
        &turn,
        "test",
        "credentials are missing",
        "deleted auth file",
    )
    .await;
    assert!(
        server.received.try_recv().is_err(),
        "deleted auth file: request reached the server"
    );
}

#[tokio::test]
async fn unimplemented_protocols_and_auth_are_configuration_errors() {
    let directory = private_dir();
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
    let turn = gateway.open_turn();
    for protocol in ["responses", "interactions"] {
        for auth in ["none", "bearer"] {
            configure(directory.path(), "http://localhost:1", protocol, auth);
            configuration_failure(
                &turn,
                "test",
                &format!("protocol {protocol} is not implemented"),
                &format!("{protocol}/{auth}"),
            )
            .await;
        }
    }
    configuration_failure(
        &turn,
        "chatgpt",
        "auth oauth:chatgpt is not implemented",
        "chatgpt",
    )
    .await;
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

struct ResumeClock;
impl Drop for ResumeClock {
    fn drop(&mut self) {
        tokio::time::resume();
    }
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
                let events = if name == "body tail timeout after Done" {
                    // 连接保持打开，收尾读取要等 TAIL_WAIT。只在这一段暂停时钟，
                    // 其余传输失败仍按真实 I/O 结束。
                    tokio::time::pause();
                    let _resume = ResumeClock;
                    let started = tokio::time::Instant::now();
                    let mut pending = std::pin::pin!(stream.collect::<Vec<_>>());
                    let mut steps = 0;
                    let events = loop {
                        tokio::select! {
                            biased;
                            events = &mut pending => break events,
                            _ = tokio::time::advance(Duration::from_millis(100)) => {
                                steps += 1;
                                assert!(steps < 20, "{name} did not finish");
                            }
                        }
                    };
                    // 与 client.rs 的 TAIL_WAIT（500ms）一致：提前结束说明没在等收尾。
                    assert!(
                        started.elapsed() >= Duration::from_millis(500),
                        "{name} finished after {:?}, before the tail wait",
                        started.elapsed()
                    );
                    events
                } else {
                    stream.collect::<Vec<_>>().await
                };
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
    let gateway = RequestGateway::new(directory.path().into(), vec![TLS.ca_der.clone()]).unwrap();
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
    auth.set("xai", "EMPTY").unwrap();
    let gateway = RequestGateway::new(directory.path().into(), Vec::new()).unwrap();
    let turn = gateway.open_turn();
    let body = format!("current-key {long_key} EMPTY");
    server.respond(format!("HTTP/1.1 401 Unauthorized\r\nx-request-id: req-current-key-123\r\nContent-Length: {}\r\n\r\n{body}", body.len()), false);
    let (result, logs) =
        capture_logs(turn.stream_chat(request("test"), StreamControl::default())).await;
    let error = match result {
        Ok(_) => panic!("HTTP error opened a stream"),
        Err(error) => error,
    };
    assert!(!error.to_string().contains("current-key"));
    assert!(!error.to_string().contains("other-secret"));
    assert!(!error.to_string().contains("EMPTY"));
    assert!(error.to_string().contains("[REDACTED]"));
    assert!(logs.contains("provider responded"));
    assert!(logs.contains("provider returned an error status"));
    assert!(logs.contains("request_id=\"req-[REDACTED]-123\""));
    assert!(!logs.contains("current-key"));
    assert!(!logs.contains("other-secret"));
    let body = format!("data: {{\"error\":{{\"message\":\"{long_key} current-key EMPTY\"}}}}\n\n");
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
    assert!(!failure.message.contains("EMPTY"));
    assert!(failure.message.contains("[REDACTED]"));
    assert!(failure.message.chars().count() <= crate::MAX_DIAGNOSTIC_CHARS);
    assert!(logs.contains("provider stream error"));
    assert!(logs.contains("request_id=\"req-[REDACTED]-456\""));
    assert!(!logs.contains("current-key"));
    assert!(!logs.contains("other-secret"));
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
