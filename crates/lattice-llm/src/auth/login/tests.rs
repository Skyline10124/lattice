// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn pkce_uses_s256_vector_and_host_uses_uuid_v4_uri() {
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let host = host_id().unwrap();
    let uuid = uuid::Uuid::parse_str(host.0.strip_prefix("urn:uuid:").unwrap()).unwrap();
    assert_eq!(uuid.get_version_num(), 4);
    assert_eq!(uuid.get_variant(), uuid::Variant::RFC4122);
    for _ in 0..3 {
        let first = random_secret().unwrap();
        let second = random_secret().unwrap();
        assert_eq!(first.0.len(), 43);
        assert!(
            first
                .0
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        );
        assert_ne!(first.0, second.0);
    }
}

#[test]
fn authorization_parameters_reuse_registration_and_hint_without_leaking_debug() {
    let mut credentials = Credentials(format::Document::default());
    credentials.0.providers.insert(
        "chatgpt".into(),
        format::Credential::Chatgpt {
            host_id: host_id().unwrap(),
            client_id: None,
            account: None,
            tokens: None,
        },
    );
    let pending = Pending::new(&credentials, "http://127.0.0.1:1455/auth/callback".into()).unwrap();
    let url = pending.authorization();
    let parsed = url::Url::parse(url.expose()).unwrap();
    let params: std::collections::BTreeMap<_, _> = parsed.query_pairs().collect();
    assert_eq!(params["client_id"], "dynamic_agent_client");
    assert_eq!(params["agent_name_hint"], "LATTICE");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["code_challenge"], challenge(&pending.verifier.0));
    assert_eq!(
        params["scope"],
        "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"
    );
    assert_eq!(params["resource"], "https://api.openai.com/v1");
    assert_eq!(params["redirect_uri"], pending.redirect);
    assert_eq!(params["ext_agent_host_id"], pending.host.0);
    assert_ne!(pending.state.0, pending.nonce.0);
    assert_ne!(pending.nonce.0, pending.verifier.0);
    let mut returning = pending;
    returning.client = Some(Secret("issued+client&value".into()));
    returning.hint = Some(Secret("retained.id.token".into()));
    for retained in [true, false] {
        if !retained {
            returning.hint = None;
        }
        let url = returning.authorization();
        let parsed = url::Url::parse(url.expose()).unwrap();
        let params: std::collections::BTreeMap<_, _> = parsed.query_pairs().collect();
        assert_eq!(params["client_id"], "issued+client&value");
        assert!(!params.contains_key("agent_name_hint"));
        assert_eq!(params.contains_key("id_token_hint"), retained);
    }
}

#[test]
fn in_flight_and_stored_secrets_are_sorted_together_before_bounding() {
    let mut document = format::Document::default();
    document.providers.insert(
        "openai".into(),
        format::Credential::ApiKey {
            key: Secret("short".into()),
        },
    );
    document.providers.insert(
        "chatgpt".into(),
        format::Credential::Chatgpt {
            host_id: host_id().unwrap(),
            client_id: None,
            account: None,
            tokens: None,
        },
    );
    let credentials = Credentials(document);
    let pending = Pending::new(&credentials, "http://127.0.0.1:1455/auth/callback".into()).unwrap();
    let error = pending.redact(
        LlmError::Other(format!(
            "short-complete-secret {} {}",
            pending.verifier.0,
            "x".repeat(2000)
        )),
        &credentials,
        &["short-complete-secret"],
    );
    let display = error.to_string();
    assert!(!display.contains("complete-secret"));
    assert!(!display.contains(&pending.verifier.0));
    assert!(display.contains("[REDACTED]"));
    assert!(display.chars().count() < 1100);
}
