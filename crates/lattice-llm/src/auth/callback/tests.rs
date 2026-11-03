// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const REDIRECT: &str = "http://127.0.0.1:1455/auth/callback";

#[test]
fn callback_checks_state_errors_client_binding_and_unique_parameters() {
    let valid = format!("{REDIRECT}?state=local-state&code=local-code&client_id=local-client");
    let value = parse(
        &SensitiveLoginUrl::new(valid.clone()),
        REDIRECT,
        "local-state",
        None,
    )
    .unwrap();
    assert_eq!(value.code.0, "local-code");
    assert_eq!(value.client_id.0, "local-client");
    let with_fragment = parse(
        &SensitiveLoginUrl::new(format!("{valid}#ignored?state=wrong&code=other")),
        REDIRECT,
        "local-state",
        None,
    )
    .unwrap();
    assert_eq!(with_fragment.code.0, value.code.0);
    assert_eq!(with_fragment.client_id.0, value.client_id.0);
    let returning = format!("{REDIRECT}?state=local-state&code=local-code");
    assert!(
        parse(
            &SensitiveLoginUrl::new(returning.clone()),
            REDIRECT,
            "local-state",
            Some("local-client")
        )
        .is_ok()
    );
    assert!(
        parse(
            &SensitiveLoginUrl::new(valid.clone()),
            REDIRECT,
            "local-state",
            Some("different-client")
        )
        .is_err()
    );
    assert!(
        parse(
            &SensitiveLoginUrl::new(returning),
            REDIRECT,
            "local-state",
            None
        )
        .is_err()
    );
    for query in [
        "code=secret-code&client_id=local-client",
        "state=wrong&code=secret-code&client_id=local-client",
        "state=local-state&state=local-state&code=secret-code&client_id=local-client",
        "state=local-state&st%61te=local-state&code=secret-code&client_id=local-client",
        "state=local-state&error=access_denied&error_description=secret-error",
        "state=local-state&code=secret-code&error=access_denied&client_id=local-client",
        "state=local-state&code=&client_id=local-client",
        "state=local-state&code=secret-code&client_id=",
        "state=local-state&code=secret-code&client_id=dynamic_agent_client",
        "state=local-state&code=%FF&client_id=local-client",
        "state=local-state&code=%z0&client_id=local-client",
    ] {
        let error = parse(
            &SensitiveLoginUrl::new(format!("{REDIRECT}?{query}")),
            REDIRECT,
            "local-state",
            None,
        )
        .err()
        .unwrap();
        assert!(!format!("{error:?}").contains("secret-"));
    }
    for address in [
        "http://localhost:1455/auth/callback",
        "http://127.0.0.1:1456/auth/callback",
        "https://127.0.0.1:1455/auth/callback",
        "http://127.0.0.1:1455/callback",
        "http://127.0.0.1:1455/other/../auth/callback",
        "http://user@127.0.0.1:1455/auth/callback",
    ] {
        assert!(
            parse(
                &SensitiveLoginUrl::new(format!("{address}?state=local-state&code=x&client_id=c")),
                REDIRECT,
                "local-state",
                None
            )
            .is_err()
        );
    }
}

async fn request(address: SocketAddr, text: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(text.as_bytes()).await.unwrap();
        let mut output = Vec::new();
        stream.read_to_end(&mut output).await.unwrap();
        String::from_utf8(output).unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn listener_serves_only_callback_get_and_static_receipt_then_releases_port() {
    let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let address = listener.listener.local_addr().unwrap();
    let authority = listener.authority.clone();
    let accepted = listener.accepted.clone();
    let task = tokio::spawn(async move { listener.receive().await });
    let mut partial = tokio::net::TcpStream::connect(address).await.unwrap();
    partial
        .write_all(b"GET /auth/callback HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), accepted.notified())
        .await
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(LOCAL_IO_TIMEOUT + Duration::from_millis(1)).await;
    tokio::time::resume();
    let mut buffer = [0; 1];
    assert!(
        !matches!(tokio::time::timeout(Duration::from_secs(2), partial.read(&mut buffer)).await.unwrap(), Ok(n) if n > 0)
    );
    for (path, method, host, expected) in [
        ("/other", "GET", authority.as_str(), "404"),
        ("/auth/callback", "POST", authority.as_str(), "405"),
        ("/auth/callback", "GET", "wrong-host", "400"),
    ] {
        let reply = request(
            address,
            &format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        )
        .await;
        assert!(
            reply.starts_with(&format!("HTTP/1.1 {expected}")),
            "预期 {expected}，实际响应首行 {:?}",
            reply.lines().next()
        );
    }
    let many_headers: String = (0..32)
        .map(|index| format!("X-{index}: value\r\n"))
        .collect();
    for (headers, expected) in [
        ("Content-Length: 4\r\n\r\nbody".into(), "400"),
        (
            "Transfer-Encoding: chunked\r\n\r\n4\r\nbody\r\n0\r\n\r\n".into(),
            "400",
        ),
        (
            format!("X-Padding: {}\r\n\r\n", "x".repeat(MAX_URL_BYTES)),
            "431",
        ),
        (format!("{many_headers}\r\n"), "431"),
    ] {
        let reply = request(
            address,
            &format!("GET /auth/callback HTTP/1.1\r\nHost: {authority}\r\n{headers}"),
        )
        .await;
        assert!(
            reply.starts_with(&format!("HTTP/1.1 {expected}")),
            "{} 字节请求头预期 {expected}，实际响应首行 {:?}",
            headers.len(),
            reply.lines().next()
        );
    }
    let reply = request(address, &format!("GET /auth/callback?code=secret-code&state=secret-state HTTP/1.1\r\nHost: {authority}\r\n\r\n")).await;
    assert!(reply.starts_with("HTTP/1.1 200"));
    assert!(reply.to_lowercase().contains("cache-control: no-store"));
    assert!(!reply.contains("secret-"));
    let url = task.await.unwrap().unwrap();
    assert!(url.expose().contains("code=secret-code"));
    assert!(tokio::net::TcpListener::bind(address).await.is_ok());
}

#[tokio::test]
async fn cancellation_closes_partial_http_connection_and_port_conflict_is_explicit() {
    let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let address = listener.listener.local_addr().unwrap();
    let error = Listener::bind(address).await.err().unwrap();
    assert!(error.to_string().contains(&address.port().to_string()));
    assert!(error.to_string().contains("AddrInUse"));
    let task = tokio::spawn(async move { listener.receive().await });
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /auth/callback HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    task.abort();
    assert!(task.await.err().unwrap().is_cancelled());
    let mut buffer = [0; 1];
    assert!(
        !matches!(tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer)).await.unwrap(), Ok(n) if n > 0)
    );
    assert!(tokio::net::TcpListener::bind(address).await.is_ok());
}
