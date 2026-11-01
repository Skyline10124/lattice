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
            Protocol::Responses => {
                set(&mut next.base_input, &["input_tokens"])?;
                set(&mut next.base_output, &["output_tokens"])?;
                set(
                    &mut next.cache_read,
                    &["input_tokens_details", "cached_tokens"],
                )?;
                set(
                    &mut next.cache_write,
                    &["input_tokens_details", "cache_write_tokens"],
                )?;
                set(
                    &mut next.reasoning,
                    &["output_tokens_details", "reasoning_tokens"],
                )?;
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
            Protocol::Interactions => {
                set(&mut next.base_input, &["total_input_tokens"])?;
                set(&mut next.tool_input, &["total_tool_use_tokens"])?;
                set(&mut next.cache_read, &["total_cached_tokens"])?;
                set(&mut next.base_output, &["total_output_tokens"])?;
                set(&mut next.reasoning, &["total_thought_tokens"])?;
            }
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
        final_snapshot.project(protocol)
    }

    fn project(&self, protocol: Protocol) -> Result<Option<TokenUsage>, ProjectionError> {
        let sum = |base: Option<u64>, parts: &[Option<u64>]| {
            let Some(base) = base else {
                return Ok(None);
            };
            parts
                .iter()
                .try_fold(base, |sum, value| {
                    sum.checked_add(value.unwrap_or(0)).ok_or_else(|| {
                        ProjectionError::MalformedUsage {
                            provider: protocol,
                            detail: "normalized usage exceeds u64".into(),
                        }
                    })
                })
                .map(Some)
        };
        let usage = TokenUsage {
            input: match protocol {
                Protocol::Messages => sum(self.base_input, &[self.cache_read, self.cache_write])?,
                Protocol::GenerateContent | Protocol::Interactions => {
                    sum(self.base_input, &[self.tool_input])?
                }
                _ => self.base_input,
            },
            cache_read: self.cache_read.or(self.cache_read_alias),
            cache_write: self.cache_write.or(self.cache_write_alias),
            output: if matches!(protocol, Protocol::GenerateContent | Protocol::Interactions) {
                sum(self.base_output, &[self.reasoning])?
            } else {
                self.base_output
            },
            reasoning: self.reasoning,
        }
        .normalized();
        Ok(usage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_base_counters_keep_totals_unknown() {
        for (name, protocol, wire, expected) in [
            (
                "interactions parts without bases",
                Protocol::Interactions,
                json!({"usage":{"total_tool_use_tokens":2,"total_thought_tokens":3}}),
                TokenUsage {
                    reasoning: Some(3),
                    ..Default::default()
                },
            ),
            (
                "messages cache without input",
                Protocol::Messages,
                json!({"usage":{"cache_read_input_tokens":7}}),
                TokenUsage {
                    cache_read: Some(7),
                    ..Default::default()
                },
            ),
            (
                "gemini tool input without prompt",
                Protocol::GenerateContent,
                json!({"usageMetadata":{"toolUsePromptTokenCount":2}}),
                TokenUsage::default(),
            ),
            (
                "gemini thoughts without candidates before interruption",
                Protocol::GenerateContent,
                json!({"usageMetadata":{"thoughtsTokenCount":3}}),
                TokenUsage {
                    reasoning: Some(3),
                    ..Default::default()
                },
            ),
        ] {
            assert_eq!(
                UsageSnapshot::default().update(protocol, &wire).unwrap(),
                expected.normalized(),
                "{name}"
            );
        }
    }

    #[test]
    fn interactions_updates_are_atomic_cumulative_and_keep_explicit_zero() {
        let protocol = Protocol::Interactions;
        let mut snapshot = UsageSnapshot::default();
        let full = json!({"usage":{"total_input_tokens":10,"total_tool_use_tokens":2,"total_cached_tokens":4,"total_output_tokens":5,"total_thought_tokens":3}});
        let expected = Some(TokenUsage {
            input: Some(12),
            cache_read: Some(4),
            cache_write: None,
            output: Some(8),
            reasoning: Some(3),
        });
        assert_eq!(snapshot.update(protocol, &full).unwrap(), expected);
        assert_eq!(snapshot.update(protocol, &full).unwrap(), expected);
        for invalid in [
            json!({"usage":[]}),
            json!({"usage":{"total_input_tokens":11,"total_thought_tokens":-1}}),
            json!({"usage":{"total_cached_tokens":0.5}}),
            json!({"usage":{"total_tool_use_tokens":"2"}}),
            json!({"usage":{"total_input_tokens":u64::MAX,"total_tool_use_tokens":1}}),
            json!({"usage":{"total_output_tokens":u64::MAX,"total_thought_tokens":1}}),
            serde_json::from_str(r#"{"usage":{"total_input_tokens":18446744073709551616}}"#)
                .unwrap(),
        ] {
            assert!(snapshot.update(protocol, &invalid).is_err(), "{invalid}");
            assert_eq!(snapshot.finish(protocol).unwrap(), expected);
        }
        assert_eq!(snapshot.update(protocol,&json!({"usage":{"total_input_tokens":null,"total_output_tokens":0,"total_cached_tokens":0}})).unwrap(),Some(TokenUsage { input:Some(12),cache_read:Some(0),cache_write:None,output:Some(3),reasoning:Some(3) }));
        assert_eq!(UsageSnapshot::default().finish(protocol).unwrap(), None);
    }
}
