// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use jsonwebtoken::{
    Algorithm, DecodingKey, Validation,
    jwk::{AlgorithmParameters, Jwk, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse},
};
use serde::Deserialize;

use super::{
    Secret,
    format::Account,
    oauth::{ISSUER, TokenEndpoint},
};
use crate::LlmError;

pub(super) async fn verify(
    token: &str,
    client_id: &str,
    nonce: &str,
    endpoint: &dyn TokenEndpoint,
) -> Result<Account, LlmError> {
    let header = jsonwebtoken::decode_header(token).map_err(|_| invalid("invalid JWT header"))?;
    if header.alg != Algorithm::RS256 {
        return Err(invalid("only RS256 ID tokens are accepted"));
    }
    let kid = header
        .kid
        .filter(|kid| !kid.is_empty())
        .ok_or_else(|| invalid("missing key ID"))?;
    let uri = endpoint.discovery().await?;
    let mut keys = endpoint.jwks(&uri).await?;
    if !keys
        .keys
        .iter()
        .any(|key| key.common.key_id.as_deref() == Some(&kid))
    {
        keys = endpoint.jwks(&uri).await?;
    }
    verify_with_keys(token, client_id, nonce, &kid, &keys)
}

fn verify_with_keys(
    token: &str,
    client_id: &str,
    nonce: &str,
    kid: &str,
    keys: &JwkSet,
) -> Result<Account, LlmError> {
    let mut matching = keys
        .keys
        .iter()
        .filter(|key| key.common.key_id.as_deref() == Some(kid));
    let key = matching.next().ok_or_else(|| invalid("unknown key ID"))?;
    if matching.next().is_some() {
        return Err(invalid("ambiguous key ID"));
    }
    validate_key(key)?;
    let decoding = DecodingKey::from_jwk(key).map_err(|_| invalid("invalid RSA key"))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.leeway = 5;
    validation.validate_exp = true;
    validation.validate_nbf = true;
    validation.set_required_spec_claims(&["iss", "aud", "sub", "exp"]);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    let claims = jsonwebtoken::decode::<Claims>(token, &decoding, &validation)
        .map_err(|_| invalid("signature or claims validation failed"))?
        .claims;
    if claims.sub.0.is_empty() || claims.email.is_empty() || claims.nonce.0 != nonce {
        return Err(invalid("invalid identity or nonce"));
    }
    Ok(Account {
        subject: claims.sub,
        email: claims.email,
    })
}

fn validate_key(key: &Jwk) -> Result<(), LlmError> {
    if !matches!(key.algorithm, AlgorithmParameters::RSA(_))
        || key
            .common
            .key_algorithm
            .is_some_and(|alg| alg != KeyAlgorithm::RS256)
        || key
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|usage| *usage != PublicKeyUse::Signature)
        || key
            .common
            .key_operations
            .as_ref()
            .is_some_and(|ops| !ops.contains(&KeyOperations::Verify))
    {
        return Err(invalid("key is not an RS256 signing key"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Claims {
    #[serde(rename = "iss")]
    _issuer: Secret,
    sub: Secret,
    nonce: Secret,
    email: String,
    #[serde(rename = "exp")]
    _expiry: u64,
    #[serde(rename = "nbf", default)]
    _not_before: Option<u64>,
}

fn invalid(reason: &str) -> LlmError {
    LlmError::Credential {
        message: format!("ChatGPT ID token: {reason}"),
    }
}

#[cfg(test)]
pub(super) mod tests;
