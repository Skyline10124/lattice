// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{future::Future, sync::Arc};

use futures::FutureExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

use super::*;

const CA: &[u8] = include_bytes!("../../../tests/fixtures/metadata/ca.der");
const CERT: &[u8] = include_bytes!("../../../tests/fixtures/metadata/server.der");
const KEY: &[u8] = include_bytes!("../../../tests/fixtures/metadata/server-key.der");
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
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(CERT.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec())),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
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
    vec![CA.to_vec()]
}

fn prior_cache() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("cache")).unwrap();
    std::fs::write(dir.path().join("cache/models.json"), PREVIOUS).unwrap();
    dir
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
    let refresh = tokio::spawn(async move { refresh(&config_dir, &url, &roots()).await });
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
async fn failed_fetch_or_parse_keeps_the_previous_cache() {
    for response in [
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n{",
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n{\"openai\":{}}",
        b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"openai\":[{}]}",
    ] {
        let server = Server::response(response.to_vec()).await;
        let dir = prior_cache();
        assert!(refresh(dir.path(), &server.url, &roots()).await.is_err());
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
        assert!(write_and_replace(temporary, &destination, b"new cache").is_err());
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
    let error = refresh(dir.path(), &server.url, &roots())
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
    let error = refresh(dir.path(), &server.url, &[]).await.unwrap_err();
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
        let error = refresh(dir.path(), url, &roots()).await.unwrap_err();
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
        refresh(dir.path(), &server.url, &roots()).await,
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
        let result = refresh(dir.path(), &server.url, &roots()).await;
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
    let mut refresh = tokio::spawn(async move { refresh(&config_dir, &url, &roots()).await });
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
    let mut refresh = tokio::spawn(async move { refresh(&config_dir, &url, &roots()).await });
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
