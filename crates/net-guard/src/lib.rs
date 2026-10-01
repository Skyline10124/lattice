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

fn is_non_global_ietf_assignment(v6: Ipv6Addr, preserve_benchmarking: bool) -> bool {
    let [first, second, third, ..] = v6.segments();
    if first != 0x2001 || second >= 0x0200 {
        return false;
    }

    let address = u128::from_be_bytes(v6.octets());
    let globally_reachable_exception = matches!(
        address,
        0x2001_0001_0000_0000_0000_0000_0000_0001 | 0x2001_0001_0000_0000_0000_0000_0000_0002
    ) || second == 0x0003
        || (second == 0x0004 && third == 0x0112)
        || (preserve_benchmarking && second == 0x0002 && third == 0);

    !globally_reachable_exception
}

fn is_private_or_reserved(ip: IpAddr, deny_benchmarking: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 10
                || (a == 100 && (b & 0xc0) == 64)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 169 && b == 254)
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 88 && c == 99)
                || (a == 192 && b == 168)
                || (deny_benchmarking && a == 198 && (b & 0xfe) == 18)
                || a == 127
                || a == 0
                || a >= 240
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] & 0xffc0) == 0xfec0
                || is_non_global_ietf_assignment(v6, !deny_benchmarking)
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                || (segments[0] == 0x3fff && segments[1] <= 0x0fff)
                || segments[0] == 0x5f00
                || (deny_benchmarking && segments[0] == 0x2002)
                || segments[..4] == [0x0100, 0, 0, 0]
                || segments[..3] == [0x0064, 0xff9b, 0x0001]
                || embedded_ipv4(&v6)
                    .is_some_and(|v4| is_private_or_reserved(v4.into(), deny_benchmarking))
        }
    }
}

/// Returns true for private/reserved space while preserving proxy fake-IP ranges.
pub fn is_private_or_reserved_ip(ip: IpAddr) -> bool {
    is_private_or_reserved(ip, false)
}

/// Strict Tool-egress classification, including the RFC 2544 benchmarking range.
pub fn is_forbidden_tool_target_ip(ip: IpAddr) -> bool {
    is_private_or_reserved(ip, true)
}

fn embedded_ipv4(v6: &Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = v6.segments();
    let (high, low) = if (segments[..5] == [0; 5] && segments[5] == 0xffff)
        || segments[..6] == [0; 6]
        || (segments[..2] == [0x0064, 0xff9b] && segments[2..6] == [0; 4])
    {
        (segments[6], segments[7])
    } else if segments[0] == 0x2002 {
        (segments[1], segments[2])
    } else {
        return None;
    };
    Some(Ipv4Addr::new(
        (high >> 8) as u8,
        high as u8,
        (low >> 8) as u8,
        low as u8,
    ))
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
        if is_private_or_reserved(ip, false) {
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
        if is_private_or_reserved(addr.ip(), false) {
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
        && is_private_or_reserved(ip, false)
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
                if is_private_or_reserved(addr.ip(), false) {
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
    fn reject_private_ipv4() {
        assert!(validate_base_url("https://10.0.0.1/v1").is_err());
        assert!(validate_base_url("https://192.168.1.1/v1").is_err());
        assert!(validate_base_url("https://172.16.0.1/v1").is_err());
    }

    #[test]
    fn classify_reserved_and_embedded_private_ranges() {
        for raw in [
            "100.64.0.1",
            "192.0.0.1",
            "192.88.99.1",
            "240.0.0.1",
            "255.255.255.255",
            "64:ff9b::a00:1",
            "64:ff9b:1::1",
            "2002:0a00:0001::",
            "::10.0.0.1",
            "100::1",
            "2001::1",
            "2001:10::1",
            "2001:100::1",
            "2001:20::1",
            "2001:db8::1",
            "3fff:0fff::1",
            "5f00::1",
            "fec0::1",
        ] {
            let ip: IpAddr = raw.parse().unwrap();
            assert!(is_private_or_reserved_ip(ip), "{raw} must fail closed");
        }
        for raw in [
            "8.8.8.8",
            "2606:4700:4700::1111",
            "64:ff9b::808:808",
            "2001:1::1",
            "2001:1::2",
            "2001:3::1",
            "2001:4:112::1",
            "3fff:1000::1",
        ] {
            let ip: IpAddr = raw.parse().unwrap();
            assert!(!is_private_or_reserved_ip(ip), "{raw} must remain public");
        }
        for raw in [
            "198.18.0.1",
            "64:ff9b::c612:1",
            "2001:2::1",
            "2002:c612:0001::",
        ] {
            let ip: IpAddr = raw.parse().unwrap();
            assert!(
                !is_private_or_reserved_ip(ip),
                "{raw} is a proxy fake-IP range"
            );
            assert!(
                is_forbidden_tool_target_ip(ip),
                "{raw} must deny Tool egress"
            );
        }
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
    fn reject_empty_url() {
        let err = validate_base_url("").expect_err("empty base_url must be rejected");
        assert!(
            err.to_string().contains("must not be empty"),
            "error should explain empty url, got: {err}"
        );
    }
}
