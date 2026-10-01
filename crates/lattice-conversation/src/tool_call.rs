// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! ToolCall typestates and strict batch validation (ADR 0005 §7).
//!
//! Tool arguments must be exactly one JSON object. Empty strings, arrays,
//! scalars, malformed JSON, and duplicate/incomplete Tool calls are invalid;
//! there is no empty-to-`{}` normalization. If any ToolCall in a completed
//! provider batch is invalid, the entire attempt is rejected
//! (`AttemptRejection::InvalidToolCall`) and produces no invocation slot,
//! `InvocationId`, or conversation-tree content — this validator is the
//! fail-closed gate that decision is built from.

use lancet_protocol::{InvocationId, ToolCallAddress, arguments_digest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::limits::{MAX_TOOL_CALL_ARGUMENTS_BYTES, MAX_TOOL_CALLS_PER_BATCH};
use crate::message::{ToolCallState, sealed};

/// A Tool call as produced by a provider before binding: the optional
/// provider-native call ID plus canonical semantics. Never carries a LANCET
/// identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnboundToolCall {
    /// The provider-native Tool Call ID, preserved verbatim when the provider
    /// supplies one; never synthesized.
    pub provider_call_id: Option<String>,
    /// The Tool method name.
    pub name: String,
    /// The canonical arguments object (always `Value::Object`; execution uses
    /// this canonical object, not the provider's textual spelling).
    pub arguments: Value,
}

impl sealed::Sealed for UnboundToolCall {}
impl ToolCallState for UnboundToolCall {
    fn name(&self) -> &str {
        &self.name
    }

    fn arguments(&self) -> &Value {
        &self.arguments
    }

    fn provider_call_id(&self) -> Option<&str> {
        self.provider_call_id.as_deref()
    }

    fn clear_provider_call_id(&mut self) {
        self.provider_call_id = None;
    }
}
/// A Tool call bound to its canonical `InvocationId` and structural address.
///
/// Deserialization is checked: `arguments_digest` is recomputed from
/// `arguments` and must match the stored value byte for byte. A payload whose
/// digest does not describe its own arguments is rejected rather than silently
/// accepted, so a `BoundToolCall` value always carries a digest that really
/// validates its binding (ADR 0005 §7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "BoundToolCallWire")]
pub struct BoundToolCall {
    /// The canonical invocation identity (ADR 0005 §1).
    pub invocation_id: InvocationId,
    /// The provider-native Tool Call ID, preserved verbatim when supplied;
    /// excluded from reservation, authorization, and digest semantics.
    pub provider_call_id: Option<String>,
    /// The Tool method name.
    pub name: String,
    /// The canonical arguments object.
    pub arguments: Value,
    /// SHA-256 over the domain separator, explicit byte length, and canonical
    /// argument bytes (see [`crate::arguments_digest`]). Hex-encoded on the
    /// wire; fixed `[u8; 32]` in memory.
    #[serde(with = "hex_digest")]
    pub arguments_digest: [u8; 32],
    /// The structural pre-commit address of this call's part.
    pub address: ToolCallAddress,
}

/// Unchecked deserialization shim for [`BoundToolCall`]. Identical field names
/// and serde shape; the digest precondition is enforced in `TryFrom`.
#[derive(Deserialize)]
struct BoundToolCallWire {
    invocation_id: InvocationId,
    #[serde(default)]
    provider_call_id: Option<String>,
    name: String,
    arguments: Value,
    #[serde(with = "hex_digest")]
    arguments_digest: [u8; 32],
    address: ToolCallAddress,
}

/// The stored `arguments_digest` does not describe the stored `arguments`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("arguments_digest does not match the canonical digest of arguments")]
pub struct ArgumentsDigestMismatch;

impl TryFrom<BoundToolCallWire> for BoundToolCall {
    type Error = ArgumentsDigestMismatch;

    fn try_from(wire: BoundToolCallWire) -> Result<Self, Self::Error> {
        if arguments_digest(&wire.arguments) != wire.arguments_digest {
            return Err(ArgumentsDigestMismatch);
        }
        Ok(Self {
            invocation_id: wire.invocation_id,
            provider_call_id: wire.provider_call_id,
            name: wire.name,
            arguments: wire.arguments,
            arguments_digest: wire.arguments_digest,
            address: wire.address,
        })
    }
}

impl sealed::Sealed for BoundToolCall {}
impl ToolCallState for BoundToolCall {
    fn name(&self) -> &str {
        &self.name
    }

    fn arguments(&self) -> &Value {
        &self.arguments
    }

    fn provider_call_id(&self) -> Option<&str> {
        self.provider_call_id.as_deref()
    }

    fn clear_provider_call_id(&mut self) {
        self.provider_call_id = None;
    }
}

impl UnboundToolCall {
    /// Bind this call to its invocation identity and structural address,
    /// computing the canonical arguments digest.
    pub fn bind(self, invocation_id: InvocationId, address: ToolCallAddress) -> BoundToolCall {
        BoundToolCall {
            invocation_id,
            provider_call_id: self.provider_call_id,
            name: self.name,
            arguments_digest: arguments_digest(&self.arguments),
            arguments: self.arguments,
            address,
        }
    }
}

/// A raw provider Tool call as assembled from the wire, before validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawToolCall {
    /// Provider-native call ID, when the wire carries one.
    pub provider_call_id: Option<String>,
    /// The Tool method name.
    pub name: String,
    /// The provider's textual arguments spelling.
    pub arguments: String,
}

/// 工具调用批次的校验失败原因。一项调用无效即拒绝整批；
/// 若该批次来自完整的模型响应，则拒绝该次模型请求尝试（ADR 0005 §7）。
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallValidation {
    /// The batch exceeds the code-defined per-step ToolCall count.
    #[error("tool call batch too large: limit {limit}, got {actual}")]
    TooManyCalls {
        /// Maximum accepted calls in one batch.
        limit: usize,
        /// Actual calls in the completed batch.
        actual: usize,
    },
    /// The textual arguments are not well-formed JSON (including the empty
    /// string; there is no empty-to-`{}` normalization).
    #[error("tool call at part ordinal {call_ordinal} has malformed JSON arguments")]
    MalformedArguments {
        /// Ordinal of the offending call in the batch.
        call_ordinal: usize,
    },
    /// The arguments parsed but are not a JSON object (array, scalar, null).
    #[error("tool call at part ordinal {call_ordinal} arguments are not a JSON object")]
    ArgumentsNotObject {
        /// Ordinal of the offending call in the batch.
        call_ordinal: usize,
    },
    /// The textual arguments exceed the per-call protocol limit.
    #[error(
        "tool call at part ordinal {call_ordinal} arguments too large: limit {limit} bytes, got {actual}"
    )]
    ArgumentsTooLarge {
        /// Ordinal of the offending call in the batch.
        call_ordinal: usize,
        /// Protocol limit in bytes.
        limit: usize,
        /// Actual byte length.
        actual: usize,
    },
    /// The call has no Tool method name.
    #[error("tool call at part ordinal {call_ordinal} has no method name")]
    MissingToolName {
        /// Ordinal of the offending call in the batch.
        call_ordinal: usize,
    },
    /// The wire carried an empty call ID where an ID is spelled.
    #[error("tool call at part ordinal {call_ordinal} has an empty provider call id")]
    EmptyToolCallId {
        /// Ordinal of the offending call in the batch.
        call_ordinal: usize,
    },
    /// Two calls in one batch carry the same provider call ID.
    #[error("duplicate provider call id at part ordinals {first_ordinal} and {second_ordinal}")]
    DuplicateToolCallId {
        /// Ordinal of the first occurrence.
        first_ordinal: usize,
        /// Ordinal of the duplicate.
        second_ordinal: usize,
    },
    /// 调用的工具不在本次 Turn 的启用目录中。
    ///
    /// 启用目录由运行时提供，因此此拒绝由绑定规划器在分配 InvocationId 前产生，
    /// 而不是由 `validate_tool_call_batch` 的协议校验产生。
    /// 仅保存调用序号，避免把没有独立长度限制的模型工具名写入持久记录和公开结果。
    /// 本实现用于 #218 止血，不代表 #220 的调用状态重设计。
    #[error("tool call at part ordinal {call_ordinal} names an unknown tool")]
    UnknownTool {
        /// 被拒绝调用在消息部件中的序号。
        call_ordinal: usize,
    },
}

/// Validate a complete provider ToolCall batch, all or nothing.
///
/// Returns the ordered unbound calls with canonical arguments objects. Any
/// invalid call fails the whole batch with a typed reason; the caller must
/// treat the completed attempt as rejected and must not produce partial
/// conversation from it.
pub fn validate_tool_call_batch(
    calls: &[RawToolCall],
) -> Result<Vec<UnboundToolCall>, ToolCallValidation> {
    if calls.len() > MAX_TOOL_CALLS_PER_BATCH {
        return Err(ToolCallValidation::TooManyCalls {
            limit: MAX_TOOL_CALLS_PER_BATCH,
            actual: calls.len(),
        });
    }
    let mut validated = Vec::with_capacity(calls.len());
    for (ordinal, call) in calls.iter().enumerate() {
        if call.arguments.len() > MAX_TOOL_CALL_ARGUMENTS_BYTES {
            return Err(ToolCallValidation::ArgumentsTooLarge {
                call_ordinal: ordinal,
                limit: MAX_TOOL_CALL_ARGUMENTS_BYTES,
                actual: call.arguments.len(),
            });
        }
        if call.name.is_empty() {
            return Err(ToolCallValidation::MissingToolName {
                call_ordinal: ordinal,
            });
        }
        if matches!(&call.provider_call_id, Some(id) if id.is_empty()) {
            return Err(ToolCallValidation::EmptyToolCallId {
                call_ordinal: ordinal,
            });
        }
        if let Some(id) = &call.provider_call_id
            && let Some(first) = validated.iter().position(|prior: &UnboundToolCall| {
                prior.provider_call_id.as_deref() == Some(id.as_str())
            })
        {
            return Err(ToolCallValidation::DuplicateToolCallId {
                first_ordinal: first,
                second_ordinal: ordinal,
            });
        }
        let arguments: Value = serde_json::from_str(&call.arguments).map_err(|_| {
            ToolCallValidation::MalformedArguments {
                call_ordinal: ordinal,
            }
        })?;
        if !arguments.is_object() {
            return Err(ToolCallValidation::ArgumentsNotObject {
                call_ordinal: ordinal,
            });
        }
        validated.push(UnboundToolCall {
            provider_call_id: call.provider_call_id.clone(),
            name: call.name.clone(),
            arguments,
        });
    }
    Ok(validated)
}

/// Hex serde for fixed digests: readable lowercase hex in persisted payloads,
/// fixed `[u8; 32]` in memory.
pub(crate) mod hex_digest {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(digest: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        serializer.serialize_str(&out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let raw = String::deserialize(deserializer)?;
        let bytes = raw.as_bytes();
        if bytes.len() != 64 {
            return Err(D::Error::custom(
                "digest must be 64 lowercase hex characters",
            ));
        }
        let mut digest = [0u8; 32];
        for (index, pair) in bytes.chunks_exact(2).enumerate() {
            let hi = hex_value(pair[0]).ok_or_else(|| D::Error::custom("non-hex digest"))?;
            let lo = hex_value(pair[1]).ok_or_else(|| D::Error::custom("non-hex digest"))?;
            digest[index] = (hi << 4) | lo;
        }
        Ok(digest)
    }

    fn hex_value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::{ModelMessageAddress, SessionId, TurnId};

    fn raw(name: &str, arguments: &str) -> RawToolCall {
        RawToolCall {
            provider_call_id: Some(format!("call_{name}")),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    fn address() -> ToolCallAddress {
        ToolCallAddress {
            message: ModelMessageAddress {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c")
                    .expect("session id"),
                turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("turn id"),
                model_step_ordinal: 0,
            },
            part_ordinal: 0,
        }
    }

    #[test]
    fn tool_call_count_exact_boundary_passes_plus_one_fails() {
        let exact: Vec<_> = (0..MAX_TOOL_CALLS_PER_BATCH)
            .map(|ordinal| raw(&format!("tool_{ordinal}"), "{}"))
            .collect();
        assert_eq!(
            validate_tool_call_batch(&exact)
                .expect("exact batch boundary")
                .len(),
            MAX_TOOL_CALLS_PER_BATCH
        );

        let mut over = exact;
        over.push(raw("one_too_many", "{}"));
        assert!(matches!(
            validate_tool_call_batch(&over),
            Err(ToolCallValidation::TooManyCalls {
                limit: MAX_TOOL_CALLS_PER_BATCH,
                actual,
            }) if actual == MAX_TOOL_CALLS_PER_BATCH + 1
        ));
    }

    #[test]
    fn valid_batch_canonicalizes_arguments() {
        let calls = vec![raw("read_file", r#"{ "b" : 2, "a" : 1 }"#)];
        let validated = validate_tool_call_batch(&calls).expect("valid batch");
        assert_eq!(validated.len(), 1);
        assert_eq!(
            lancet_protocol::canonical_json(&validated[0].arguments),
            r#"{"a":1,"b":2}"#
        );
    }

    #[test]
    fn tool_call_batch_validation_preserves_rejection_reasons() {
        struct Case {
            name: &'static str,
            calls: Vec<RawToolCall>,
            expected: fn(&Result<Vec<UnboundToolCall>, ToolCallValidation>) -> bool,
        }
        let arguments = format!(
            "{{\"a\":\"{}\"}}",
            "x".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES)
        );
        for Case {
            name,
            calls,
            expected,
        } in [
            // No empty-to-`{}` normalization: empty arguments reject the batch.
            Case {
                name: "empty_arguments",
                calls: vec![raw("read_file", "")],
                expected: |result| {
                    matches!(
                        result,
                        Err(ToolCallValidation::MalformedArguments { call_ordinal: 0 })
                    )
                },
            },
            Case {
                name: "empty_array",
                calls: vec![raw("read_file", "[]")],
                expected: |result| {
                    matches!(
                        result,
                        Err(ToolCallValidation::ArgumentsNotObject { call_ordinal: 0 })
                    )
                },
            },
            Case {
                name: "nonempty_array",
                calls: vec![raw("read_file", "[1]")],
                expected: |result| {
                    matches!(
                        result,
                        Err(ToolCallValidation::ArgumentsNotObject { call_ordinal: 0 })
                    )
                },
            },
            Case {
                name: "string_scalar",
                calls: vec![raw("read_file", "\"x\"")],
                expected: |result| {
                    matches!(
                        result,
                        Err(ToolCallValidation::ArgumentsNotObject { call_ordinal: 0 })
                    )
                },
            },
            Case {
                name: "number_scalar",
                calls: vec![raw("read_file", "42")],
                expected: |result| {
                    matches!(
                        result,
                        Err(ToolCallValidation::ArgumentsNotObject { call_ordinal: 0 })
                    )
                },
            },
            Case {
                name: "null_scalar",
                calls: vec![raw("read_file", "null")],
                expected: |result| {
                    matches!(
                        result,
                        Err(ToolCallValidation::ArgumentsNotObject { call_ordinal: 0 })
                    )
                },
            },
            Case {
                name: "boolean_scalar",
                calls: vec![raw("read_file", "true")],
                expected: |result| {
                    matches!(
                        result,
                        Err(ToolCallValidation::ArgumentsNotObject { call_ordinal: 0 })
                    )
                },
            },
            Case {
                name: "truncated_object",
                calls: vec![raw("read_file", "{")],
                expected: |result| {
                    matches!(result, Err(ToolCallValidation::MalformedArguments { .. }))
                },
            },
            Case {
                name: "missing_property_value",
                calls: vec![raw("read_file", "{\"a\":}")],
                expected: |result| {
                    matches!(result, Err(ToolCallValidation::MalformedArguments { .. }))
                },
            },
            Case {
                name: "invalid_token",
                calls: vec![raw("read_file", "not json")],
                expected: |result| {
                    matches!(result, Err(ToolCallValidation::MalformedArguments { .. }))
                },
            },
            Case {
                name: "whitespace_only",
                calls: vec![raw("read_file", "  ")],
                expected: |result| {
                    matches!(result, Err(ToolCallValidation::MalformedArguments { .. }))
                },
            },
            Case {
                name: "duplicate_id",
                calls: vec![
                    raw("read_file", "{\"path\":\"a\"}"),
                    RawToolCall {
                        provider_call_id: Some("call_read_file".to_string()),
                        ..raw("write_file", "{\"path\":\"b\"}")
                    },
                ],
                expected: |result| {
                    matches!(
                        result,
                        Err(ToolCallValidation::DuplicateToolCallId {
                            first_ordinal: 0,
                            second_ordinal: 1
                        })
                    )
                },
            },
            Case {
                name: "absent_id_is_allowed",
                calls: vec![RawToolCall {
                    provider_call_id: None,
                    ..raw("read_file", "{\"path\":\"a\"}")
                }],
                expected: |result| result.is_ok(),
            },
            Case {
                name: "explicit_empty_id",
                calls: vec![RawToolCall {
                    provider_call_id: Some("".to_string()),
                    ..raw("read_file", "{\"path\":\"a\"}")
                }],
                expected: |result| {
                    matches!(result, Err(ToolCallValidation::EmptyToolCallId { .. }))
                },
            },
            Case {
                name: "empty_tool_name",
                calls: vec![raw("", "{\"path\":\"a\"}")],
                expected: |result| {
                    matches!(result, Err(ToolCallValidation::MissingToolName { .. }))
                },
            },
            Case {
                name: "invalid_second_call",
                calls: vec![
                    raw("read_file", "{\"path\":\"a\"}"),
                    raw("write_file", "[]"),
                ],
                expected: |result| {
                    matches!(
                        result,
                        Err(ToolCallValidation::ArgumentsNotObject { call_ordinal: 1 })
                    )
                },
            },
            Case {
                name: "oversized_object_arguments",
                calls: vec![raw("read_file", &arguments)],
                expected: |result| {
                    matches!(result, Err(ToolCallValidation::ArgumentsTooLarge { .. }))
                },
            },
        ] {
            let result = validate_tool_call_batch(&calls);
            assert!(
                expected(&result),
                "{name}: unexpected validation result: {result:?}"
            );
        }
    }

    #[test]
    fn tampered_arguments_digest_is_rejected_on_deserialize() {
        // A digest that does not describe its own arguments must never
        // deserialize: the binding tuple would otherwise validate nothing.
        let bound = validate_tool_call_batch(&[raw("read_file", r#"{"path":"a"}"#)])
            .expect("valid")
            .into_iter()
            .next()
            .expect("one call")
            .bind(
                InvocationId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("invocation id"),
                address(),
            );

        let mut payload: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&bound).expect("serialize"))
                .expect("value");
        payload["arguments_digest"] = serde_json::Value::String("0".repeat(64));
        let error = serde_json::from_value::<BoundToolCall>(payload).expect_err("must reject");
        assert!(
            error.to_string().contains("arguments_digest"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn digest_consistent_payload_still_deserializes() {
        // The checked path must not reject legitimate payloads, including a
        // non-canonical key order in the stored arguments object.
        let bound = validate_tool_call_batch(&[raw("read_file", r#"{"b":2,"a":1}"#)])
            .expect("valid")
            .into_iter()
            .next()
            .expect("one call")
            .bind(
                InvocationId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("invocation id"),
                address(),
            );
        let json = serde_json::to_string(&bound).expect("serialize");
        let back: BoundToolCall = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(bound, back);
    }
}
