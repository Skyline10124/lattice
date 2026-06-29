// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP network policy configuration for `fetch.web`.

use lancet_protocol::error::{PermissionErrorData, ValidationErrorData};
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability};
use net_guard as security;
use std::net::IpAddr;
use url::Url;

/// Immutable network policy derived from profile `[lancet.network]` and safe defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPolicyConfig {
    pub allow_public_https: bool,
    pub allow_localhost: bool,
    pub allowed_hosts: Vec<String>,
    pub max_web_response_bytes: usize,
}

impl NetworkPolicyConfig {
    /// Safe built-in defaults when `[lancet.network]` is absent.
    pub fn safe_defaults() -> Self {
        Self {
            allow_public_https: true,
            allow_localhost: false,
            allowed_hosts: Vec::new(),
            max_web_response_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Parsed URL that passed CEP network policy checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedFetchUrl {
    pub url: Url,
    pub host: String,
    pub port: u16,
    pub use_localhost_client: bool,
}

const METADATA_IPV4: &str = "169.254.169.254";
const METADATA_HOSTS: &[&str] = &["metadata", "metadata.google.internal"];

fn is_localhost_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

fn is_cloud_metadata_host(host: &str) -> bool {
    let host_lower = host.to_ascii_lowercase();
    METADATA_HOSTS
        .iter()
        .any(|name| host_lower == *name || host_lower.ends_with(&format!(".{name}")))
}

fn is_cloud_metadata_ip(ip: IpAddr) -> bool {
    matches!(
        ip,
        IpAddr::V4(v4) if v4.to_string() == METADATA_IPV4
    )
}

fn host_allowed_by_list(host: &str, allowed_hosts: &[String]) -> bool {
    let host_lower = host.to_ascii_lowercase();
    allowed_hosts.iter().any(|pattern| {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        if pattern.starts_with('.') {
            let suffix = pattern.trim_start_matches('.').to_ascii_lowercase();
            host_lower == suffix || host_lower.ends_with(&format!(".{suffix}"))
        } else {
            host_lower == pattern.to_ascii_lowercase()
        }
    })
}

fn network_denied(message: impl Into<String>) -> CepError {
    let message = message.into();
    CepError::new(
        CepErrorCode::AmbientAuthorityDenied,
        message.clone(),
        CepErrorData::Permission(PermissionErrorData {
            required_capability: "FetchUrl".into(),
            active_envelope_id: None,
            granted_capabilities: Vec::new(),
        }),
        Retriability::Permanent,
    )
}

fn url_validation_error(reason: impl Into<String>) -> CepError {
    let reason = reason.into();
    CepError::new(
        CepErrorCode::TypeError,
        reason.clone(),
        CepErrorData::Validation(ValidationErrorData {
            field: "url".into(),
            reason,
        }),
        Retriability::Permanent,
    )
}

/// Validate `url` against CEP network policy before any network I/O.
pub fn validate_fetch_url(
    url_str: &str,
    policy: &NetworkPolicyConfig,
) -> Result<ValidatedFetchUrl, CepError> {
    let parsed =
        Url::parse(url_str).map_err(|err| url_validation_error(format!("invalid URL: {err}")))?;
    let scheme = parsed.scheme();
    let host = parsed
        .host_str()
        .ok_or_else(|| url_validation_error("URL must include a host"))?
        .to_string();
    let port = parsed
        .port_or_known_default()
        .unwrap_or(if scheme == "http" { 80 } else { 443 });

    if security::host_is_dns_rebinding_domain(&host) {
        return Err(network_denied(format!(
            "URL not allowed: DNS rebinding domain '{host}'"
        )));
    }

    if is_cloud_metadata_host(&host) {
        return Err(network_denied(format!(
            "URL not allowed: cloud metadata host '{host}'"
        )));
    }

    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        if is_cloud_metadata_ip(ip) {
            return Err(network_denied(
                "URL not allowed: cloud metadata IP 169.254.169.254",
            ));
        }
        if security::is_private_or_reserved_ip(ip) && !is_localhost_host(&host) {
            return Err(network_denied(format!(
                "URL not allowed: private or reserved IP {ip}"
            )));
        }
    }

    let localhost = is_localhost_host(&host);
    let explicitly_allowed = host_allowed_by_list(&host, &policy.allowed_hosts);

    if localhost {
        if !policy.allow_localhost {
            return Err(network_denied(
                "URL not allowed: localhost access requires allow_localhost=true",
            ));
        }
        if !explicitly_allowed {
            return Err(network_denied(format!(
                "URL not allowed: localhost host '{host}' must appear in allowed_hosts"
            )));
        }
    } else if !policy.allowed_hosts.is_empty() && !explicitly_allowed {
        return Err(network_denied(format!(
            "URL not allowed: host '{host}' is not in allowed_hosts"
        )));
    }

    match scheme {
        "http" => {
            if !localhost {
                return Err(network_denied(
                    "URL not allowed: only https:// is permitted for non-localhost hosts",
                ));
            }
            Ok(ValidatedFetchUrl {
                url: parsed,
                host,
                port,
                use_localhost_client: true,
            })
        }
        "https" => {
            if !localhost {
                let public_https_ok = policy.allow_public_https && policy.allowed_hosts.is_empty();
                if !explicitly_allowed && !public_https_ok {
                    return Err(network_denied(
                        "URL not allowed: host is not granted by allow_public_https or allowed_hosts",
                    ));
                }
                if let Ok(ip) = host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<IpAddr>()
                    && security::is_private_or_reserved_ip(ip)
                {
                    return Err(network_denied(format!(
                        "URL not allowed: private or reserved IP {ip}"
                    )));
                }
            }
            Ok(ValidatedFetchUrl {
                url: parsed,
                host,
                port,
                use_localhost_client: localhost,
            })
        }
        _ => Err(network_denied(format!(
            "URL not allowed: scheme '{scheme}' is not http(s)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_policy_denies_localhost_by_default() {
        let policy = NetworkPolicyConfig::safe_defaults();
        let err = validate_fetch_url("http://localhost:8080/", &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::AmbientAuthorityDenied);
    }

    #[test]
    fn network_policy_localhost_requires_allowlist_entry() {
        let policy = NetworkPolicyConfig {
            allow_localhost: true,
            ..NetworkPolicyConfig::safe_defaults()
        };
        let err = validate_fetch_url("http://127.0.0.1:8080/path", &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::AmbientAuthorityDenied);
    }

    #[test]
    fn network_policy_allows_localhost_with_matching_allowlist() {
        let policy = NetworkPolicyConfig {
            allow_localhost: true,
            allowed_hosts: vec!["127.0.0.1".into()],
            ..NetworkPolicyConfig::safe_defaults()
        };
        let validated = validate_fetch_url("http://127.0.0.1:8080/path", &policy).unwrap();
        assert!(validated.use_localhost_client);
    }

    #[test]
    fn network_policy_allowlist_only_public_https() {
        let policy = NetworkPolicyConfig {
            allow_public_https: false,
            allowed_hosts: vec!["api.example.com".into()],
            ..NetworkPolicyConfig::safe_defaults()
        };
        validate_fetch_url("https://other.example/", &policy).unwrap_err();
        validate_fetch_url("https://api.example.com/", &policy).unwrap();
    }

    #[test]
    fn network_policy_denies_private_ip_literal() {
        let policy = NetworkPolicyConfig::safe_defaults();
        let err = validate_fetch_url("https://10.0.0.1/", &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::AmbientAuthorityDenied);
    }

    #[test]
    fn network_policy_denies_metadata_ip() {
        let policy = NetworkPolicyConfig::safe_defaults();
        let err =
            validate_fetch_url("https://169.254.169.254/latest/meta-data/", &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::AmbientAuthorityDenied);
    }

    #[test]
    fn network_policy_denies_non_http_scheme() {
        let policy = NetworkPolicyConfig::safe_defaults();
        let err = validate_fetch_url("ftp://example.com/", &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::AmbientAuthorityDenied);
    }

    #[test]
    fn network_policy_allows_public_https_host() {
        let policy = NetworkPolicyConfig::safe_defaults();
        let validated = validate_fetch_url("https://example.com/path", &policy).unwrap();
        assert!(!validated.use_localhost_client);
        assert_eq!(validated.host, "example.com");
    }

    #[test]
    fn network_policy_allowed_hosts_narrows_access() {
        let policy = NetworkPolicyConfig {
            allowed_hosts: vec!["allowed.example".into()],
            ..NetworkPolicyConfig::safe_defaults()
        };
        validate_fetch_url("https://other.example/", &policy).unwrap_err();
        validate_fetch_url("https://allowed.example/", &policy).unwrap();
    }
}
