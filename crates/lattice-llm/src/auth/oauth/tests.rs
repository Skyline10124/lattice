// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::BTreeMap;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

fn response() -> serde_json::Value {
    serde_json::json!({
        "access_token": "new-access-secret", "refresh_token": "new-refresh-secret",
        "token_type": "Bearer", "expires_in": 3600,
        "scope": "openid chatgpt.tokens.use.direct",
        "id_token": "ignored-id-secret"
    })
}

#[test]
fn grants_use_issued_client_and_encode_reserved_characters_without_scope() {
    let grant = RefreshGrant {
        client_id: "issued+client&=",
        refresh_token: "refresh+secret&=",
    };
    let decoded = |form: &[u8]| {
        url::form_urlencoded::parse(form)
            .into_owned()
            .collect::<BTreeMap<_, _>>()
    };
    let refresh = decoded(&refresh_form(&grant));
    assert_eq!(refresh.len(), 4);
    assert_eq!(refresh["grant_type"], "refresh_token");
    assert!(refresh["client_id"] == grant.client_id);
    assert!(refresh["refresh_token"] == grant.refresh_token);
    assert_eq!(refresh["resource"], "https://api.openai.com/v1");
    let revoke = decoded(&revocation_form(&grant));
    assert_eq!(revoke.len(), 3);
    assert_eq!(revoke["token_type_hint"], "refresh_token");
    assert!(revoke["client_id"] == grant.client_id && revoke["token"] == grant.refresh_token);
}

#[test]
fn token_response_validates_material_expiry_and_permission_while_ignoring_id_token() {
    let value = response();
    for (expires_in, expected) in [
        (3600, "2027-01-15T09:00:00+00:00"),
        (937, "2027-01-15T08:15:37+00:00"),
    ] {
        for id_token in [
            Some(value["id_token"].clone()),
            Some(serde_json::json!({"unexpected": "ignored-id-secret"})),
            None,
        ] {
            let mut sample = value.clone();
            sample["expires_in"] = serde_json::json!(expires_in);
            if let Some(id_token) = id_token {
                sample["id_token"] = id_token;
            } else {
                sample.as_object_mut().unwrap().remove("id_token");
            }
            let parsed = parse_token(&serde_json::to_vec(&sample).unwrap(), now()).unwrap();
            assert!(parsed.access_token.0 == "new-access-secret");
            assert!(parsed.refresh_token.0 == "new-refresh-secret");
            assert_eq!(parsed.expires_at, expected);
            assert_eq!(parsed.scopes, ["openid", DIRECT_SCOPE]);
        }
    }
    for (field, invalid) in [
        ("access_token", serde_json::json!("")),
        ("access_token", serde_json::json!("bad\nsecret")),
        ("access_token", serde_json::json!("bad secret")),
        ("refresh_token", serde_json::json!("")),
        ("refresh_token", serde_json::json!(null)),
        ("token_type", serde_json::json!("Basic")),
        ("scope", serde_json::json!("openid")),
        ("expires_in", serde_json::json!(0)),
        ("expires_in", serde_json::json!(-1)),
        ("expires_in", serde_json::json!(0.5)),
        ("expires_in", serde_json::json!(u64::MAX)),
    ] {
        let mut value = value.clone();
        value[field] = invalid;
        let error = parse_token(&serde_json::to_vec(&value).unwrap(), now())
            .err()
            .unwrap();
        assert!(!error.retryable(), "{field}");
        for secret in [
            "new-access-secret",
            "new-refresh-secret",
            "ignored-id-secret",
        ] {
            assert!(!format!("{error:?} {error}").contains(secret));
        }
    }
}

#[test]
fn oauth_error_codes_are_exact_and_do_not_echo_error_descriptions() {
    for code in [
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
    ] {
        for error in [serde_json::json!(code), serde_json::json!({"code": code})] {
            let reply = HttpReply {
                status: 400,
                headers: HeaderMap::new(),
                body: Zeroizing::new(
                    serde_json::to_vec(&serde_json::json!({"error":error})).unwrap(),
                ),
            };
            assert!(
                matches!(refresh_error(&reply, now()), RefreshFailure::InvalidToken(found) if found == code)
            );
        }
    }
    for (status, code, retryable) in [
        (400, "invalid_client", false),
        (503, "invalid_client", false),
        (503, "temporarily_unavailable", true),
        (429, "unknown", true),
        (401, "refresh_token_reused_extra", false),
        (400, "unknown", false),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "3".parse().unwrap());
        headers.insert("x-should-retry", "true".parse().unwrap());
        let reply = HttpReply {
            status,
            headers,
            body: Zeroizing::new(
                serde_json::to_vec(&serde_json::json!({
                    "error": code, "error_description": "refresh-secret and arbitrary-secret"
                }))
                .unwrap(),
            ),
        };
        let RefreshFailure::Other(error) = refresh_error(&reply, now()) else {
            panic!("unexpected terminal error")
        };
        assert_eq!(error.status_code(), Some(status));
        assert_eq!(error.retryable(), retryable);
        assert_eq!(error.retry_after(), Some(Duration::from_secs(3)));
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
    let reply = HttpReply {
        status: 401,
        headers: HeaderMap::new(),
        body: Zeroizing::new(b"invalid_grant secret".to_vec()),
    };
    assert!(matches!(
        refresh_error(&reply, now()),
        RefreshFailure::Other(_)
    ));
}

#[test]
fn refresh_request_ids_redact_all_overlapping_credentials_together() {
    use crate::auth::tests::{chatgpt, fixture, write};

    for field in ["refresh_token", "access_token", "api_key"] {
        let (_directory, store) = fixture();
        let client_id = "oaiapp_test-issued-client";
        let refresh_token = format!("{client_id}.refresh-secret");
        let access_token = format!("{refresh_token}.access-secret");
        let api_key = format!("{access_token}.api-key-secret");
        let mut entry = chatgpt();
        entry["client_id"] = serde_json::json!(client_id);
        entry["tokens"]["refresh_token"] = serde_json::json!(refresh_token);
        entry["tokens"]["access_token"] = serde_json::json!(access_token);
        write(
            &store,
            serde_json::json!({"version":1,"providers":{
                "chatgpt":entry,"deepseek":{"type":"api_key","key":api_key}
            }})
            .to_string(),
        );
        let request_id = match field {
            "refresh_token" => &refresh_token,
            "access_token" => &access_token,
            _ => &api_key,
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", request_id.parse().unwrap());
        let reply = HttpReply {
            status: 503,
            headers,
            body: Zeroizing::new(br#"{"error":"temporarily_unavailable"}"#.to_vec()),
        };
        let RefreshFailure::Other(error) = refresh_error(&reply, now()) else {
            panic!("unexpected terminal error")
        };
        let error = crate::client::redact_error(error, &store.load().unwrap());
        for diagnostic in [error.to_string(), format!("{error:?}")] {
            assert!(diagnostic.contains("request_id=[REDACTED]"));
            for secret in [
                client_id,
                ".refresh-secret",
                ".access-secret",
                ".api-key-secret",
            ] {
                assert!(!diagnostic.contains(secret), "{field}");
            }
        }
    }
}

#[test]
fn revocation_discovery_only_accepts_the_trusted_https_origin() {
    for (endpoint, valid) in [
        ("https://auth.openai.com/oauth/revoke", true),
        ("https://auth.openai.com:443/oauth/revoke", true),
        ("http://auth.openai.com/oauth/revoke", false),
        ("https://auth.openai.com.evil.test/revoke", false),
        ("https://127.0.0.1/revoke", false),
        ("https://auth.openai.com:444/revoke", false),
        ("https://secret@auth.openai.com/revoke", false),
        ("https://auth.openai.com/revoke?token=secret", false),
        ("https://auth.openai.com/revoke#secret", false),
    ] {
        let body = serde_json::to_vec(
            &serde_json::json!({"issuer": ISSUER, "revocation_endpoint": endpoint}),
        )
        .unwrap();
        assert_eq!(
            revocation_endpoint(&body).is_ok(),
            valid,
            "discovery policy"
        );
    }
    assert!(revocation_endpoint(br#"{"issuer":"https://evil.test","revocation_endpoint":"https://auth.openai.com/revoke"}"#).is_err());
    for (status, body, success) in [
        (200, b"".as_slice(), true),
        (200, b"{}".as_slice(), false),
        (204, b"".as_slice(), false),
        (500, b"".as_slice(), false),
    ] {
        assert_eq!(confirm_revocation(status, body).is_ok(), success);
    }
}

#[tokio::test]
async fn interrupted_response_bodies_are_retryable_but_invalid_json_is_not() {
    for kind in [
        std::io::ErrorKind::ConnectionReset,
        std::io::ErrorKind::UnexpectedEof,
    ] {
        let body = reqwest::Body::wrap_stream(futures::stream::iter([
            Ok(bytes::Bytes::from_static(br#"{"access_token":"partial"#)),
            Err(std::io::Error::new(kind, "interrupted synthetic body")),
        ]));
        let response = reqwest::Response::from(http::Response::new(body));
        let error = read_reply(response).await.err().unwrap();
        assert!(
            matches!(
                error,
                LlmError::Transport {
                    retryable: true,
                    ..
                }
            ),
            "{kind:?}"
        );
    }
    let response = reqwest::Response::from(http::Response::new("invalid JSON"));
    let reply = read_reply(response).await.unwrap();
    let error = parse_token(&reply.body, now()).err().unwrap();
    assert!(!error.retryable());
}

#[tokio::test]
async fn oauth_without_roots_fails_before_any_network_request() {
    let endpoint = HttpTokenEndpoint {
        roots: vec![],
        clock: Arc::new(now),
    };
    let grant = RefreshGrant {
        client_id: "client-secret",
        refresh_token: "refresh-secret",
    };
    assert!(matches!(
        endpoint.refresh(grant).await,
        Err(RefreshFailure::Other(LlmError::Config { .. }))
    ));
    let grant = RefreshGrant {
        client_id: "client-secret",
        refresh_token: "refresh-secret",
    };
    assert!(endpoint.revoke(grant).await.is_err());
}
