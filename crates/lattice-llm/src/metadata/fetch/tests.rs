// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{future::Future, sync::Arc};

use futures::FutureExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

use super::*;
use crate::metadata::Metadata;
use crate::test_tls::TLS;

const PREVIOUS: &[u8] = br#"{"openai":{"models":{"old":{"id":"old"}}}}"#;

struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start<F, Fut>(respond: F) -> Self
    where
        F: FnOnce(TlsStream<TcpStream>, String) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = TlsAcceptor::from(Arc::clone(&TLS.server));
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let Ok(mut stream) = acceptor.accept(stream).await else {
                return;
            };
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                if stream.read_exact(&mut byte).await.is_err() {
                    return;
                }
                request.push(byte[0]);
            }
            respond(stream, String::from_utf8(request).unwrap()).await;
        });
        Self {
            url: format!("https://localhost:{port}/api.json"),
            task,
        }
    }

    async fn response(bytes: Vec<u8>) -> Self {
        Self::start(move |mut stream, _| async move {
            // 客户端在超限或状态错误时可以提前关闭连接。
            let _ = stream.write_all(&bytes).await;
            let _ = stream.shutdown().await;
        })
        .await
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn roots() -> Vec<Vec<u8>> {
    vec![TLS.ca_der.clone()]
}

fn prior_cache() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(dir.path().join("cache")).unwrap();
    std::fs::write(dir.path().join("cache/models.json"), PREVIOUS).unwrap();
    dir
}

#[tokio::test]
async fn refresh_creates_a_private_configuration_directory_and_preserves_existing_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".lattice");
    let registry = ProviderRegistry::builtins();
    for mode in [None, Some(0o755)] {
        if let Some(mode) = mode {
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let server =
            Server::response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec()).await;
        let result = refresh_cancellable(
            &config,
            &registry,
            &server.url,
            &roots(),
            Arc::new(MetadataRefreshControl::default()),
        )
        .await;
        if mode.is_none() {
            result.unwrap();
            assert_eq!(
                std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
                0o700
            );
            crate::auth::AuthStore::new(config.clone())
                .set("anthropic", "local-test-key")
                .unwrap();
        } else {
            assert!(matches!(result, Err(MetadataError::Configuration(_))));
            assert_eq!(
                std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }
}

#[test]
fn cancellation_before_publication_keeps_the_previous_cache() {
    let dir = prior_cache();
    let temporary = tempfile::NamedTempFile::new_in(dir.path().join("cache")).unwrap();
    let control = MetadataRefreshControl::default();
    control.cancel();
    let result = write_and_replace_cancellable(
        temporary,
        &dir.path().join("cache/models.json"),
        b"new",
        &control,
        None,
    );
    assert!(matches!(result, Err(MetadataError::Cancelled)));
    assert_previous(dir.path());
}

#[test]
fn cancellation_return_marks_the_final_cache_and_prevents_later_replacement() {
    use std::sync::mpsc;
    let dir = prior_cache();
    let destination = dir.path().join("cache/models.json");
    let temporary = tempfile::NamedTempFile::new_in(dir.path().join("cache")).unwrap();
    let control = Arc::new(MetadataRefreshControl::default());
    let (checked, passed_check) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let publisher = std::thread::spawn({
        let control = control.clone();
        let destination = destination.clone();
        move || {
            write_and_replace_cancellable(
                temporary,
                &destination,
                b"new cache",
                &control,
                Some(&|| {
                    checked.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(5)).unwrap();
                }),
            )
        }
    });
    passed_check.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(control.cancelled.try_lock().is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), PREVIOUS);
    let (stopped, stop_returned) = mpsc::channel();
    let stopper = std::thread::spawn({
        let control = control.clone();
        let destination = destination.clone();
        move || {
            control.cancel();
            stopped.send(std::fs::read(destination).unwrap()).unwrap();
        }
    });
    release.send(()).unwrap();
    let cache_when_stopped = stop_returned.recv_timeout(Duration::from_secs(5)).unwrap();
    publisher.join().unwrap().unwrap();
    stopper.join().unwrap();
    assert_eq!(cache_when_stopped, b"new cache");
    assert_eq!(std::fs::read(&destination).unwrap(), cache_when_stopped);
    let temporary = tempfile::NamedTempFile::new_in(dir.path().join("cache")).unwrap();
    assert!(matches!(
        write_and_replace_cancellable(temporary, &destination, b"late cache", &control, None),
        Err(MetadataError::Cancelled)
    ));
    assert_eq!(std::fs::read(destination).unwrap(), cache_when_stopped);
    assert_eq!(
        std::fs::read_dir(dir.path().join("cache")).unwrap().count(),
        1
    );
}

fn assert_previous(dir: &Path) {
    assert_eq!(
        std::fs::read(dir.join("cache/models.json")).unwrap(),
        PREVIOUS
    );
    assert_eq!(std::fs::read_dir(dir.join("cache")).unwrap().count(), 1);
}

/// 持续轮询本地 I/O，避免暂停的 Tokio 时钟在等待通道时自动推进。
async fn ready_without_advancing_time<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let frozen = tokio::time::Instant::now();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert_eq!(tokio::time::Instant::now(), frozen);
        if let Some(result) = future.as_mut().now_or_never() {
            return result;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "local I/O did not become ready"
        );
        tokio::task::yield_now().await;
    }
}

async fn advance_to(at: tokio::time::Instant) {
    tokio::time::advance(at.duration_since(tokio::time::Instant::now())).await;
    // 让已唤醒的任务与 I/O 驱动运行，保持时钟停在本次指定的时刻。
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert_eq!(tokio::time::Instant::now(), at);
}

#[tokio::test]
async fn refresh_filters_and_replaces_cache_only_after_a_complete_https_response() {
    let source = br#"{"openai":{"env":["UNTRUSTED_KEY"],"models":{"new":{"id":"new","limit":{"output":99,"input":1},"endpoint":"https://untrusted.invalid"}}}}"#;
    let (started_tx, started_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let server = Server::start(move |mut stream, request| async move {
        assert!(request.starts_with("GET /api.json HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("accept: application/json\r\n")
        );
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            source.len()
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(&source[..source.len() / 2]).await.unwrap();
        started_tx.send(()).unwrap();
        finish_rx.await.unwrap();
        stream.write_all(&source[source.len() / 2..]).await.unwrap();
        stream.shutdown().await.unwrap();
    })
    .await;
    let dir = prior_cache();
    let mut old_file = std::fs::File::open(dir.path().join("cache/models.json")).unwrap();
    let config_dir = dir.path().to_owned();
    let url = server.url.clone();
    let refresh = tokio::spawn(async move {
        refresh_cancellable(
            &config_dir,
            &ProviderRegistry::builtins(),
            &url,
            &roots(),
            Arc::new(MetadataRefreshControl::default()),
        )
        .await
    });
    started_rx.await.unwrap();
    assert_previous(dir.path());
    finish_tx.send(()).unwrap();
    refresh.await.unwrap().unwrap();
    let mut old_contents = Vec::new();
    std::io::Read::read_to_end(&mut old_file, &mut old_contents).unwrap();
    assert_eq!(old_contents, PREVIOUS);
    assert_eq!(
        std::fs::read(dir.path().join("cache/models.json")).unwrap(),
        snapshot_from_json(source).unwrap()
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("cache")).unwrap().count(),
        1
    );
}

#[tokio::test]
async fn refresh_keeps_builtin_and_configured_catalogs_and_missing_entries_are_empty() {
    let registry = ProviderRegistry::parse(
        "providers.toml",
        r#"
        [providers.anthropic]
        catalog = "togetherai"
        [providers.custom]
        protocol = "chat-completions"
        base_url = "https://example.com/v1"
        catalog = "togetherai"
        [providers.missing]
        protocol = "chat-completions"
        base_url = "https://example.com/v1"
        catalog = "does-not-exist"
    "#,
    )
    .unwrap();
    let source = serde_json::json!({
        "anthropic": {"models": {"native": {"id": "native"}}},
        "togetherai": {"env": ["OTHER_KEY"], "api": "https://untrusted.invalid", "models": {
            "org/model": {"id": "org/model", "limit": {"context": 32768, "output": 4096, "input": 1},
                          "cost": {"input": 0.1}, "tool_call": false,
                          "endpoint": "https://untrusted.invalid", "env": ["OTHER_KEY"],
                          "protocol": "ignored"}
        }},
        "not-selected": false
    });
    let bytes = serde_json::to_vec(&source).unwrap();
    let snapshot: serde_json::Value =
        serde_json::from_slice(&snapshot_from_json(&bytes).unwrap()).unwrap();
    assert_eq!(
        snapshot,
        serde_json::json!({"anthropic": source["anthropic"]})
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        bytes.len(),
        source
    );
    let server = Server::response(response.into_bytes()).await;
    let dir = prior_cache();
    let custom = registry.get("custom").unwrap();
    let missing = registry.get("missing").unwrap();
    let before = Metadata::load(dir.path());
    assert!(before.merge(custom).is_empty());
    assert!(before.merge(missing).is_empty());
    refresh_cancellable(
        dir.path(),
        &registry,
        &server.url,
        &roots(),
        Arc::new(MetadataRefreshControl::default()),
    )
    .await
    .unwrap();
    let cached: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("cache/models.json")).unwrap())
            .unwrap();
    assert_eq!(
        cached,
        serde_json::json!({
            "anthropic": {"models": {"native": {"id": "native"}}},
            "togetherai": {"models": {"org/model": {
                "id": "org/model", "limit": {"context": 32768, "output": 4096},
                "cost": {"input": 0.1}, "tool_call": false
            }}}
        })
    );
    let metadata = Metadata::load(dir.path());
    let models = metadata.merge(custom);
    assert_eq!(models.len(), 1);
    let model = &models["org/model"];
    assert_eq!(model.limit.as_ref().unwrap().context, Some(32768));
    assert_eq!(model.limit.as_ref().unwrap().output, Some(4096));
    assert_eq!(model.cost.as_ref().unwrap().input, Some(0.1));
    assert_eq!(model.tool_call, Some(false));
    assert_eq!(metadata.merge(registry.get("anthropic").unwrap()), models);
    assert!(metadata.merge(missing).is_empty());
}

#[tokio::test]
async fn cancelling_a_stalled_refresh_closes_http_without_waiting_for_the_deadline() {
    let (started, ready) = oneshot::channel();
    let (closed, connection_closed) = oneshot::channel();
    let server = Server::start(move |mut stream, _| async move {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
            .await
            .unwrap();
        stream.flush().await.unwrap();
        started.send(()).unwrap();
        let mut byte = [0];
        let _ = stream.read(&mut byte).await;
        let _ = closed.send(());
    })
    .await;
    let dir = prior_cache();
    let cancel = Arc::new(MetadataRefreshControl::default());
    let task = tokio::spawn({
        let directory = dir.path().to_owned();
        let url = server.url.clone();
        let cancel = cancel.clone();
        async move {
            refresh_cancellable(
                &directory,
                &ProviderRegistry::builtins(),
                &url,
                &roots(),
                cancel,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), ready)
        .await
        .unwrap()
        .unwrap();
    cancel.cancel();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), connection_closed)
        .await
        .unwrap()
        .unwrap();
    assert_previous(dir.path());
}

#[tokio::test]
async fn failed_fetch_or_parse_keeps_the_previous_cache() {
    let registry = ProviderRegistry::parse(
        "providers.toml",
        r#"
        [providers.custom]
        protocol = "chat-completions"
        base_url = "https://example.com/v1"
        catalog = "togetherai"
    "#,
    )
    .unwrap();
    for response in [
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n{",
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n{\"openai\":{}}",
        b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"openai\":[{}]}",
        b"HTTP/1.1 200 OK\r\nContent-Length: 19\r\n\r\n{\"togetherai\":[{}]}",
    ] {
        let server = Server::response(response.to_vec()).await;
        let dir = prior_cache();
        assert!(
            refresh_cancellable(
                dir.path(),
                &registry,
                &server.url,
                &roots(),
                Arc::new(MetadataRefreshControl::default()),
            )
            .await
            .is_err()
        );
        assert_previous(dir.path());
    }
}

#[test]
fn cache_write_and_rename_failures_keep_the_old_file_and_remove_temporary_files() {
    for fail_write in [true, false] {
        let dir = prior_cache();
        let cache_dir = dir.path().join("cache");
        let temporary = tempfile::NamedTempFile::new_in(&cache_dir).unwrap();
        let (temporary, destination) = if fail_write {
            let (file, path) = temporary.into_parts();
            drop(file);
            let read_only = std::fs::File::open(&path).unwrap();
            (
                tempfile::NamedTempFile::from_parts(read_only, path),
                cache_dir.join("models.json"),
            )
        } else {
            // 目标的父路径是现有缓存文件，rename 会失败，临时文件仍可正常删除。
            (temporary, cache_dir.join("models.json/blocked"))
        };
        assert!(
            write_and_replace_cancellable(
                temporary,
                &destination,
                b"new cache",
                &MetadataRefreshControl::default(),
                None,
            )
            .is_err()
        );
        assert_previous(dir.path());
    }
}

#[tokio::test]
async fn redirects_are_rejected_before_contacting_the_target() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let response = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: https://127.0.0.1:{}/collect\r\nContent-Length: 0\r\n\r\n",
        target.local_addr().unwrap().port()
    );
    let server = Server::response(response.into_bytes()).await;
    let dir = prior_cache();
    let error = refresh_cancellable(
        dir.path(),
        &ProviderRegistry::builtins(),
        &server.url,
        &roots(),
        Arc::new(MetadataRefreshControl::default()),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        MetadataError::Status(reqwest::StatusCode::TEMPORARY_REDIRECT)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), target.accept())
            .await
            .is_err()
    );
    assert_previous(dir.path());
}

#[tokio::test]
async fn tls_trust_is_limited_to_the_supplied_roots() {
    let server = Server::response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec()).await;
    let dir = prior_cache();
    let error = refresh_cancellable(
        dir.path(),
        &ProviderRegistry::builtins(),
        &server.url,
        &[],
        Arc::new(MetadataRefreshControl::default()),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, MetadataError::Http(_)));
    assert_previous(dir.path());
}

#[tokio::test]
async fn unsafe_urls_are_rejected_without_changing_cache() {
    let dir = prior_cache();
    for url in [
        "http://127.0.0.1/api.json",
        "https://10.0.0.1/api.json",
        "https://192.168.1.1/api.json",
        "https://[fc00::1]/api.json",
        "https://127.0.0.2/api.json",
        "https://127.0.0.1.nip.io/api.json",
        "https://user:password@localhost/api.json",
        "https://localhost/api.json?key=value",
        "https://localhost/api.json#fragment",
    ] {
        let error = refresh_cancellable(
            dir.path(),
            &ProviderRegistry::builtins(),
            url,
            &roots(),
            Arc::new(MetadataRefreshControl::default()),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error,
                MetadataError::Policy(_) | MetadataError::HttpsRequired
            ),
            "{url}: {error}"
        );
        assert_previous(dir.path());
    }
}

#[tokio::test]
async fn declared_or_streamed_responses_above_32_mib_are_abandoned() {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        32 * 1024 * 1024 + 1
    );
    let server = Server::response(header.into_bytes()).await;
    let dir = prior_cache();
    assert!(matches!(
        refresh_cancellable(
            dir.path(),
            &ProviderRegistry::builtins(),
            &server.url,
            &roots(),
            Arc::new(MetadataRefreshControl::default()),
        )
        .await,
        Err(MetadataError::TooLarge)
    ));
    assert_previous(dir.path());
    for size in [32 * 1024 * 1024, 32 * 1024 * 1024 + 1] {
        let server = Server::start(move |mut stream, _| async move {
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}")
                .await;
            let padding = vec![b' '; 64 * 1024];
            let mut remaining = size - 2;
            while remaining > 0 {
                let count = remaining.min(padding.len());
                if stream.write_all(&padding[..count]).await.is_err() {
                    return;
                }
                remaining -= count;
            }
            let _ = stream.shutdown().await;
        })
        .await;
        let dir = prior_cache();
        let result = refresh_cancellable(
            dir.path(),
            &ProviderRegistry::builtins(),
            &server.url,
            &roots(),
            Arc::new(MetadataRefreshControl::default()),
        )
        .await;
        if size == 32 * 1024 * 1024 {
            result.unwrap();
            assert_eq!(
                std::fs::read(dir.path().join("cache/models.json")).unwrap(),
                b"{}\n"
            );
        } else {
            assert!(matches!(result, Err(MetadataError::TooLarge)));
            assert_previous(dir.path());
        }
    }
}

#[tokio::test]
async fn an_incomplete_body_is_abandoned_at_the_total_deadline() {
    let (started_tx, started_rx) = oneshot::channel();
    let server = Server::start(move |mut stream, _| async move {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
            .await
            .unwrap();
        stream.flush().await.unwrap();
        started_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    })
    .await;
    let dir = prior_cache();
    let config_dir = dir.path().to_owned();
    let url = server.url.clone();
    tokio::time::pause();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut refresh = tokio::spawn(async move {
        refresh_cancellable(
            &config_dir,
            &ProviderRegistry::builtins(),
            &url,
            &roots(),
            Arc::new(MetadataRefreshControl::default()),
        )
        .await
    });
    ready_without_advancing_time(started_rx).await.unwrap();
    advance_to(tokio::time::Instant::now()).await;
    advance_to(deadline - Duration::from_millis(1)).await;
    assert!(
        (&mut refresh).now_or_never().is_none(),
        "fetch stopped before 30 seconds"
    );
    assert_previous(dir.path());
    advance_to(deadline + Duration::from_millis(1)).await;
    let error = refresh
        .now_or_never()
        .expect("fetch was still pending after 30 seconds")
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(error, MetadataError::Timeout)
            || matches!(error, MetadataError::Http(ref error) if error.is_timeout())
    );
    assert_previous(dir.path());
}

#[tokio::test]
async fn ongoing_body_data_does_not_extend_the_30_second_deadline() {
    let (started_tx, started_rx) = oneshot::channel();
    let (send_tx, mut send_rx) = mpsc::unbounded_channel::<oneshot::Sender<()>>();
    let server = Server::start(move |mut stream, _| async move {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
            .await
            .unwrap();
        stream.flush().await.unwrap();
        started_tx.send(()).unwrap();
        while let Some(sent) = send_rx.recv().await {
            stream.write_all(b" ").await.unwrap();
            stream.flush().await.unwrap();
            sent.send(()).unwrap();
        }
    })
    .await;
    let dir = prior_cache();
    let config_dir = dir.path().to_owned();
    let url = server.url.clone();
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let mut refresh = tokio::spawn(async move {
        refresh_cancellable(
            &config_dir,
            &ProviderRegistry::builtins(),
            &url,
            &roots(),
            Arc::new(MetadataRefreshControl::default()),
        )
        .await
    });
    ready_without_advancing_time(started_rx).await.unwrap();
    advance_to(started).await;
    for second in [10, 20, 29] {
        advance_to(started + Duration::from_secs(second)).await;
        let (sent_tx, sent_rx) = oneshot::channel();
        send_tx.send(sent_tx).unwrap();
        ready_without_advancing_time(sent_rx).await.unwrap();
        advance_to(tokio::time::Instant::now()).await;
        assert!(
            (&mut refresh).now_or_never().is_none(),
            "fetch stopped at {second} seconds"
        );
        assert_previous(dir.path());
    }
    advance_to(started + Duration::from_secs(30) + Duration::from_millis(1)).await;
    let error = refresh
        .now_or_never()
        .expect("ongoing data extended the total deadline")
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(error, MetadataError::Timeout)
            || matches!(error, MetadataError::Http(ref error) if error.is_timeout())
    );
    assert_previous(dir.path());
}
