//! Base URL validation and SSRF safety floor.
//!
//! Validates that provider base URLs are safe before HTTP clients connect:
//! HTTPS-only for public hosts, plain HTTP allowed only for localhost
//! development endpoints, no userinfo, and no private/reserved IP targets.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use crate::stub::LlmError;

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

/// Validate a provider base URL.
///
/// Empty URLs are accepted. Non-empty URLs must use HTTP(S), have a host,
/// contain no userinfo, and must not target private/reserved IP space.
/// Plain HTTP is allowed only for localhost: `http://localhost:11434/v1`.
pub fn validate_base_url(url: &str) -> Result<(), LlmError> {
    if url.is_empty() {
        return Ok(());
    }

    let parsed =
        url::Url::parse(url).map_err(|e| LlmError::Other(format!("invalid base_url: {}", e)))?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(LlmError::Other(format!(
            "scheme '{}' not allowed — only http:// and https:// are permitted",
            scheme
        )));
    }

    let host_str = match parsed.host_str() {
        Some(h) if !h.is_empty() => h,
        _ => {
            return Err(LlmError::Other(
                "base_url has scheme but no host".to_string(),
            ))
        }
    };

    if scheme == "http" && host_str != "localhost" && host_str != "127.0.0.1" && host_str != "::1" {
        return Err(LlmError::Other(
            "HTTP is only allowed for localhost — use HTTPS for remote hosts".to_string(),
        ));
    }

    if parsed.username() != "" || parsed.password().is_some() {
        return Err(LlmError::Other(
            "base_url contains userinfo which is not allowed".to_string(),
        ));
    }

    // Reject IP literals that are private/reserved regardless of scheme.
    let host_clean = host_str.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host_clean.parse::<IpAddr>() {
        if is_private_or_reserved(ip) {
            return Err(LlmError::Other(format!(
                "base_url targets private/reserved IP {}",
                ip
            )));
        }
    }

    // DNS-based check: resolve hostname and reject if it points to private IP space.
    if !host_str.ends_with(".local")
        && host_str != "localhost"
        && host_str != "127.0.0.1"
        && host_str != "::1"
    {
        if let Ok(addrs) = ToSocketAddrs::to_socket_addrs(&format!("{}:443", host_str)) {
            for addr in addrs {
                if is_private_or_reserved(addr.ip()) {
                    return Err(LlmError::Other(format!(
                        "hostname '{}' resolves to private/reserved IP {}",
                        host_str,
                        addr.ip()
                    )));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::{ANTHROPIC_BASE_URL, OPENAI_BASE_URL};

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
    fn reject_https_loopback_ipv4() {
        assert!(validate_base_url("https://127.0.0.1/v1").is_err());
    }

    #[test]
    fn reject_https_loopback_ipv6() {
        assert!(validate_base_url("https://[::1]/v1").is_err());
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
        assert!(validate_base_url(OPENAI_BASE_URL).is_ok());
        assert!(validate_base_url(ANTHROPIC_BASE_URL).is_ok());
    }

    #[test]
    fn allow_empty_url() {
        assert!(validate_base_url("").is_ok());
    }

    #[test]
    fn reject_bad_scheme() {
        assert!(validate_base_url("ftp://example.com/v1").is_err());
    }

    #[test]
    fn reject_empty_host_after_scheme() {
        // url crate parses "https:///v1" as host="v1" path="/" — valid.
        // Use a non-parseable URL to exercise the "no host" path.
        assert!(validate_base_url("https://").is_err());
    }
}
