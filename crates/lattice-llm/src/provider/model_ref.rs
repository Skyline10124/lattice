// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{fmt, str::FromStr};

pub(crate) const MAX_PROVIDER_ID_BYTES: usize = 64;
pub(crate) const MAX_MODEL_ID_BYTES: usize = 4_096;

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ModelRefError(pub(crate) &'static str);

pub(crate) fn validate_provider_id(id: &str) -> Result<(), ModelRefError> {
    if id.is_empty()
        || id.len() > MAX_PROVIDER_ID_BYTES
        || !id.as_bytes()[0].is_ascii_lowercase() && !id.as_bytes()[0].is_ascii_digit()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(ModelRefError(
            "provider id must be 1–64 characters, start with [a-z0-9], and contain only [a-z0-9-]",
        ));
    }
    Ok(())
}

pub(crate) fn validate_model_id(model: &str) -> Result<(), ModelRefError> {
    if model.is_empty() || model.len() > MAX_MODEL_ID_BYTES || model.chars().any(char::is_control) {
        return Err(ModelRefError(
            "model id must be nonempty, at most 4096 bytes, and contain no control characters",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRef {
    provider: String,
    model: String,
}

impl ModelRef {
    pub fn parse(value: &str) -> Result<Self, ModelRefError> {
        let (provider, model) = value
            .split_once('/')
            .ok_or(ModelRefError("model reference must be <provider>/<model>"))?;
        validate_provider_id(provider)?;
        validate_model_id(model)?;
        Ok(Self {
            provider: provider.to_owned(),
            model: model.to_owned(),
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

impl FromStr for ModelRef {
    type Err = ModelRefError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.provider, self.model)
    }
}
