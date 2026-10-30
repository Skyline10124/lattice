// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 保留协议的原生累计分项，再换算中立用量；缺失更新不擦除已知值。

use lattice_conversation::{Protocol, TokenUsage};
use serde_json::Value;

use super::projection::ProjectionError;

#[derive(Clone, Default)]
pub(super) struct UsageSnapshot {
    base_input: Option<u64>,
    tool_input: Option<u64>,
    cache_read: Option<u64>,
    cache_read_alias: Option<u64>,
    cache_write: Option<u64>,
    cache_write_alias: Option<u64>,
    base_output: Option<u64>,
    reasoning: Option<u64>,
    has_usage_metadata: bool,
}

impl UsageSnapshot {
    pub(super) fn update(
        &mut self,
        protocol: Protocol,
        wire: &Value,
    ) -> Result<Option<TokenUsage>, ProjectionError> {
        let key = if protocol == Protocol::GenerateContent {
            "usageMetadata"
        } else {
            "usage"
        };
        let Some(value) = wire.get(key).filter(|value| !value.is_null()) else {
            return Ok(None);
        };
        let malformed = |detail: &str| ProjectionError::MalformedUsage {
            provider: protocol,
            detail: detail.into(),
        };
        if !value.is_object() {
            return Err(malformed("usage must be an object"));
        }
        let field = |path: &[&str]| -> Result<Option<u64>, ProjectionError> {
            let mut value = value;
            for name in path {
                if value.is_null() {
                    return Ok(None);
                }
                if !value.is_object() {
                    return Err(malformed("usage details must be an object"));
                }
                let Some(next) = value.get(*name) else {
                    return Ok(None);
                };
                value = next;
            }
            if value.is_null() {
                Ok(None)
            } else {
                value
                    .as_u64()
                    .map(Some)
                    .ok_or_else(|| malformed("usage counter must be a u64 integer"))
            }
        };
        let mut next = self.clone();
        let set = |slot: &mut Option<u64>, path: &[&str]| -> Result<(), ProjectionError> {
            *slot = field(path)?.or(*slot);
            Ok(())
        };
        let set_input_side =
            |slot: &mut Option<u64>, path: &[&str]| -> Result<(), ProjectionError> {
                if let Some(value) = field(path)?
                    && (value != 0 || slot.is_none())
                {
                    *slot = Some(value);
                }
                Ok(())
            };
        match protocol {
            Protocol::ChatCompletions => {
                set(&mut next.base_input, &["prompt_tokens"])?;
                set(&mut next.base_output, &["completion_tokens"])?;
                set(
                    &mut next.cache_read,
                    &["prompt_tokens_details", "cached_tokens"],
                )?;
                set(&mut next.cache_read_alias, &["prompt_cache_hit_tokens"])?;
                set(
                    &mut next.cache_write,
                    &["prompt_tokens_details", "cache_creation_input_tokens"],
                )?;
                set(
                    &mut next.cache_write_alias,
                    &["prompt_tokens_details", "cache_write_tokens"],
                )?;
                set(
                    &mut next.reasoning,
                    &["completion_tokens_details", "reasoning_tokens"],
                )?;
            }
            Protocol::Messages => {
                set_input_side(&mut next.base_input, &["input_tokens"])?;
                set(&mut next.base_output, &["output_tokens"])?;
                set_input_side(&mut next.cache_read, &["cache_read_input_tokens"])?;
                set_input_side(&mut next.cache_write, &["cache_creation_input_tokens"])?;
            }
            Protocol::GenerateContent => {
                set(&mut next.base_input, &["promptTokenCount"])?;
                set(&mut next.tool_input, &["toolUsePromptTokenCount"])?;
                set(&mut next.base_output, &["candidatesTokenCount"])?;
                set(&mut next.reasoning, &["thoughtsTokenCount"])?;
                set(&mut next.cache_read, &["cachedContentTokenCount"])?;
                next.has_usage_metadata |= value
                    .as_object()
                    .is_some_and(|value| value.values().any(|field| !field.is_null()));
            }
            _ => return Err(ProjectionError::UnsupportedProtocol { protocol }),
        }
        let usage = next.project(protocol)?;
        *self = next;
        Ok(usage)
    }

    pub(super) fn finish(&self, protocol: Protocol) -> Result<Option<TokenUsage>, ProjectionError> {
        if protocol != Protocol::GenerateContent {
            return self.project(protocol);
        }
        if !self.has_usage_metadata {
            return Ok(None);
        }
        let mut final_snapshot = self.clone();
        final_snapshot.tool_input.get_or_insert(0);
        final_snapshot.base_output.get_or_insert(0);
        final_snapshot.reasoning.get_or_insert(0);
        final_snapshot.cache_read.get_or_insert(0);
        let mut usage = final_snapshot.project(protocol)?;
        if self.base_input.is_none()
            && let Some(usage) = &mut usage
        {
            usage.input = None;
        }
        Ok(usage)
    }

    fn project(&self, protocol: Protocol) -> Result<Option<TokenUsage>, ProjectionError> {
        let sum = |parts: &[Option<u64>]| -> Result<Option<u64>, ProjectionError> {
            parts.iter().flatten().try_fold(None, |sum, value| {
                sum.unwrap_or(0_u64)
                    .checked_add(*value)
                    .map(Some)
                    .ok_or_else(|| ProjectionError::MalformedUsage {
                        provider: protocol,
                        detail: "normalized usage exceeds u64".into(),
                    })
            })
        };
        let usage = TokenUsage {
            input: match protocol {
                Protocol::Messages => sum(&[self.base_input, self.cache_read, self.cache_write])?,
                Protocol::GenerateContent => sum(&[self.base_input, self.tool_input])?,
                _ => self.base_input,
            },
            cache_read: self.cache_read.or(self.cache_read_alias),
            cache_write: self.cache_write.or(self.cache_write_alias),
            output: if protocol == Protocol::GenerateContent {
                sum(&[self.base_output, self.reasoning])?
            } else {
                self.base_output
            },
            reasoning: self.reasoning,
        }
        .normalized();
        Ok(usage)
    }
}
