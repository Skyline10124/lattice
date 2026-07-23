// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Harness-only HTTP fixture for `fetch.web` scenarios.
//!
//! Separate from the OpenAI-compatible fake LLM: this server returns a fixed
//! status/headers/body and records each request's method and path. It binds
//! `127.0.0.1:0`, advertises a `localhost` base URL (product net-guard rejects
//! private IP literals), tracks handler threads, and shuts down on drop.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;

/// Fixed response the fixture serves for every request.
#[derive(Debug, Clone)]
pub struct HttpFixture {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

impl HttpFixture {
    pub fn plain(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "text/plain".into(),
            body: body.into(),
        }
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
}

/// Localhost HTTP fixture with OS-assigned port and bounded shutdown.
pub struct FixtureHttpServer {
    listener: TcpListener,
    captures: Arc<(Mutex<CaptureState>, Condvar)>,
    serve_handle: Option<JoinHandle<()>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    shutdown: Arc<Mutex<bool>>,
}

impl FixtureHttpServer {
    /// Bind `127.0.0.1:0` and serve `fixture` for every request until drop.
    pub fn start(fixture: HttpFixture) -> Result<Self, FixtureHttpError> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let captures = Arc::new((Mutex::new(CaptureState::default()), Condvar::new()));
        let handlers = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let serve_handle = {
            let listener_clone = listener.try_clone()?;
            let captures = Arc::clone(&captures);
            let handlers = Arc::clone(&handlers);
            let shutdown = Arc::clone(&shutdown);
            thread::spawn(move || {
                serve_loop(listener_clone, fixture, captures, handlers, shutdown);
            })
        };
        Ok(Self {
            listener,
            captures,
            serve_handle: Some(serve_handle),
            handlers,
            shutdown,
        })
    }

    /// Base URL using hostname `localhost` (not `127.0.0.1`) so product
    /// net-guard accepts it when `allow_localhost` + allowlist are set.
    pub fn base_url(&self) -> String {
        format!("http://localhost:{}", self.port())
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

    pub fn captured_requests(&self) -> Vec<CapturedHttpRequest> {
        self.captures.0.lock().unwrap().requests.clone()
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
    captures: Arc<(Mutex<CaptureState>, Condvar)>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    shutdown: Arc<Mutex<bool>>,
) {
    loop {
        if *shutdown.lock().unwrap() {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let fixture = fixture.clone();
                let captures = Arc::clone(&captures);
                let handle = thread::spawn(move || {
                    if let Err(err) = handle_request(stream, &fixture, &captures) {
                        eprintln!("fixture_http: serve error: {err}");
                    }
                });
                handlers.lock().unwrap().push(handle);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(err) => {
                eprintln!("fixture_http: accept error: {err}");
                break;
            }
        }
    }
}

fn handle_request(
    mut stream: TcpStream,
    fixture: &HttpFixture,
    captures: &(Mutex<CaptureState>, Condvar),
) -> Result<(), std::io::Error> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;

    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf)?;
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

    let reason = match fixture.status {
        200 => "OK",
        404 => "Not Found",
        other => {
            let _ = other;
            "OK"
        }
    };
    let body = fixture.body.as_bytes();
    let response = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        fixture.status,
        reason,
        fixture.content_type,
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
    fn fixture_http_captures_request_and_returns_body() {
        let server =
            FixtureHttpServer::start(HttpFixture::plain(200, "fixture body")).expect("start");
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
