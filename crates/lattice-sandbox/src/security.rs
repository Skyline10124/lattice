// NOTE: Duplicated from lattice-core/src/security.rs to maintain leaf crate property (no internal deps).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

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

/// Resolve a host and reject it if any returned address is private/reserved.
///
/// DNS lookup failures are treated as validation errors for fetch-capable
/// tools. Otherwise a hostname that cannot be checked could still be used as
/// an SSRF target at request time.
pub fn reject_private_resolved_host(host: &str, port: u16) -> Result<(), String> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if is_private_ip(host) {
        return Err(format!("host '{}' is private or reserved", host));
    }
    if host.ends_with(".local") {
        return Err(format!("host '{}' is a local network name", host));
    }

    let addrs = ToSocketAddrs::to_socket_addrs(&(host, port))
        .map_err(|e| format!("DNS lookup failed for '{}': {}", host, e))?;
    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        if is_private_or_reserved(addr.ip()) {
            return Err(format!(
                "host '{}' resolves to private/reserved IP {}",
                host,
                addr.ip()
            ));
        }
    }

    if !resolved_any {
        return Err(format!("DNS lookup for '{}' returned no addresses", host));
    }
    Ok(())
}
