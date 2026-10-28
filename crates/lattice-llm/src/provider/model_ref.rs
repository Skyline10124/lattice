// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{fmt, str::FromStr};

pub(crate) const MAX_MODEL_ID_BYTES: usize = 4_096;

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ModelRefError(pub(crate) String);

pub(crate) fn validate_model_id(model: &str) -> Result<(), ModelRefError> {
    if model.is_empty() || model.len() > MAX_MODEL_ID_BYTES || model.chars().any(char::is_control) {
        return Err(ModelRefError(
            "model id must be nonempty, at most 4096 bytes, and contain no control characters"
                .into(),
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
            .ok_or_else(|| ModelRefError("model reference must be <provider>/<model>".into()))?;
        lattice_conversation::validate_provider_id(provider)
            .map_err(|error| ModelRefError(error.to_string()))?;
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
