// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::auth::format::Account;
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

// 测试私钥在本地生成，只保护本地签名与声明规则，不代表 OpenAI 凭据。
pub(in crate::auth::siwc) fn keys() -> JwkSet {
    serde_json::from_str(include_str!("test-jwks.json")).unwrap()
}

pub(in crate::auth::siwc) fn claims(client: &str, nonce: &str) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({ "iss": ISSUER, "aud": client, "sub": "local-subject", "email": "local@example.test",
        "nonce": nonce, "exp": now + 3600, "nbf": now - 60 })
}

pub(in crate::auth::siwc) fn signed(claims: &Value) -> String {
    let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
    header.kid = Some("local-rsa".into());
    jsonwebtoken::encode(
        &header,
        claims,
        &jsonwebtoken::EncodingKey::from_rsa_der(include_bytes!("test-key.der")),
    )
    .unwrap()
}

fn check(claims: &Value, keys: &JwkSet) -> Result<Account, LlmError> {
    verify_with_keys(
        &signed(claims),
        "local-client",
        "local-nonce",
        "local-rsa",
        keys,
    )
}

// 将端点私有的 verify_login 转发给登录流程测试，避免扩大生产可见性。
pub(in crate::auth::siwc) async fn verify_login<F, Fut>(
    login: UnverifiedLogin,
    client_id: &str,
    nonce: &str,
    discovery: impl Future<Output = Result<url::Url, LlmError>>,
    jwks: F,
) -> Result<(Account, format::Tokens), LlmError>
where
    F: FnMut(url::Url) -> Fut,
    Fut: Future<Output = Result<JwkSet, LlmError>>,
{
    super::verify_login(login, client_id, nonce, discovery, jwks).await
}

#[tokio::test]
async fn login_verification_propagates_failure_before_returning_identity() {
    let signed = signed(&claims("local-client", "local-nonce"));
    for (id_token, nonce, succeeds) in [
        (signed.as_str(), "local-nonce", true),
        (signed.as_str(), "wrong-nonce", false),
        ("invalid-jwt-header", "local-nonce", false),
    ] {
        let login = super::tests::exchange_response(
            &json!({
                "access_token": "new-access-secret", "refresh_token": "new-refresh-secret",
                "token_type": "Bearer", "expires_in": 3600,
                "scope": DIRECT_SCOPE, "id_token": id_token,
            }),
            Utc::now(),
        )
        .unwrap_or_else(|_| panic!("合法换码回复应返回待验签材料"));
        let result = super::verify_login(
            login,
            "local-client",
            nonce,
            async { Ok(url::Url::parse("https://auth.openai.com/local-test-jwks").unwrap()) },
            |_| async { Ok(keys()) },
        )
        .await;
        if succeeds {
            let (account, tokens) = result.unwrap();
            assert_eq!(account.subject.0, "local-subject");
            assert_eq!(account.email, "local@example.test");
            assert_eq!(tokens.id_token.0, id_token);
            assert_eq!(tokens.access_token.0, "new-access-secret");
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn validates_signed_identity_and_string_or_array_audience() {
    let mut value = claims("local-client", "local-nonce");
    for audience in [json!("local-client"), json!(["other", "local-client"])] {
        value["aud"] = audience;
        let account = check(&value, &keys()).unwrap();
        assert_eq!(account.subject.0, "local-subject");
        assert_eq!(account.email, "local@example.test");
    }
    value.as_object_mut().unwrap().remove("nbf");
    assert!(check(&value, &keys()).is_ok());
}

#[test]
fn rejects_missing_wrong_or_malformed_claims_without_echoing_them() {
    let valid = claims("local-client", "local-nonce");
    for field in ["iss", "aud", "sub", "exp", "nonce", "email"] {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(field);
        assert!(check(&value, &keys()).is_err(), "{field}");
    }
    for (field, value) in [
        ("iss", json!("secret-wrong-issuer")),
        ("iss", json!([ISSUER])),
        ("aud", json!("secret-wrong-audience")),
        ("aud", json!([])),
        ("sub", json!("")),
        ("nonce", json!("secret-wrong-nonce")),
        ("email", json!("")),
        ("exp", json!("secret-expiry")),
        ("exp", json!(1.5)),
        ("exp", json!(-1)),
        ("nbf", json!("secret-nbf")),
    ] {
        let mut candidate = valid.clone();
        candidate[field] = value;
        let error = check(&candidate, &keys()).err().unwrap();
        assert!(!error.to_string().contains("secret-"));
        assert!(!format!("{error:?}").contains("local-nonce"));
    }
}

#[test]
fn library_time_validation_enforces_expiry_nbf_and_small_leeway() {
    let now = chrono::Utc::now().timestamp();
    let mut value = claims("local-client", "local-nonce");
    value["exp"] = json!(now - 1);
    assert!(check(&value, &keys()).is_ok());
    value["exp"] = json!(now - 30);
    assert!(check(&value, &keys()).is_err());
    value["exp"] = json!(now + 3600);
    value["nbf"] = json!(now + 1);
    assert!(check(&value, &keys()).is_ok());
    value["nbf"] = json!(now + 30);
    assert!(check(&value, &keys()).is_err());
}

#[test]
fn rejects_non_rs256_or_inappropriate_jwk() {
    let value = claims("local-client", "local-nonce");
    for algorithm in [Algorithm::RS384, Algorithm::PS256, Algorithm::HS256] {
        let mut header = jsonwebtoken::Header::new(algorithm);
        header.kid = Some("local-rsa".into());
        let key = if algorithm == Algorithm::HS256 {
            jsonwebtoken::EncodingKey::from_secret(b"local-test-secret")
        } else {
            jsonwebtoken::EncodingKey::from_rsa_der(include_bytes!("test-key.der"))
        };
        let token = jsonwebtoken::encode(&header, &value, &key).unwrap();
        assert!(
            verify_with_keys(&token, "local-client", "local-nonce", "local-rsa", &keys()).is_err()
        );
    }
    for (field, value) in [
        ("alg", json!("RS384")),
        ("use", json!("enc")),
        ("key_ops", json!(["sign"])),
    ] {
        let mut jwks = serde_json::to_value(keys()).unwrap();
        jwks["keys"][0][field] = value;
        assert!(
            check(
                &claims("local-client", "local-nonce"),
                &serde_json::from_value(jwks).unwrap()
            )
            .is_err()
        );
    }
    let symmetric: Jwk =
        serde_json::from_value(json!({"kty":"oct","k":"c2VjcmV0","kid":"local-rsa","alg":"RS256"}))
            .unwrap();
    assert!(
        check(
            &value,
            &JwkSet {
                keys: vec![symmetric]
            }
        )
        .is_err()
    );
    let mut duplicate = keys();
    duplicate.keys.push(duplicate.keys[0].clone());
    assert!(check(&value, &duplicate).is_err());
    let mut no_alg = keys();
    no_alg.keys[0].common.key_algorithm = None;
    assert!(check(&value, &no_alg).is_ok());
}

struct KeyEndpoint {
    replies: Mutex<Vec<JwkSet>>,
    calls: AtomicUsize,
}

async fn verify_with_endpoint(
    token: &str,
    client_id: &str,
    nonce: &str,
    endpoint: &KeyEndpoint,
) -> Result<Account, LlmError> {
    verify(
        token,
        client_id,
        nonce,
        async { Ok(url::Url::parse("https://auth.openai.com/local-test-jwks").unwrap()) },
        |_| async {
            endpoint.calls.fetch_add(1, Ordering::SeqCst);
            Ok(endpoint.replies.lock().unwrap().remove(0))
        },
    )
    .await
}

#[tokio::test]
async fn unknown_kid_reloads_once_and_known_key_failure_does_not_reload() {
    for found in [true, false] {
        let endpoint = KeyEndpoint {
            replies: Mutex::new(vec![
                JwkSet { keys: vec![] },
                if found {
                    keys()
                } else {
                    JwkSet { keys: vec![] }
                },
            ]),
            calls: AtomicUsize::new(0),
        };
        assert_eq!(
            verify_with_endpoint(
                &signed(&claims("local-client", "local-nonce")),
                "local-client",
                "local-nonce",
                &endpoint
            )
            .await
            .is_ok(),
            found
        );
        assert_eq!(endpoint.calls.load(Ordering::SeqCst), 2);
    }
    let token = signed(&claims("local-client", "local-nonce"));
    let mut parts: Vec<_> = token.split('.').map(str::to_owned).collect();
    let mut signature = parts[2].as_bytes().to_vec();
    signature[0] = if signature[0] == b'A' { b'B' } else { b'A' };
    parts[2] = String::from_utf8(signature).unwrap();
    for token in [
        signed(&claims("wrong-client", "local-nonce")),
        parts.join("."),
    ] {
        let endpoint = KeyEndpoint {
            replies: Mutex::new(vec![keys(), keys()]),
            calls: AtomicUsize::new(0),
        };
        assert!(
            verify_with_endpoint(&token, "local-client", "local-nonce", &endpoint)
                .await
                .is_err()
        );
        assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
    }
    let endpoint = KeyEndpoint {
        replies: Mutex::new(vec![]),
        calls: AtomicUsize::new(0),
    };
    assert!(
        verify_with_endpoint(
            "eyJhbGciOiJub25lIn0.e30.",
            "local-client",
            "local-nonce",
            &endpoint
        )
        .await
        .is_err()
    );
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 0);
}
