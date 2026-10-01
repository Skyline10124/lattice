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
    /// Safe defaults: no host is granted until `allowed_hosts` is configured.
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

/// Normalize one `allowed_hosts` entry (or an observed host) into its
/// comparison form: surrounding whitespace trimmed, ASCII-lowercased.
///
/// Normalization is idempotent, so already-normalized patterns (for example the
/// ones stored in `AuthorizedEffectRegion::hosts`) pass through unchanged.
pub fn normalize_host_pattern(pattern: &str) -> String {
    pattern.trim().to_ascii_lowercase()
}

/// Match a single normalized host against a single `allowed_hosts` pattern.
///
/// A leading dot means suffix match: `.example.com` matches the apex domain
/// `example.com` and any subdomain of it. Every other pattern is an exact host
/// match. An empty (or dots-only) pattern never matches — fail-closed.
fn host_matches_pattern(host_lower: &str, pattern: &str) -> bool {
    let pattern = normalize_host_pattern(pattern);
    if pattern.is_empty() {
        return false;
    }
    if pattern.starts_with('.') {
        let suffix = pattern.trim_start_matches('.');
        if suffix.is_empty() {
            return false;
        }
        host_lower == suffix || host_lower.ends_with(&format!(".{suffix}"))
    } else {
        host_lower == pattern
    }
}

/// The single host allow-matching implementation for the whole workspace.
///
/// Every network policy consumer must call this function so host matching cannot drift.
///
/// This function answers exactly one question: does any configured pattern
/// grant this host? An empty list matches nothing, and every enforcement layer
/// treats that as no host grant. For
/// `validate_fetch_url`, `allow_public_https` and `allow_localhost` are additional
/// scheme/target switches, never replacements for an explicit host pattern.
/// The network Tool Host uses the same matcher for the initial request and every
/// redirect hop. Empty lists stay fail-closed.
pub fn host_allowed_by_list(host: &str, allowed_hosts: &[String]) -> bool {
    let host_lower = normalize_host_pattern(host);
    allowed_hosts
        .iter()
        .any(|pattern| host_matches_pattern(&host_lower, pattern))
}

fn network_denied(message: impl Into<String>) -> CepError {
    let message = message.into();
    CepError::new(
        CepErrorCode::AmbientAuthorityDenied,
        message.clone(),
        CepErrorData::Permission(PermissionErrorData {
            required_capability: "FetchUrl".into(),
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
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(url_validation_error("URL userinfo is not allowed"));
    }
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
        if security::is_forbidden_tool_target_ip(ip) && !is_localhost_host(&host) {
            return Err(network_denied(format!(
                "URL not allowed: private or reserved IP {ip}"
            )));
        }
    }

    let localhost = is_localhost_host(&host);
    let explicitly_allowed = host_allowed_by_list(&host, &policy.allowed_hosts);

    if policy.allowed_hosts.is_empty() {
        return Err(network_denied("URL not allowed: allowed_hosts is empty"));
    }
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
    } else if !explicitly_allowed {
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
                if !policy.allow_public_https {
                    return Err(network_denied(
                        "URL not allowed: public HTTPS requires allow_public_https=true",
                    ));
                }
                if let Ok(ip) = host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<IpAddr>()
                    && security::is_forbidden_tool_target_ip(ip)
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
    fn network_policy_requires_public_https_master_switch() {
        let policy = NetworkPolicyConfig {
            allow_public_https: false,
            allowed_hosts: vec!["api.example.com".into()],
            ..NetworkPolicyConfig::safe_defaults()
        };
        validate_fetch_url("https://api.example.com/", &policy).unwrap_err();
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
        let policy = NetworkPolicyConfig {
            allowed_hosts: vec!["example.com".into()],
            ..NetworkPolicyConfig::safe_defaults()
        };
        let validated = validate_fetch_url("https://example.com/path", &policy).unwrap();
        assert!(!validated.use_localhost_client);
        assert_eq!(validated.host, "example.com");
    }

    #[test]
    fn network_policy_empty_allowlist_fails_closed() {
        let mut policy = NetworkPolicyConfig::safe_defaults();
        policy.allow_public_https = true;
        let error = validate_fetch_url("https://example.com/", &policy).unwrap_err();
        assert_eq!(error.code, CepErrorCode::AmbientAuthorityDenied);
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

    // ── #122: the one allow-matcher ────────────────────────────────────

    #[test]
    fn normalize_host_pattern_trims_and_lowercases_idempotently() {
        for (raw, expected) in [
            (" example.com ", "example.com"),
            ("Example.COM", "example.com"),
            ("\t.Example.com\n", ".example.com"),
            ("example.com", "example.com"),
        ] {
            let once = normalize_host_pattern(raw);
            assert_eq!(once, expected, "normalizing {raw:?}");
            assert_eq!(
                normalize_host_pattern(&once),
                once,
                "normalization must be idempotent for {raw:?}"
            );
        }
    }

    /// A leading dot is a suffix pattern that also matches the apex domain.
    #[test]
    fn leading_dot_pattern_matches_apex_and_subdomains() {
        let patterns = vec![".example.com".to_string()];
        for host in [
            "example.com",
            "api.example.com",
            "deep.api.example.com",
            "API.Example.COM",
        ] {
            assert!(
                host_allowed_by_list(host, &patterns),
                "'.example.com' must grant {host}"
            );
        }
        for host in ["notexample.com", "example.com.evil.tld", "example.org"] {
            assert!(
                !host_allowed_by_list(host, &patterns),
                "'.example.com' must not grant {host}"
            );
        }
    }

    #[test]
    fn non_canonical_pattern_forms_all_match_the_same_host() {
        for pattern in ["Example.com", " example.com ", "EXAMPLE.COM"] {
            assert!(
                host_allowed_by_list("example.com", &[pattern.to_string()]),
                "pattern {pattern:?} must grant example.com"
            );
        }
    }

    #[test]
    fn empty_and_dots_only_patterns_match_nothing() {
        for pattern in ["", " ", ".", "..", "\t"] {
            assert!(
                !host_allowed_by_list("example.com", &[pattern.to_string()]),
                "pattern {pattern:?} must be fail-closed"
            );
        }
        assert!(!host_allowed_by_list("example.com", &[]));
    }

    /// Non-canonical `allowed_hosts` entries produce the same verdict in URL
    /// validation and the shared matcher.
    #[test]
    fn validate_fetch_url_agrees_with_the_shared_matcher() {
        for pattern in [".example.com", "Example.com", " example.com "] {
            let policy = NetworkPolicyConfig {
                allow_public_https: true,
                allowed_hosts: vec![pattern.to_string()],
                ..NetworkPolicyConfig::safe_defaults()
            };
            let host = "example.com";
            let matcher_allows = host_allowed_by_list(host, &policy.allowed_hosts);
            let url_allows = validate_fetch_url("https://example.com/", &policy).is_ok();
            assert!(
                matcher_allows && url_allows,
                "pattern {pattern:?}: matcher={matcher_allows} url={url_allows}"
            );
        }
    }

    /// The subdomain a leading-dot pattern grants must also pass URL validation.
    #[test]
    fn validate_fetch_url_accepts_subdomain_of_leading_dot_pattern() {
        let policy = NetworkPolicyConfig {
            allow_public_https: true,
            allowed_hosts: vec![".example.com".into()],
            ..NetworkPolicyConfig::safe_defaults()
        };
        validate_fetch_url("https://api.example.com/v1", &policy).unwrap();
        validate_fetch_url("https://example.com/", &policy).unwrap();
        validate_fetch_url("https://example.org/", &policy).unwrap_err();
    }
}
