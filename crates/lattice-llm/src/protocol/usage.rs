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
    base_output: Option<u64>,
    reasoning: Option<u64>,
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
                    &mut next.reasoning,
                    &["completion_tokens_details", "reasoning_tokens"],
                )?;
            }
            Protocol::Messages => {
                set(&mut next.base_input, &["input_tokens"])?;
                set(&mut next.base_output, &["output_tokens"])?;
                set(&mut next.cache_read, &["cache_read_input_tokens"])?;
                set(&mut next.cache_write, &["cache_creation_input_tokens"])?;
            }
            Protocol::GenerateContent => {
                set(&mut next.base_input, &["promptTokenCount"])?;
                set(&mut next.tool_input, &["toolUsePromptTokenCount"])?;
                set(&mut next.base_output, &["candidatesTokenCount"])?;
                set(&mut next.reasoning, &["thoughtsTokenCount"])?;
                set(&mut next.cache_read, &["cachedContentTokenCount"])?;
            }
            _ => return Err(ProjectionError::UnsupportedProtocol { protocol }),
        }
        let sum = |parts: &[Option<u64>]| -> Result<Option<u64>, ProjectionError> {
            parts.iter().flatten().try_fold(None, |sum, value| {
                sum.unwrap_or(0_u64)
                    .checked_add(*value)
                    .map(Some)
                    .ok_or_else(|| malformed("normalized usage exceeds u64"))
            })
        };
        let usage = TokenUsage {
            input: match protocol {
                Protocol::Messages => sum(&[next.base_input, next.cache_read, next.cache_write])?,
                Protocol::GenerateContent => sum(&[next.base_input, next.tool_input])?,
                _ => next.base_input,
            },
            cache_read: next.cache_read.or(next.cache_read_alias),
            cache_write: next.cache_write,
            output: if protocol == Protocol::GenerateContent {
                sum(&[next.base_output, next.reasoning])?
            } else {
                next.base_output
            },
            reasoning: next.reasoning,
        }
        .normalized();
        *self = next;
        Ok(usage)
    }
}
