// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Harness-only HTTP fixture for `fetch__web` scenarios.
//!
//! Separate from the OpenAI-compatible fake LLM: this server returns a fixed
//! status/headers/body and records each request's method and path. It binds
//! `127.0.0.1:0`, advertises a `localhost` base URL (product net-guard rejects
//! private IP literals), tracks handler threads, and shuts down on drop.
//! [`FixtureHttpServer::start_tls`] 在同一服务外包一层 TLS（HTTP/1.1，与产品
//! `fetch__web` 一致），用于验证冻结的信任根。

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio_rustls::rustls;

use crate::tls::TlsIdentity;

/// Fixed plain-text response the fixture serves for every request.
#[derive(Debug, Clone)]
pub struct HttpFixture {
    body: String,
    content_type: Option<String>,
}

impl HttpFixture {
    pub fn plain(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            content_type: Some("text/plain".into()),
        }
    }

    /// 设置响应类型；None 表示省略 Content-Type。
    pub fn with_content_type(mut self, content_type: Option<&str>) -> Self {
        self.content_type = content_type.map(str::to_owned);
        self
    }
}

/// One captured HTTP request (method + path only; body not needed for assert).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedHttpRequest {
    pub method: String,
    pub path: String,
}

#[derive(Debug, Error)]
pub enum FixtureHttpError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("timed out after {0:?} waiting for fixture HTTP requests")]
    Timeout(Duration),
}

#[derive(Default)]
struct CaptureState {
    requests: Vec<CapturedHttpRequest>,
    accepts: usize,
    probes: BTreeMap<SocketAddr, Option<usize>>,
    /// 客户端在 TLS 握手中拒绝了服务端证书的次数。
    rejected_handshakes: usize,
    /// 夹具自身的首个错误（accept 或响应写出失败）。对端拒绝证书、响应写完后的 TLS 收尾
    /// 失败都不算。
    first_error: Option<String>,
}

/// Localhost HTTP fixture with OS-assigned port and bounded shutdown.
pub struct FixtureHttpServer {
    listener: TcpListener,
    tls: bool,
    captures: Arc<(Mutex<CaptureState>, Condvar)>,
    serve_handle: Option<JoinHandle<()>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    shutdown: Arc<Mutex<bool>>,
}

impl FixtureHttpServer {
    /// Bind `127.0.0.1:0` and serve `fixture` for every request until drop.
    pub fn start(fixture: HttpFixture) -> Result<Self, FixtureHttpError> {
        Self::start_inner(fixture, None)
    }

    /// 同 [`Self::start`]，但用 `identity` 的证书提供 TLS。
    pub fn start_tls(
        fixture: HttpFixture,
        identity: &TlsIdentity,
    ) -> Result<Self, FixtureHttpError> {
        Self::start_inner(fixture, Some(identity.server_config(&[])?))
    }

    fn start_inner(
        fixture: HttpFixture,
        tls: Option<Arc<rustls::ServerConfig>>,
    ) -> Result<Self, FixtureHttpError> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let captures = Arc::new((Mutex::new(CaptureState::default()), Condvar::new()));
        let handlers = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let secure = tls.is_some();
        let serve_handle = {
            let listener_clone = listener.try_clone()?;
            let captures = Arc::clone(&captures);
            let handlers = Arc::clone(&handlers);
            let shutdown = Arc::clone(&shutdown);
            thread::spawn(move || {
                serve_loop(listener_clone, fixture, tls, captures, handlers, shutdown);
            })
        };
        Ok(Self {
            listener,
            tls: secure,
            captures,
            serve_handle: Some(serve_handle),
            handlers,
            shutdown,
        })
    }

    /// Base URL using hostname `localhost` (not `127.0.0.1`) so product
    /// net-guard accepts it when `allow_localhost` + allowlist are set.
    pub fn base_url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://localhost:{}", self.port())
    }

    pub fn port(&self) -> u16 {
        self.listener.local_addr().expect("bound").port()
    }

    pub fn url_path(&self, path: &str) -> String {
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        format!("{}{path}", self.base_url())
    }

    /// 等服务线程 accept 探针后，读取此前的连接数；探针不计入连接、请求或错误。
    /// 调用前须已结束被测操作；Linux accept 队列中此前完成握手的连接先于探针出队。
    pub fn accepted_connections_after_probe(
        &self,
        timeout: Duration,
    ) -> Result<usize, FixtureHttpError> {
        let deadline = Instant::now() + timeout;
        let (lock, wake) = &*self.captures;
        let mut state = lock.lock().unwrap();
        // accept 后的分类也持此锁，保证服务线程不会在登记地址之前处理探针。
        let probe = TcpStream::connect_timeout(&self.listener.local_addr()?, timeout)?;
        let address = probe.local_addr()?;
        state.probes.insert(address, None);
        loop {
            if let Some(Some(count)) = state.probes.get(&address) {
                let count = *count;
                state.probes.remove(&address);
                return Ok(count);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(FixtureHttpError::Timeout(timeout));
            }
            state = wake.wait_timeout(state, remaining).unwrap().0;
        }
    }

    pub fn captured_requests(&self) -> Vec<CapturedHttpRequest> {
        self.captures.0.lock().unwrap().requests.clone()
    }

    /// 夹具自身的首个错误；场景结束时断言它为空。
    pub fn first_error(&self) -> Option<String> {
        self.captures.0.lock().unwrap().first_error.clone()
    }
    /// 客户端在 TLS 握手中拒绝服务端证书的次数。
    pub fn rejected_handshakes(&self) -> usize {
        self.captures.0.lock().unwrap().rejected_handshakes
    }

    pub fn wait_for_requests(
        &self,
        count: usize,
        timeout: Duration,
    ) -> Result<(), FixtureHttpError> {
        let deadline = Instant::now() + timeout;
        let (lock, wake) = &*self.captures;
        let mut captures = lock.lock().unwrap();
        while captures.requests.len() < count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(FixtureHttpError::Timeout(timeout));
            }
            let (next, result) = wake.wait_timeout(captures, remaining).unwrap();
            captures = next;
            if result.timed_out() && captures.requests.len() < count {
                return Err(FixtureHttpError::Timeout(timeout));
            }
        }
        Ok(())
    }
}

impl Drop for FixtureHttpServer {
    fn drop(&mut self) {
        if let Ok(mut flag) = self.shutdown.lock() {
            *flag = true;
        }
        if let Some(handle) = self.serve_handle.take() {
            let _ = handle.join();
        }
        let mut handlers = self.handlers.lock().unwrap();
        for handle in handlers.drain(..) {
            let _ = handle.join();
        }
    }
}

fn serve_loop(
    listener: TcpListener,
    fixture: HttpFixture,
    tls: Option<Arc<rustls::ServerConfig>>,
    captures: Arc<(Mutex<CaptureState>, Condvar)>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    shutdown: Arc<Mutex<bool>>,
) {
    loop {
        if *shutdown.lock().unwrap() {
            break;
        }
        match listener.accept() {
            Ok((stream, address)) => {
                {
                    let mut state = captures.0.lock().unwrap();
                    let count = state.accepts;
                    if let Some(probe) = state.probes.get_mut(&address) {
                        *probe = Some(count);
                        captures.1.notify_all();
                        continue;
                    }
                    state.accepts += 1;
                }
                let fixture = fixture.clone();
                let tls = tls.clone();
                let captures = Arc::clone(&captures);
                let handle = thread::spawn(move || {
                    if let Err(err) = handle_connection(stream, &fixture, tls, &captures) {
                        let mut state = captures.0.lock().unwrap();
                        state.first_error.get_or_insert(format!("serve: {err}"));
                    }
                });
                handlers.lock().unwrap().push(handle);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(err) => {
                let mut state = captures.0.lock().unwrap();
                state.first_error.get_or_insert(format!("accept: {err}"));
                break;
            }
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    fixture: &HttpFixture,
    tls: Option<Arc<rustls::ServerConfig>>,
    captures: &(Mutex<CaptureState>, Condvar),
) -> Result<(), std::io::Error> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let Some(config) = tls else {
        return handle_request(&mut stream, fixture, captures);
    };
    let mut connection = rustls::ServerConnection::new(config).map_err(std::io::Error::other)?;
    while connection.is_handshaking() {
        if connection.complete_io(&mut stream).is_err() {
            captures.0.lock().unwrap().rejected_handshakes += 1;
            return Ok(());
        }
    }
    let mut tls = rustls::StreamOwned::new(connection, stream);
    handle_request(&mut tls, fixture, captures)?;
    // 响应已完整写出；客户端读完就可能关闭连接，close_notify 失败不算夹具错误。
    tls.conn.send_close_notify();
    let _ = tls.flush();
    Ok(())
}

fn handle_request(
    stream: &mut (impl Read + Write),
    fixture: &HttpFixture,
    captures: &(Mutex<CaptureState>, Condvar),
) -> Result<(), std::io::Error> {
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf)?;
    // 已连接即断开的客户端仍计入 accept，但没有 HTTP 请求可记录或响应。
    if n == 0 {
        return Ok(());
    }
    let raw = String::from_utf8_lossy(&buf[..n]);
    let request_line = raw.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    {
        let (lock, wake) = captures;
        let mut state = lock.lock().unwrap();
        state.requests.push(CapturedHttpRequest { method, path });
        wake.notify_all();
    }

    let body = fixture.body.as_bytes();
    let content_type = fixture
        .content_type
        .as_ref()
        .map(|value| format!("Content-Type: {value}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         {content_type}\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;

    #[test]
    fn fixture_http_counts_immediately_closed_connections() {
        let server = FixtureHttpServer::start(HttpFixture::plain("unused")).expect("start");
        assert_eq!(
            server
                .accepted_connections_after_probe(Duration::from_secs(2))
                .expect("empty barrier"),
            0
        );
        drop(TcpStream::connect(("127.0.0.1", server.port())).expect("connect"));
        assert_eq!(
            server
                .accepted_connections_after_probe(Duration::from_secs(2))
                .expect("closed client barrier"),
            1
        );
        assert_eq!(
            server
                .accepted_connections_after_probe(Duration::from_secs(2))
                .expect("second barrier"),
            1
        );
        assert!(server.captured_requests().is_empty());
        assert!(server.first_error().is_none());
    }

    #[test]
    fn fixture_http_captures_request_and_returns_body() {
        let server = FixtureHttpServer::start(HttpFixture::plain("fixture body")).expect("start");
        let url = server.url_path("/hello");
        // Parse host/port from base_url — connect via 127.0.0.1.
        let port = server.port();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .write_all(b"GET /hello HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write");
        let mut resp = String::new();
        stream.read_to_string(&mut resp).expect("read");
        assert!(resp.contains("200 OK"));
        assert!(resp.contains("\r\nContent-Type: text/plain\r\n"));
        assert_eq!(
            server
                .accepted_connections_after_probe(Duration::from_secs(2))
                .expect("accept barrier"),
            1
        );
        assert!(resp.contains("fixture body"));
        server
            .wait_for_requests(1, Duration::from_secs(2))
            .expect("wait");
        let captured = server.captured_requests();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].method, "GET");
        assert_eq!(captured[0].path, "/hello");
        let _ = url;
    }
}
