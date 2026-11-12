// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-conversation` 线上类型片段（全部 enum 穷尽片段与带可选/集合字段结构体片段）。

use lattice_conversation::{
    BoundToolCall, CostEstimate, CostSummary, Effort, IndeterminateCause, InvocationId,
    MessagePart, MessageRole, MessageSemantic, ModelMessageAddress, NativeMessage,
    OpaqueProviderState, Protocol, ReplayOrigin, ResultPayload, SuccessContent, TokenSummary,
    TokenUsage, ToolCallAddress, ToolCallValidation, ToolError, ToolResultOutcome, TurnId,
    UsageCompleteness,
};
use serde_json::json;

use super::{
    ARGS_CARGO_TOML_DIGEST_HEX, ARGS_MISSING_TXT_DIGEST_HEX, ARGS_SLEEP_CMD_DIGEST_HEX,
    CONTENT_LINES_42_BYTES, CONTENT_LINES_42_DIGEST_HEX, CONTENT_OVERSIZE_BYTES,
    CONTENT_OVERSIZE_DIGEST_HEX, CaseBook, hex_32, invocation_id_1, provider_openai,
    session_id_primary, turn_id_1,
};

pub(super) fn replay_origin_openai() -> ReplayOrigin {
    ReplayOrigin {
        provider_id: provider_openai(),
        protocol: Protocol::Responses,
    }
}

pub(super) fn opaque_state_sample() -> OpaqueProviderState {
    OpaqueProviderState::new("rs_opaque_sig_01").expect("固定 OpaqueProviderState 应合法")
}

pub(super) fn bound_call_read_cargo(
    turn_id: TurnId,
    step_ordinal: u32,
    part_ordinal: u32,
    invocation_id: InvocationId,
    provider_call_id: Option<&str>,
) -> BoundToolCall {
    BoundToolCall {
        invocation_id,
        provider_call_id: provider_call_id.map(str::to_owned),
        name: "read_file".to_owned(),
        arguments: json!({ "path": "Cargo.toml" }),
        arguments_digest: hex_32(ARGS_CARGO_TOML_DIGEST_HEX),
        address: ToolCallAddress {
            message: ModelMessageAddress {
                session_id: session_id_primary(),
                turn_id,
                model_step_ordinal: step_ordinal,
            },
            part_ordinal,
        },
    }
}

pub(super) fn bound_call_read_missing(
    turn_id: TurnId,
    step_ordinal: u32,
    part_ordinal: u32,
    invocation_id: InvocationId,
) -> BoundToolCall {
    BoundToolCall {
        invocation_id,
        provider_call_id: Some("call_missing_01".to_owned()),
        name: "read_file".to_owned(),
        arguments: json!({ "path": "missing.txt" }),
        arguments_digest: hex_32(ARGS_MISSING_TXT_DIGEST_HEX),
        address: ToolCallAddress {
            message: ModelMessageAddress {
                session_id: session_id_primary(),
                turn_id,
                model_step_ordinal: step_ordinal,
            },
            part_ordinal,
        },
    }
}

pub(super) fn bound_call_sleep_cmd(
    turn_id: TurnId,
    step_ordinal: u32,
    part_ordinal: u32,
    invocation_id: InvocationId,
) -> BoundToolCall {
    BoundToolCall {
        invocation_id,
        provider_call_id: Some("call_sleep_01".to_owned()),
        name: "run_shell".to_owned(),
        arguments: json!({ "command": "sleep 10" }),
        arguments_digest: hex_32(ARGS_SLEEP_CMD_DIGEST_HEX),
        address: ToolCallAddress {
            message: ModelMessageAddress {
                session_id: session_id_primary(),
                turn_id,
                model_step_ordinal: step_ordinal,
            },
            part_ordinal,
        },
    }
}

pub(super) fn success_content_json_sample() -> SuccessContent {
    SuccessContent::Json {
        value: json!({ "lines": 42 }),
        digest: hex_32(CONTENT_LINES_42_DIGEST_HEX),
        byte_length: CONTENT_LINES_42_BYTES,
    }
}

pub(super) fn success_content_oversize_sample() -> SuccessContent {
    SuccessContent::OutputTooLarge {
        digest: hex_32(CONTENT_OVERSIZE_DIGEST_HEX),
        byte_length: CONTENT_OVERSIZE_BYTES,
    }
}

pub(super) fn tool_error_not_found() -> ToolError {
    ToolError {
        code: "not_found".to_owned(),
        message: "missing.txt does not exist".to_owned(),
    }
}

pub(super) fn token_usage_full() -> TokenUsage {
    TokenUsage {
        input: Some(1200),
        cache_read: Some(400),
        cache_write: Some(100),
        output: Some(320),
        reasoning: Some(90),
    }
}

pub(super) fn token_usage_minimal() -> TokenUsage {
    TokenUsage {
        input: None,
        cache_read: None,
        cache_write: None,
        output: None,
        reasoning: None,
    }
}

pub(super) fn cost_estimate_exact() -> CostEstimate {
    CostEstimate {
        usd: 0.0042,
        exact: true,
    }
}

pub(super) fn register(book: &mut CaseBook) {
    let call_full = bound_call_read_cargo(turn_id_1(), 0, 1, invocation_id_1(), Some("call_01"));
    let call_minimal = bound_call_read_cargo(turn_id_1(), 0, 0, invocation_id_1(), None);

    enum_fragments!(
        book,
        "effort",
        Effort {
            ("off", Effort::Off => Effort::Off),
            ("minimal", Effort::Minimal => Effort::Minimal),
            ("low", Effort::Low => Effort::Low),
            ("medium", Effort::Medium => Effort::Medium),
            ("high", Effort::High => Effort::High),
            ("xhigh", Effort::Xhigh => Effort::Xhigh),
            ("max", Effort::Max => Effort::Max),
        }
    );

    enum_fragments!(
        book,
        "protocol",
        Protocol {
            ("chat_completions", Protocol::ChatCompletions => Protocol::ChatCompletions),
            ("responses", Protocol::Responses => Protocol::Responses),
            ("messages", Protocol::Messages => Protocol::Messages),
            ("generate_content", Protocol::GenerateContent => Protocol::GenerateContent),
            ("interactions", Protocol::Interactions => Protocol::Interactions),
        }
    );

    enum_fragments!(
        book,
        "message_role",
        MessageRole {
            ("system", MessageRole::System => MessageRole::System),
            ("user", MessageRole::User => MessageRole::User),
            ("assistant", MessageRole::Assistant => MessageRole::Assistant),
            ("tool", MessageRole::Tool => MessageRole::Tool),
        }
    );

    enum_fragments!(
        book,
        "message_semantic",
        MessageSemantic<BoundToolCall> {
            (
                "text",
                MessageSemantic::Text { .. } => MessageSemantic::Text {
                    text: "请读取 Cargo.toml".to_owned(),
                }
            ),
            (
                "reasoning",
                MessageSemantic::Reasoning { .. } => MessageSemantic::Reasoning {
                    text: "先调用 read_file 查看工作区配置".to_owned(),
                }
            ),
            (
                "tool_call",
                MessageSemantic::ToolCall { .. } => MessageSemantic::ToolCall {
                    call: call_full.clone(),
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "tool_call_validation",
        ToolCallValidation {
            (
                "too_many_calls",
                ToolCallValidation::TooManyCalls { .. } => ToolCallValidation::TooManyCalls {
                    limit: 64,
                    actual: 65,
                }
            ),
            (
                "malformed_arguments",
                ToolCallValidation::MalformedArguments { .. } => ToolCallValidation::MalformedArguments {
                    call_ordinal: 0,
                }
            ),
            (
                "arguments_not_object",
                ToolCallValidation::ArgumentsNotObject { .. } => ToolCallValidation::ArgumentsNotObject {
                    call_ordinal: 1,
                }
            ),
            (
                "arguments_too_large",
                ToolCallValidation::ArgumentsTooLarge { .. } => ToolCallValidation::ArgumentsTooLarge {
                    call_ordinal: 0,
                    limit: 65_536,
                    actual: 70_000,
                }
            ),
            (
                "missing_tool_name",
                ToolCallValidation::MissingToolName { .. } => ToolCallValidation::MissingToolName {
                    call_ordinal: 2,
                }
            ),
            (
                "empty_tool_call_id",
                ToolCallValidation::EmptyToolCallId { .. } => ToolCallValidation::EmptyToolCallId {
                    call_ordinal: 0,
                }
            ),
            (
                "duplicate_tool_call_id",
                ToolCallValidation::DuplicateToolCallId { .. } => ToolCallValidation::DuplicateToolCallId {
                    first_ordinal: 0,
                    second_ordinal: 1,
                }
            ),
            (
                "unknown_tool",
                ToolCallValidation::UnknownTool { .. } => ToolCallValidation::UnknownTool {
                    call_ordinal: 0,
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "tool_result_outcome",
        ToolResultOutcome {
            ("succeeded", ToolResultOutcome::Succeeded => ToolResultOutcome::Succeeded),
            ("failed", ToolResultOutcome::Failed => ToolResultOutcome::Failed),
            ("not_executed", ToolResultOutcome::NotExecuted => ToolResultOutcome::NotExecuted),
            ("indeterminate", ToolResultOutcome::Indeterminate => ToolResultOutcome::Indeterminate),
        }
    );

    enum_fragments!(
        book,
        "indeterminate_cause",
        IndeterminateCause {
            ("no_result", IndeterminateCause::NoResult => IndeterminateCause::NoResult),
            ("cancelled", IndeterminateCause::Cancelled => IndeterminateCause::Cancelled),
            ("reported", IndeterminateCause::Reported => IndeterminateCause::Reported),
        }
    );

    enum_fragments!(
        book,
        "success_content",
        SuccessContent {
            ("json", SuccessContent::Json { .. } => success_content_json_sample()),
            ("output_too_large", SuccessContent::OutputTooLarge { .. } => success_content_oversize_sample()),
        }
    );

    enum_fragments!(
        book,
        "result_payload",
        ResultPayload {
            (
                "succeeded",
                ResultPayload::Succeeded(_) => ResultPayload::Succeeded(success_content_json_sample())
            ),
            (
                "failed",
                ResultPayload::Failed(_) => ResultPayload::Failed(tool_error_not_found())
            ),
            (
                "not_executed",
                ResultPayload::NotExecuted => ResultPayload::NotExecuted
            ),
            (
                "indeterminate",
                ResultPayload::Indeterminate(_) => ResultPayload::Indeterminate(IndeterminateCause::NoResult)
            ),
        }
    );

    enum_fragments!(
        book,
        "usage_completeness",
        UsageCompleteness {
            ("complete", UsageCompleteness::Complete => UsageCompleteness::Complete),
            ("partial", UsageCompleteness::Partial => UsageCompleteness::Partial),
        }
    );

    book.insert_struct_pair("bound_tool_call", &call_full, &call_minimal);

    let part_full = MessagePart {
        semantic: MessageSemantic::Reasoning {
            text: "先读取工作区配置".to_owned(),
        },
        replay_state: Some(opaque_state_sample()),
    };
    let part_minimal = MessagePart {
        semantic: MessageSemantic::Text {
            text: "请总结这个项目".to_owned(),
        },
        replay_state: None,
    };
    book.insert_struct_pair("message_part", &part_full, &part_minimal);

    let msg_full = NativeMessage {
        role: MessageRole::Assistant,
        provider_origin: Some(replay_origin_openai()),
        parts: vec![
            part_full,
            MessagePart {
                semantic: MessageSemantic::ToolCall { call: call_full },
                replay_state: None,
            },
        ],
    };
    let msg_minimal: NativeMessage<BoundToolCall> = NativeMessage {
        role: MessageRole::User,
        provider_origin: None,
        parts: Vec::new(),
    };
    book.insert_struct_pair("native_message", &msg_full, &msg_minimal);

    book.insert_struct_pair("token_usage", &token_usage_full(), &token_usage_minimal());

    let token_summary_full = TokenSummary {
        known: Some(1600),
        complete: true,
        overflowed: false,
    };
    let token_summary_minimal = TokenSummary {
        known: None,
        complete: false,
        overflowed: false,
    };
    book.insert_struct_pair("token_summary", &token_summary_full, &token_summary_minimal);

    let cost_summary_full = CostSummary {
        known: Some(cost_estimate_exact()),
        unknown_attempts: 0,
        overflowed: false,
    };
    let cost_summary_minimal = CostSummary {
        known: None,
        unknown_attempts: 1,
        overflowed: false,
    };
    book.insert_struct_pair("cost_summary", &cost_summary_full, &cost_summary_minimal);
}
