// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Neutral URL and DNS safety guards.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

/// Errors produced by URL configuration checks and DNS resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetGuardError {
    /// The URL or hostname violates the network safety configuration.
    Config { message: String },
    /// DNS resolution failed or returned no addresses.
    Dns { message: String },
}

impl fmt::Display for NetGuardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config { message } => write!(formatter, "configuration error: {message}"),
            Self::Dns { message } => write!(formatter, "DNS error: {message}"),
        }
    }
}

impl std::error::Error for NetGuardError {}

/// Public DNS services that map arbitrary labels to attacker-chosen IPs.
const DNS_REBINDING_DOMAIN_SUFFIXES: &[&str] = &[
    "nip.io",
    "xip.io",
    "sslip.io",
    "localtest.me",
    "lvh.me",
    "vcap.me",
    "burrow.io",
];

/// Returns true when `host` is a known DNS rebinding helper domain.
pub fn host_is_dns_rebinding_domain(host: &str) -> bool {
    DNS_REBINDING_DOMAIN_SUFFIXES
        .iter()
        .any(|suffix| host_matches_domain_suffix(host, suffix))
}

fn host_matches_domain_suffix(host: &str, suffix: &str) -> bool {
    host.eq_ignore_ascii_case(suffix)
        || (host.len() > suffix.len() + 1
            && host.as_bytes().get(host.len() - suffix.len() - 1) == Some(&b'.')
            && host[host.len() - suffix.len()..].eq_ignore_ascii_case(suffix))
}

fn is_localhost_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

fn is_private_or_reserved(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_documentation()
                || octets[0] == 10
                || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 168)
                || (octets[0] == 169 && octets[1] == 254)
                || octets[0] == 127
                || octets[0] == 0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || is_ipv4_mapped_private(&v6)
        }
    }
}

/// Returns true for loopback, RFC1918, link-local, documentation, and related reserved space.
pub fn is_private_or_reserved_ip(ip: IpAddr) -> bool {
    is_private_or_reserved(ip)
}

fn is_ipv4_mapped_private(v6: &Ipv6Addr) -> bool {
    let segments = v6.segments();
    if segments[0..5] == [0, 0, 0, 0, 0] && segments[5] == 0xffff {
        let v4 = Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            (segments[6] & 0xff) as u8,
            (segments[7] >> 8) as u8,
            (segments[7] & 0xff) as u8,
        );
        return is_private_or_reserved(IpAddr::V4(v4));
    }
    false
}

/// Resolve `host:port` and return socket addresses that are all on the public internet.
///
/// Callers should pin these addresses on the HTTP request to close DNS-rebinding TOCTOU.
pub fn resolve_public_socket_addrs(
    host: &str,
    port: u16,
) -> Result<Vec<SocketAddr>, NetGuardError> {
    if host_is_dns_rebinding_domain(host) {
        return Err(NetGuardError::Config {
            message: format!(
                "hostname '{host}' is a known DNS rebinding domain and is not allowed"
            ),
        });
    }

    if is_localhost_host(host) {
        return Err(NetGuardError::Config {
            message: format!(
                "hostname '{host}' is localhost and cannot be resolved for remote pinning"
            ),
        });
    }

    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        if is_private_or_reserved(ip) {
            return Err(NetGuardError::Config {
                message: format!("hostname targets private/reserved IP {ip}"),
            });
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }

    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| NetGuardError::Dns {
            message: format!("DNS resolution failed for '{host}': {error}"),
        })?
        .collect();

    if addrs.is_empty() {
        return Err(NetGuardError::Dns {
            message: format!("hostname '{host}' did not resolve to any address"),
        });
    }

    for addr in &addrs {
        if is_private_or_reserved(addr.ip()) {
            return Err(NetGuardError::Config {
                message: format!(
                    "hostname '{host}' resolves to private/reserved IP {}",
                    addr.ip()
                ),
            });
        }
    }

    Ok(addrs)
}

/// Validate a provider base URL.
pub fn validate_base_url(url: &str) -> Result<(), NetGuardError> {
    if url.is_empty() {
        return Err(NetGuardError::Config {
            message: "base_url must not be empty".to_string(),
        });
    }

    let parsed = url::Url::parse(url).map_err(|error| NetGuardError::Config {
        message: format!("invalid base_url: {error}"),
    })?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(NetGuardError::Config {
            message: format!(
                "scheme '{}' not allowed — only http:// and https:// are permitted",
                parsed.scheme()
            ),
        });
    }

    let host = parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| NetGuardError::Config {
            message: "base_url has scheme but no host".to_string(),
        })?;

    if parsed.scheme() == "http" && !is_localhost_host(host) {
        return Err(NetGuardError::Config {
            message: "HTTP is only allowed for localhost — use HTTPS for remote hosts".to_string(),
        });
    }

    if parsed.username() != "" || parsed.password().is_some() {
        return Err(NetGuardError::Config {
            message: "base_url contains userinfo which is not allowed".to_string(),
        });
    }

    let host_clean = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host_clean.parse::<IpAddr>()
        && !is_localhost_host(host)
        && is_private_or_reserved(ip)
    {
        return Err(NetGuardError::Config {
            message: format!("base_url targets private/reserved IP {ip}"),
        });
    }

    if host_is_dns_rebinding_domain(host) {
        return Err(NetGuardError::Config {
            message: format!(
                "base_url hostname '{host}' is a known DNS rebinding domain and is not allowed"
            ),
        });
    }

    // Best-effort DNS check when resolution is available. Request-time pinning
    // by callers remains authoritative.
    if !host.ends_with(".local") && !is_localhost_host(host) {
        let port = parsed
            .port()
            .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
        if let Ok(addrs) = (host, port).to_socket_addrs() {
            for addr in addrs {
                if is_private_or_reserved(addr.ip()) {
                    return Err(NetGuardError::Config {
                        message: format!(
                            "hostname '{host}' resolves to private/reserved IP {}",
                            addr.ip()
                        ),
                    });
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_file_scheme() {
        assert!(validate_base_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn reject_userinfo() {
        assert!(validate_base_url("https://user:pass@api.example.com/v1").is_err());
    }

    #[test]
    fn reject_plain_http_remote() {
        assert!(validate_base_url("http://example.com/v1").is_err());
    }

    #[test]
    fn allow_loopback_ipv4() {
        assert!(validate_base_url("http://127.0.0.1/v1").is_ok());
        assert!(validate_base_url("https://127.0.0.1/v1").is_ok());
    }

    #[test]
    fn allow_loopback_ipv6() {
        assert!(validate_base_url("http://[::1]/v1").is_ok());
        assert!(validate_base_url("https://[::1]/v1").is_ok());
    }

    #[test]
    fn reject_private_ipv4() {
        assert!(validate_base_url("https://10.0.0.1/v1").is_err());
        assert!(validate_base_url("https://192.168.1.1/v1").is_err());
        assert!(validate_base_url("https://172.16.0.1/v1").is_err());
    }

    #[test]
    fn allow_localhost_http() {
        assert!(validate_base_url("http://localhost:11434/v1").is_ok());
    }

    #[test]
    fn allow_localhost_https() {
        assert!(validate_base_url("https://localhost/v1").is_ok());
    }

    #[test]
    fn allow_https_public() {
        assert!(validate_base_url("https://api.openai.com/v1").is_ok());
        assert!(validate_base_url("https://api.anthropic.com").is_ok());
    }

    #[test]
    fn reject_bad_scheme() {
        assert!(validate_base_url("ftp://example.com/v1").is_err());
    }

    #[test]
    fn reject_empty_host_after_scheme() {
        assert!(validate_base_url("https://").is_err());
    }

    #[test]
    fn reject_dns_rebinding_domains_in_base_url() {
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
            let url = format!("https://{host}/v1");
            let err = validate_base_url(&url).expect_err("rebinding domain must be rejected");
            assert!(
                err.to_string().contains("rebinding"),
                "expected rebinding error for {host}, got: {err}"
            );
        }
    }

    #[test]
    fn resolve_public_socket_addrs_rejects_private_literal() {
        let err = resolve_public_socket_addrs("10.0.0.1", 443)
            .expect_err("private literal must be rejected");
        assert!(err.to_string().contains("private"));
    }

    #[test]
    fn resolve_public_socket_addrs_rejects_rebinding_domain() {
        let err = resolve_public_socket_addrs("evil.nip.io", 443)
            .expect_err("rebinding domain must be rejected");
        assert!(err.to_string().contains("rebinding"));
    }

    #[test]
    fn reject_empty_url() {
        let err = validate_base_url("").expect_err("empty base_url must be rejected");
        assert!(
            err.to_string().contains("must not be empty"),
            "error should explain empty url, got: {err}"
        );
    }
}
