// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::auth::oauth::{
    CodeGrant, ExchangeFailure, LoginTokens, RefreshFailure, RefreshGrant, TokenReply,
};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

// 测试私钥在本地生成，只保护本地签名与声明规则，不代表 OpenAI 凭据。
pub(in crate::auth) fn keys() -> JwkSet {
    serde_json::from_str(include_str!("test-jwks.json")).unwrap()
}

pub(in crate::auth) fn claims(client: &str, nonce: &str) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({ "iss": ISSUER, "aud": client, "sub": "local-subject", "email": "local@example.test",
        "nonce": nonce, "exp": now + 3600, "nbf": now - 60 })
}

pub(in crate::auth) fn signed(claims: &Value) -> String {
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

impl TokenEndpoint for KeyEndpoint {
    fn exchange<'a>(
        &'a self,
        _: CodeGrant<'a>,
    ) -> BoxFuture<'a, Result<LoginTokens, ExchangeFailure>> {
        unreachable!()
    }
    fn refresh<'a>(
        &'a self,
        _: RefreshGrant<'a>,
    ) -> BoxFuture<'a, Result<TokenReply, RefreshFailure>> {
        unreachable!()
    }
    fn revoke<'a>(&'a self, _: RefreshGrant<'a>) -> BoxFuture<'a, Result<(), LlmError>> {
        unreachable!()
    }
    fn discovery(&self) -> BoxFuture<'_, Result<url::Url, LlmError>> {
        Box::pin(async { Ok(url::Url::parse("https://auth.openai.com/local-test-jwks").unwrap()) })
    }
    fn jwks<'a>(&'a self, _: &'a url::Url) -> BoxFuture<'a, Result<JwkSet, LlmError>> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.replies.lock().unwrap().remove(0))
        })
    }
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
            verify(
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
            verify(&token, "local-client", "local-nonce", &endpoint)
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
        verify(
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
