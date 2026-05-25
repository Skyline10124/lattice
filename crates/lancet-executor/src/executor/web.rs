use lattice_llm::security;

pub(crate) async fn web_fetch(
    http_client: &reqwest::Client,
    limits: &lattice_tooling::ToolLimits,
    args: &serde_json::Value,
) -> Result<String, String> {
    use futures::StreamExt;
    use lattice_tooling::ToolError;

    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let parsed = check_url(url)?;
    let host = parsed.host_str().unwrap_or("");
    let port = parsed.port_or_known_default().unwrap_or(443);

    let response = if is_localhost(host) {
        http_client
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::HttpError(e.to_string()).to_string())?
    } else {
        let pinned_client = build_pinned_http_client(host, port)
            .map_err(|e| format!("HTTPS URL not allowed: {url} ({e})"))?;
        pinned_client
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::HttpError(e.to_string()).to_string())?
    };
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
