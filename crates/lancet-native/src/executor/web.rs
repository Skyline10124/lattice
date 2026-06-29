// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use futures::StreamExt;
use net_guard as security;

use crate::executor::error::ToolError;

/// Structured HTTP fetch outcome for native `fetch.web`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WebFetchOutcome {
    pub status: u16,
    pub body: String,
    pub content_type: Option<String>,
    /// Bytes returned to the agent after caps (may be smaller than `response_size`).
    pub bytes_returned: usize,
    /// Observed or declared response size before return truncation semantics.
    pub response_size: usize,
    pub response_size_known: bool,
    pub truncated: bool,
}

/// Fetch `url` with redirect policy disabled and optional DNS pinning for public hosts.
pub(crate) async fn fetch_url(
    http_client: &reqwest::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    max_bytes: usize,
    pin_public_hosts: bool,
) -> Result<WebFetchOutcome, String> {
    let parsed = check_url(url)?;
    let host = parsed.host_str().unwrap_or("").to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let use_localhost = is_localhost(&host);

    let client = if use_localhost || !pin_public_hosts {
        http_client.clone()
    } else {
        build_pinned_http_client(&host, port)
            .map_err(|e| format!("HTTPS URL not allowed: {url} ({e})"))?
    };

    let mut request = client.get(url);
    for (key, value) in headers {
        request = request.header(key, value);
    }

    let response = request.send().await.map_err(|e| {
        let mut msg = e.to_string();
        let mut source = std::error::Error::source(&e);
        while let Some(cause) = source {
            msg.push_str(&format!(" → {cause}"));
            source = std::error::Error::source(cause);
        }
        ToolError::HttpError(msg).to_string()
    })?;

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let limit = max_bytes;
    if let Some(content_length) = response.content_length()
        && content_length > limit as u64
    {
        let body = read_body_capped(response, limit, Some(content_length as usize)).await?;
        return Ok(WebFetchOutcome {
            status,
            body: body.body,
            content_type,
            bytes_returned: body.bytes_returned,
            response_size: content_length as usize,
            response_size_known: true,
            truncated: true,
        });
    }

    let body = read_body_capped(response, limit, None).await?;
    Ok(WebFetchOutcome {
        status,
        body: body.body.clone(),
        content_type,
        bytes_returned: body.bytes_returned,
        response_size: body.response_size,
        response_size_known: body.response_size_known,
        truncated: body.truncated,
    })
}

struct CappedBody {
    body: String,
    bytes_returned: usize,
    response_size: usize,
    response_size_known: bool,
    truncated: bool,
}

async fn read_body_capped(
    response: reqwest::Response,
    limit: usize,
    declared_length: Option<usize>,
) -> Result<CappedBody, String> {
    let mut buf = Vec::new();
    let mut truncated = false;
    let mut bytes_read = 0usize;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ToolError::HttpError(e.to_string()).to_string())?;
        bytes_read = bytes_read.saturating_add(chunk.len());
        if buf.len() + chunk.len() > limit {
            let remaining = limit.saturating_sub(buf.len());
            buf.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
            truncated = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    let bytes_returned = buf.len();
    let body = String::from_utf8_lossy(&buf).into_owned();
    let (response_size, response_size_known) = if let Some(declared) = declared_length {
        (declared, true)
    } else if truncated {
        (bytes_read, false)
    } else {
        (bytes_returned, true)
    };
    Ok(CappedBody {
        body,
        bytes_returned,
        response_size,
        response_size_known,
        truncated,
    })
}

fn check_url(url_str: &str) -> Result<url::Url, String> {
    let parsed = url::Url::parse(url_str).map_err(|error| format!("invalid URL: {error}"))?;
    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or("");

    if security::host_is_dns_rebinding_domain(host) {
        return Err(format!(
            "URL not allowed: {url_str} (DNS rebinding domain '{host}' blocked)"
        ));
    }

    if scheme == "http" && is_localhost(host) {
        return Ok(parsed);
    }

    if scheme != "https" {
        return Err(format!(
            "URL not allowed: {url_str}. Only https:// and http://localhost are permitted."
        ));
    }

    if is_localhost(host) {
        return Err(format!(
            "HTTPS URL not allowed: {url_str} (host resolves to localhost)"
        ));
    }

    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
    {
        if security::is_private_or_reserved_ip(ip) {
            return Err(format!(
                "HTTPS URL not allowed: {url_str} (blocked private IP {ip})"
            ));
        }
        return Ok(parsed);
    }

    Ok(parsed)
}

fn is_localhost(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

fn build_pinned_http_client(host: &str, port: u16) -> Result<reqwest::Client, String> {
    let pinned = security::resolve_public_socket_addrs(host, port).map_err(|e| e.to_string())?;
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .resolve_to_addrs(host, &pinned)
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_known_dns_rebinding_domains() {
        for host in [
            "nip.io",
            "sub.nip.io",
            "xip.io",
            "sslip.io",
            "localtest.me",
            "lvh.me",
            "vcap.me",
            "burrow.io",
        ] {
            let url = format!("https://{host}/");
            let err = check_url(&url).expect_err("rebinding domain must be rejected");
            assert!(
                err.contains("rebinding"),
                "expected rebinding error for {host}, got: {err}"
            );
        }
    }

    #[test]
    fn rejects_https_localhost() {
        let err = check_url("https://localhost/").expect_err("https localhost must be rejected");
        assert!(err.contains("localhost"));
    }

    #[test]
    fn allows_http_localhost() {
        assert!(check_url("http://localhost:8080/").is_ok());
    }

    #[test]
    fn rejects_private_ip_literal() {
        let err = check_url("https://10.0.0.1/").expect_err("private IP must be rejected");
        assert!(err.contains("private"));
    }
}
