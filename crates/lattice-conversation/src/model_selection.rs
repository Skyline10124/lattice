// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 共享的模型选择值：provider 标识、模型标识与完整模型引用。

use std::{borrow::Borrow, fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// provider 标识的最大 UTF-8 字节数。
pub const MAX_PROVIDER_ID_BYTES: usize = 64;

/// 模型标识的最大 UTF-8 字节数。
pub const MAX_MODEL_ID_BYTES: usize = 4_096;

/// provider 标识不符合语法规则的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProviderIdError {
    #[error("provider id must not be empty")]
    Empty,
    #[error("provider id must be at most {MAX_PROVIDER_ID_BYTES} bytes")]
    TooLong,
    #[error("provider id must start with [a-z0-9]")]
    InvalidStart,
    #[error("provider id must contain only [a-z0-9-]")]
    InvalidCharacter,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn parse(value: &str) -> Result<Self, ProviderIdError> {
        let Some(first) = value.as_bytes().first() else {
            return Err(ProviderIdError::Empty);
        };
        if value.len() > MAX_PROVIDER_ID_BYTES {
            return Err(ProviderIdError::TooLong);
        }
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(ProviderIdError::InvalidStart);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(ProviderIdError::InvalidCharacter);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for ProviderId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for ProviderId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ProviderId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ProviderId {
    type Err = ProviderIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for ProviderId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// 模型标识不符合语法规则的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ModelIdError {
    #[error("model id must not be empty")]
    Empty,
    #[error("model id must be at most {MAX_MODEL_ID_BYTES} bytes")]
    TooLong,
    #[error("model id must not contain control characters")]
    ControlCharacter,
}

/// 不透明的远端模型标识；保留空格、Unicode 与后续 `/`，仅拒绝空串、超长与控制字符。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    pub fn parse(value: &str) -> Result<Self, ModelIdError> {
        if value.is_empty() {
            return Err(ModelIdError::Empty);
        }
        if value.len() > MAX_MODEL_ID_BYTES {
            return Err(ModelIdError::TooLong);
        }
        if value.chars().any(char::is_control) {
            return Err(ModelIdError::ControlCharacter);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for ModelId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for ModelId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ModelId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ModelId {
    type Err = ModelIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for ModelId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// `<provider>/<model>` 引用解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ModelRefError {
    #[error("model reference must be <provider>/<model>")]
    MissingSeparator,
    #[error(transparent)]
    InvalidProvider(#[from] ProviderIdError),
    #[error(transparent)]
    InvalidModel(#[from] ModelIdError),
}

/// 只在第一个 `/` 拆分的完整模型引用。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelRef {
    provider: ProviderId,
    model: ModelId,
}

impl ModelRef {
    pub fn new(provider: ProviderId, model: ModelId) -> Self {
        Self { provider, model }
    }

    pub fn parse(value: &str) -> Result<Self, ModelRefError> {
        let (provider, model) = value
            .split_once('/')
            .ok_or(ModelRefError::MissingSeparator)?;
        Ok(Self::new(
            ProviderId::parse(provider)?,
            ModelId::parse(model)?,
        ))
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn model(&self) -> &ModelId {
        &self.model
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

impl FromStr for ModelRef {
    type Err = ModelRefError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for ModelRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_bounds_alphabet_and_serde() {
        for valid in ["a".into(), "0".into(), "a-b-9-".into(), "a".repeat(64)] {
            let parsed = ProviderId::parse(&valid).unwrap();
            assert_eq!(parsed.as_str(), valid);
            assert_eq!(
                serde_json::from_value::<ProviderId>(serde_json::to_value(&parsed).unwrap())
                    .unwrap(),
                parsed
            );
        }
        for (invalid, expected) in [
            ("".into(), ProviderIdError::Empty),
            ("-a".into(), ProviderIdError::InvalidStart),
            ("A".into(), ProviderIdError::InvalidStart),
            ("é".into(), ProviderIdError::InvalidStart),
            ("aB".into(), ProviderIdError::InvalidCharacter),
            ("a_b".into(), ProviderIdError::InvalidCharacter),
            ("a.b".into(), ProviderIdError::InvalidCharacter),
            ("a b".into(), ProviderIdError::InvalidCharacter),
            ("aé".into(), ProviderIdError::InvalidCharacter),
            ("a/".into(), ProviderIdError::InvalidCharacter),
            ("a".repeat(65), ProviderIdError::TooLong),
        ] {
            assert_eq!(ProviderId::parse(&invalid), Err(expected), "{invalid:?}");
            assert!(
                serde_json::from_value::<ProviderId>(serde_json::json!(invalid)).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn model_id_bounds_characters_and_serde() {
        for valid in [
            "m".into(),
            " 模型 /suffix".into(),
            "anthropic/claude-sonnet-4.6".into(),
            "é".repeat(MAX_MODEL_ID_BYTES / 2),
        ] {
            let parsed = ModelId::parse(&valid).unwrap();
            assert_eq!(parsed.as_str(), valid);
            assert_eq!(
                serde_json::from_value::<ModelId>(serde_json::to_value(&parsed).unwrap()).unwrap(),
                parsed
            );
        }
        for (invalid, expected) in [
            ("".into(), ModelIdError::Empty),
            ("x".repeat(MAX_MODEL_ID_BYTES + 1), ModelIdError::TooLong),
            (
                format!("{}a", "é".repeat(MAX_MODEL_ID_BYTES / 2)),
                ModelIdError::TooLong,
            ),
            ("m\0".into(), ModelIdError::ControlCharacter),
            ("m\n".into(), ModelIdError::ControlCharacter),
            ("m\u{7f}".into(), ModelIdError::ControlCharacter),
            ("m\u{85}".into(), ModelIdError::ControlCharacter),
        ] {
            assert_eq!(ModelId::parse(&invalid), Err(expected), "{invalid:?}");
            assert!(
                serde_json::from_value::<ModelId>(serde_json::json!(invalid)).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn model_ref_splits_first_slash_and_roundtrips_wire_string() {
        let source = "openrouter/anthropic/claude-sonnet-4.6";
        let parsed = ModelRef::parse(source).unwrap();
        assert_eq!(parsed.provider().as_str(), "openrouter");
        assert_eq!(parsed.model().as_str(), "anthropic/claude-sonnet-4.6");
        assert_eq!(parsed.to_string(), source);
        assert_eq!(
            ModelRef::new(parsed.provider().clone(), parsed.model().clone()),
            parsed
        );
        let wire = serde_json::to_value(&parsed).unwrap();
        assert_eq!(wire, serde_json::json!(source));
        assert_eq!(serde_json::from_value::<ModelRef>(wire).unwrap(), parsed);

        let max_provider = "a".repeat(MAX_PROVIDER_ID_BYTES);
        let max_model = "é".repeat(MAX_MODEL_ID_BYTES / 2);
        let max_ref = format!("{max_provider}/{max_model}");
        assert_eq!(
            max_ref.len(),
            MAX_PROVIDER_ID_BYTES + 1 + MAX_MODEL_ID_BYTES
        );
        assert!(ModelRef::parse(&max_ref).is_ok());

        for (invalid, expected) in [
            ("".into(), ModelRefError::MissingSeparator),
            ("openai".into(), ModelRefError::MissingSeparator),
            (
                "/model".into(),
                ModelRefError::InvalidProvider(ProviderIdError::Empty),
            ),
            (
                "bad_id/model".into(),
                ModelRefError::InvalidProvider(ProviderIdError::InvalidCharacter),
            ),
            (
                format!("{}a/model", max_provider),
                ModelRefError::InvalidProvider(ProviderIdError::TooLong),
            ),
            (
                "openai/".into(),
                ModelRefError::InvalidModel(ModelIdError::Empty),
            ),
            (
                "openai/m\0".into(),
                ModelRefError::InvalidModel(ModelIdError::ControlCharacter),
            ),
            (
                format!("{max_ref}a"),
                ModelRefError::InvalidModel(ModelIdError::TooLong),
            ),
        ] {
            assert_eq!(ModelRef::parse(&invalid), Err(expected), "{invalid:?}");
            assert!(
                serde_json::from_value::<ModelRef>(serde_json::json!(invalid)).is_err(),
                "{invalid:?}"
            );
        }
    }
}
