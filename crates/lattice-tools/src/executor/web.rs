use std::net::{IpAddr, ToSocketAddrs};

pub(super) async fn web_fetch(
    http_client: &reqwest::Client,
    limits: &crate::ToolLimits,
    args: &serde_json::Value,
) -> Result<String, String> {
    use crate::tool_error::ToolError;
    use futures::StreamExt;

    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
    check_url(url)?;
    let response = http_client
        .get(url)
        .send()
        .await
        .map_err(|e| ToolError::HttpError(e.to_string()).to_string())?;
    let limit = limits.max_http_response_size;
    if let Some(content_length) = response.content_length() {
        if content_length > limit as u64 {
            return Err(ToolError::HttpError(format!(
                "Response too large: {} bytes (max {})",
                content_length, limit
            ))
            .to_string());
        }
    }
    let mut buf = Vec::new();
    let mut exceeded = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ToolError::HttpError(e.to_string()).to_string())?;
        if buf.len() + chunk.len() > limit {
            let remaining = limit - buf.len();
            buf.extend_from_slice(&chunk[..remaining]);
            exceeded = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    if exceeded {
        let truncated = String::from_utf8_lossy(&buf);
        Ok(format!(
            "{}[TRUNCATED: response exceeded {} bytes]",
            truncated.trim_end(),
            limit
        ))
    } else {
        Ok(String::from_utf8_lossy(&buf).to_string())
    }
}

fn check_url(url_str: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url_str).map_err(|error| format!("invalid URL: {error}"))?;
    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or("");

    let rebinding_domains = ["nip.io", "xip.io"];
    if rebinding_domains
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        return Err(format!(
            "URL not allowed: {url_str} (DNS rebinding domain '{host}' blocked)"
        ));
    }

    if scheme == "http" && is_localhost(host) {
        return Ok(());
    }

    if scheme != "https" {
        return Err(format!(
            "URL not allowed: {url_str}. Only https:// and http://localhost are permitted."
        ));
    }

    let port = parsed.port_or_known_default().unwrap_or(443);
    reject_private_resolved_host(host, port)
        .map_err(|reason| format!("HTTPS URL not allowed: {url_str} ({reason})"))
}

fn is_localhost(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

fn reject_private_resolved_host(host: &str, port: u16) -> Result<(), String> {
    if is_localhost(host) {
        return Err("host resolves to localhost".to_string());
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return reject_private_ip(ip);
    }

    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("DNS resolution failed for '{host}:{port}': {error}"))?;
    for addr in addrs {
        reject_private_ip(addr.ip())?;
    }
    Ok(())
}

fn reject_private_ip(ip: IpAddr) -> Result<(), String> {
    match ip {
        IpAddr::V4(v4)
            if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() =>
        {
            Err(format!("blocked private IPv4 address {v4}"))
        }
        IpAddr::V6(v6)
            if v6.is_loopback()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_unspecified() =>
        {
            Err(format!("blocked private IPv6 address {v6}"))
        }
        _ => Ok(()),
    }
}
