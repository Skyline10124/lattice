use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use crate::errors::LatticeError;

/// Return true when an IP address is private, loopback, link-local, multicast,
/// unspecified, documentation-only, or otherwise unsafe for outbound model API
/// endpoints.
pub fn is_private_or_reserved(ip: IpAddr) -> bool {
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

/// Check if a host string is a literal private/link-local host.
///
/// This does not perform DNS resolution; use [`validate_base_url`] for endpoint
/// validation that resolves public hostnames and rejects DNS rebinding targets.
pub fn is_private_ip(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(is_private_or_reserved)
}

/// Validate a model provider base URL.
///
/// Empty URLs remain allowed for diagnostic and backward-compatible paths.
/// Non-empty URLs must use HTTP(S), must have a host, must not contain
/// userinfo, and must not resolve to private or reserved IP space. Plain HTTP
/// is accepted only for localhost development endpoints.
pub fn validate_base_url(url: &str) -> Result<(), LatticeError> {
    if url.is_empty() {
        return Ok(());
    }

    let parsed = url::Url::parse(url).map_err(|e| LatticeError::Config {
        message: format!("Invalid base_url '{}': {}", url, e),
    })?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(LatticeError::Config {
            message: format!(
                "Invalid base_url '{}': scheme '{}' is not allowed. Only http:// and https:// are permitted.",
                url, scheme
            ),
        });
    }

    let host = match parsed.host_str() {
        Some(h) if !h.is_empty() => h,
        _ => {
            return Err(LatticeError::Config {
                message: format!("Invalid base_url '{}': URL has scheme but no host", url),
            });
        }
    };

    if scheme == "http" && host != "localhost" && host != "127.0.0.1" && host != "::1" {
        return Err(LatticeError::Config {
            message: format!(
                "Insecure base_url '{}': HTTP is only allowed for localhost. Use HTTPS.",
                url
            ),
        });
    }

    if parsed.username() != "" || parsed.password().is_some() {
        return Err(LatticeError::Config {
            message: format!(
                "Invalid base_url '{}': URL contains userinfo which is not allowed",
                url
            ),
        });
    }

    if !host.ends_with(".local") && host != "localhost" && host != "127.0.0.1" && host != "::1" {
        if let Ok(addrs) = ToSocketAddrs::to_socket_addrs(&format!("{host}:443")) {
            for addr in addrs {
                if is_private_or_reserved(addr.ip()) {
                    return Err(LatticeError::Config {
                        message: format!(
                            "Insecure base_url '{}': hostname '{}' resolves to private/reserved IP {}. Use a public endpoint.",
                            url, host, addr.ip()
                        ),
                    });
                }
            }
        }
    }

    Ok(())
}
