// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-context` 领域行为与逐字节黄金测试（ADR 0014）。

use std::collections::BTreeMap;
use std::str::FromStr;

use lattice_context::{
    Channel, ChannelError, ContextEdit, ContextError, ContextInput, ContextInspection, ContextOp,
    ContextText, EffectiveEntrySummary, EmptyContextOpError, InvalidUserMessageError,
    MAX_CHANNEL_BYTES, NodeStatus, NormalizedUserMessage, OperatedNode, PathNodeKind, PathRecord,
    TextBoundError, ToolResultText, resolve,
};
use lattice_conversation::{
    BoundToolCall, ConversationItem, ImageMediaType, ImageRef, IndeterminateCause, InvocationId,
    MAX_IMAGE_BYTES, MAX_TOOL_CONTENT_BYTES, MAX_USER_TEXT_BYTES, MessagePart, MessageRole,
    MessageSemantic, ModelMessageAddress, NativeMessage, NodeId, OpaqueProviderState, Protocol,
    ProviderId, ReplayOrigin, ResultPayload, SessionId, StepContent, StoredToolResult,
    SuccessContent, ToolCallAddress, ToolError, TurnId, UnboundToolCall, UserInputError,
    canonical_json, tool_content_digest,
};
use serde_json::{Value, json};

const SESSION_ID: &str = "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c";
const TURN_ID: &str = "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c";
const INV_1: &str = "11111111-1111-4111-8111-111111111111";
const INV_2: &str = "22222222-2222-4222-9222-222222222222";
const INV_3: &str = "33333333-3333-4333-a333-333333333333";

fn ch(raw: &str) -> Channel {
    Channel::parse(raw).expect("测试用 Channel 应合法")
}

fn inv(raw: &str) -> InvocationId {
    InvocationId::parse(raw).expect("测试用 InvocationId 应合法")
}

fn img(tag: u8, byte_length: u64) -> ImageRef {
    ImageRef {
        sha256: [tag; 32],
        media_type: ImageMediaType::Png,
        byte_length,
    }
}

fn text_sem(s: &str) -> MessageSemantic<BoundToolCall> {
    MessageSemantic::Text { text: s.to_owned() }
}

fn image_sem(image: ImageRef) -> MessageSemantic<BoundToolCall> {
    MessageSemantic::Image { image }
}

fn text_part(s: &str) -> MessagePart<BoundToolCall> {
    MessagePart::semantic_only(text_sem(s))
}

fn reasoning_part(s: &str, replay: Option<&str>) -> MessagePart<BoundToolCall> {
    MessagePart {
        semantic: MessageSemantic::Reasoning { text: s.to_owned() },
        replay_state: replay.map(|r| OpaqueProviderState::new(r).expect("合法回放状态")),
    }
}

fn sample_origin() -> ReplayOrigin {
    ReplayOrigin {
        protocol: Protocol::GenerateContent,
        provider_id: ProviderId::parse("google").expect("合法 provider id"),
    }
}

fn bound_call(
    invocation: InvocationId,
    name: &str,
    part_ordinal: u32,
    args: Value,
) -> BoundToolCall {
    UnboundToolCall {
        provider_call_id: Some(format!("call_{name}_{part_ordinal}")),
        name: name.to_owned(),
        arguments: args,
    }
    .bind(
        invocation,
        ToolCallAddress {
            message: ModelMessageAddress {
                session_id: SessionId::parse(SESSION_ID).expect("合法 SessionId"),
                turn_id: TurnId::parse(TURN_ID).expect("合法 TurnId"),
                model_step_ordinal: 0,
            },
            part_ordinal,
        },
    )
}

fn json_result(invocation: InvocationId, name: &str, value: Value) -> StoredToolResult {
    let bytes = canonical_json(&value);
    StoredToolResult {
        invocation_id: invocation,
        name: name.to_owned(),
        payload: ResultPayload::Succeeded(SuccessContent::Json {
            value,
            digest: tool_content_digest(&bytes),
            byte_length: bytes.len() as u64,
        }),
    }
}

fn step_with_parts(
    origin: Option<ReplayOrigin>,
    parts: Vec<MessagePart<BoundToolCall>>,
    results: Vec<StoredToolResult>,
) -> StepContent {
    StepContent {
        assistant: NativeMessage::new(MessageRole::Assistant, origin, parts),
        results,
    }
}

// ============================================================================
// 1. Channel 解析、边界与 serde
// ============================================================================

#[test]
fn channel_accepts_valid_values_and_rejects_invalid_ones_in_parse_and_serde() {
    let max_ascii = "a".repeat(MAX_CHANNEL_BYTES);
    let max_utf8 = format!("{}a", "中".repeat(MAX_CHANNEL_BYTES / 3));
    assert_eq!(max_utf8.len(), MAX_CHANNEL_BYTES);

    for raw in [
        "tui",
        "tui/comment",
        "cli",
        "im/telegram:123456",
        "doc:path with spaces.md",
        "web:https://example.com/?a=1&b=\"2\"<3>",
        "中文通道",
        &max_ascii,
        &max_utf8,
    ] {
        let parsed = Channel::parse(raw).expect("合法 Channel 应解析成功");
        assert_eq!(parsed.as_str(), raw);
        assert_eq!(parsed.to_string(), raw);
        assert_eq!(parsed, raw);
        assert_eq!(parsed, *raw);
        assert_eq!(Channel::from_str(raw), Ok(parsed.clone()));
        assert_eq!(
            std::borrow::Borrow::<str>::borrow(&parsed),
            raw,
            "Borrow<str> 应返回原文"
        );
        assert_eq!(parsed.as_ref(), raw, "AsRef<str> 应返回原文");

        let json = serde_json::to_string(&parsed).expect("Channel 序列化应成功");
        let back: Channel = serde_json::from_str(&json).expect("Channel 反序列化应成功");
        assert_eq!(back, parsed, "Channel 透明 JSON 往返应相等");
    }

    let over_ascii = "a".repeat(MAX_CHANNEL_BYTES + 1);
    let over_utf8 = "中".repeat(MAX_CHANNEL_BYTES / 3 + 1);
    assert_eq!(over_utf8.len(), 258);

    for (raw, expected) in [
        ("", ChannelError::Empty),
        (
            over_ascii.as_str(),
            ChannelError::TooLong {
                actual: MAX_CHANNEL_BYTES + 1,
            },
        ),
        (over_utf8.as_str(), ChannelError::TooLong { actual: 258 }),
        ("tui\nline", ChannelError::ControlCharacter),
        ("tui\rline", ChannelError::ControlCharacter),
        ("tui\tline", ChannelError::ControlCharacter),
        ("tui\0null", ChannelError::ControlCharacter),
        ("tui\u{007f}", ChannelError::ControlCharacter),
        ("tui\u{0085}", ChannelError::ControlCharacter),
        ("tui\u{2028}", ChannelError::ControlCharacter),
        ("tui\u{2029}", ChannelError::ControlCharacter),
        ("\n", ChannelError::ControlCharacter),
        (" tui", ChannelError::SurroundingWhitespace),
        ("tui ", ChannelError::SurroundingWhitespace),
        (" ", ChannelError::SurroundingWhitespace),
        ("\u{3000}tui", ChannelError::SurroundingWhitespace),
        ("tui\u{00a0}", ChannelError::SurroundingWhitespace),
    ] {
        assert_eq!(
            Channel::parse(raw),
            Err(expected),
            "非法 Channel {raw:?} 应返回预期错误"
        );
        let raw_json = serde_json::to_string(raw).expect("字符串序列化");
        assert!(
            serde_json::from_str::<Channel>(&raw_json).is_err(),
            "Channel 反序列化必须拒绝 {raw:?}"
        );
    }
}

// ============================================================================
// 2. 强类型载体与反序列化边界（NormalizedUserMessage / ContextText / ToolResultText / ContextInput / ContextOp）
// ============================================================================

#[test]
fn normalized_user_message_enforces_invariants_on_rust_and_json_boundaries() {
    let from_parts = NormalizedUserMessage::from_parts(vec![
        text_sem(""),
        text_sem("前"),
        text_sem("后"),
        image_sem(img(1, 128)),
    ])
    .expect("合法多片段应规范化成功");
    assert_eq!(from_parts.as_message().parts.len(), 2);

    let encoded = serde_json::to_string(&from_parts).expect("序列化成功");
    let decoded: NormalizedUserMessage =
        serde_json::from_str(&encoded).expect("合法规范化消息反序列化应成功");
    assert_eq!(decoded, from_parts);

    // 非 User 角色
    let assistant_msg = NativeMessage::new(MessageRole::Assistant, None, vec![text_part("hi")]);
    assert_eq!(
        NormalizedUserMessage::try_from_message(assistant_msg.clone()),
        Err(InvalidUserMessageError::UnexpectedRole {
            actual: MessageRole::Assistant
        })
    );
    assert!(
        serde_json::from_str::<NormalizedUserMessage>(
            &serde_json::to_string(&assistant_msg).unwrap()
        )
        .is_err(),
        "反序列化必须拒绝非 User 角色"
    );

    // 携带 provider_origin
    let with_origin = NativeMessage::new(
        MessageRole::User,
        Some(sample_origin()),
        vec![text_part("hi")],
    );
    assert_eq!(
        NormalizedUserMessage::try_from_message(with_origin.clone()),
        Err(InvalidUserMessageError::UnexpectedProviderOrigin)
    );
    assert!(
        serde_json::from_str::<NormalizedUserMessage>(
            &serde_json::to_string(&with_origin).unwrap()
        )
        .is_err(),
        "反序列化必须拒绝非空 provider_origin"
    );

    // 携带 replay_state
    let with_replay = NativeMessage::new(
        MessageRole::User,
        None,
        vec![MessagePart {
            semantic: text_sem("hi"),
            replay_state: Some(OpaqueProviderState::new("sig").unwrap()),
        }],
    );
    assert_eq!(
        NormalizedUserMessage::try_from_message(with_replay.clone()),
        Err(InvalidUserMessageError::UnexpectedReplayState { part: 1 })
    );
    assert!(
        serde_json::from_str::<NormalizedUserMessage>(
            &serde_json::to_string(&with_replay).unwrap()
        )
        .is_err(),
        "反序列化必须拒绝携带 replay_state 的片段"
    );

    // 未规范化：空文本与未合并相邻文本
    for (label, parts) in [
        ("空文本片段", vec![text_part(""), text_part("ok")]),
        ("未合并相邻文本", vec![text_part("a"), text_part("b")]),
    ] {
        let msg = NativeMessage::new(MessageRole::User, None, parts);
        assert_eq!(
            NormalizedUserMessage::try_from_message(msg.clone()),
            Err(InvalidUserMessageError::NotNormalized),
            "{label} 必须被拒绝"
        );
        assert!(
            serde_json::from_str::<NormalizedUserMessage>(&serde_json::to_string(&msg).unwrap())
                .is_err(),
            "反序列化必须拒绝{label}"
        );
    }

    // 委托 normalize_user_message 拒绝空消息、非法片段与上限越界（四项精确边界由 lattice-conversation 锁定）
    let empty_msg = NativeMessage::new(MessageRole::User, None, vec![]);
    assert_eq!(
        NormalizedUserMessage::try_from_message(empty_msg),
        Err(InvalidUserMessageError::InvalidInput(UserInputError::Empty))
    );

    let reasoning_msg = NativeMessage::new(
        MessageRole::User,
        None,
        vec![reasoning_part("secret", None)],
    );
    assert_eq!(
        NormalizedUserMessage::try_from_message(reasoning_msg),
        Err(InvalidUserMessageError::InvalidInput(
            UserInputError::UnsupportedPart { part: 1 }
        ))
    );

    let oversized_text = NativeMessage::new(
        MessageRole::User,
        None,
        vec![text_part(&"x".repeat(MAX_USER_TEXT_BYTES + 1))],
    );
    assert_eq!(
        NormalizedUserMessage::try_from_message(oversized_text),
        Err(InvalidUserMessageError::InvalidInput(
            UserInputError::TextTooLarge {
                actual: MAX_USER_TEXT_BYTES + 1
            }
        ))
    );

    let oversized_image = NativeMessage::new(
        MessageRole::User,
        None,
        vec![MessagePart::semantic_only(image_sem(img(
            1,
            MAX_IMAGE_BYTES as u64 + 1,
        )))],
    );
    assert_eq!(
        NormalizedUserMessage::try_from_message(oversized_image),
        Err(InvalidUserMessageError::InvalidInput(
            UserInputError::InvalidImageLength {
                image: 1,
                actual: MAX_IMAGE_BYTES as u64 + 1
            }
        ))
    );
}

#[test]
fn context_text_and_tool_result_text_enforce_exact_byte_bounds() {
    // ContextText：非空（空白保留不裁剪），上限 MAX_USER_TEXT_BYTES
    assert_eq!(ContextText::new(""), Err(TextBoundError::Empty));
    let ws = ContextText::new(" \n\t ").expect("空白文本应原样保留");
    assert_eq!(ws.as_str(), " \n\t ");

    let max_ctx = "a".repeat(MAX_USER_TEXT_BYTES);
    assert!(ContextText::new(max_ctx).is_ok(), "恰好 2 MiB 应通过");
    let over_ctx = "a".repeat(MAX_USER_TEXT_BYTES + 1);
    assert_eq!(
        ContextText::new(over_ctx.clone()),
        Err(TextBoundError::TooLarge {
            limit: MAX_USER_TEXT_BYTES,
            actual: MAX_USER_TEXT_BYTES + 1,
        })
    );
    assert!(
        serde_json::from_str::<ContextText>("\"\"").is_err(),
        "ContextText 反序列化必须拒绝空串"
    );
    assert!(
        serde_json::from_str::<ContextText>(&serde_json::to_string(&over_ctx).unwrap()).is_err(),
        "ContextText 反序列化必须拒绝超限文本"
    );

    // ToolResultText：按 canonical_json(&Value::String(text)).len() <= MAX_TOOL_CONTENT_BYTES 核验
    assert_eq!(ToolResultText::new(""), Err(TextBoundError::Empty));

    // 纯 ASCII：加两端引号恰好 65,536 字节通过，多 1 字节拒绝
    let exact_ascii = "a".repeat(MAX_TOOL_CONTENT_BYTES - 2);
    assert_eq!(
        canonical_json(&Value::String(exact_ascii.clone())).len(),
        MAX_TOOL_CONTENT_BYTES
    );
    assert!(
        ToolResultText::new(exact_ascii).is_ok(),
        "规范 JSON 恰好 65,536 字节的 ToolResultText 应通过"
    );

    let over_ascii = "a".repeat(MAX_TOOL_CONTENT_BYTES - 1);
    assert_eq!(
        ToolResultText::new(over_ascii.clone()),
        Err(TextBoundError::TooLarge {
            limit: MAX_TOOL_CONTENT_BYTES,
            actual: MAX_TOOL_CONTENT_BYTES + 1,
        })
    );
    assert!(
        serde_json::from_str::<ToolResultText>(&serde_json::to_string(&over_ascii).unwrap())
            .is_err(),
        "ToolResultText 反序列化必须按规范 JSON 字节拒绝超限文本"
    );

    // 转义膨胀场景：换行符 `\n` 在 JSON 中占 2 字节（`\n`），32,767 个换行加引号恰好 65,536 字节
    let exact_newlines = "\n".repeat((MAX_TOOL_CONTENT_BYTES - 2) / 2);
    assert_eq!(
        canonical_json(&Value::String(exact_newlines.clone())).len(),
        MAX_TOOL_CONTENT_BYTES
    );
    assert!(
        ToolResultText::new(exact_newlines).is_ok(),
        "含转义换行且规范 JSON 恰好 65,536 字节应通过"
    );

    let over_newlines = "\n".repeat((MAX_TOOL_CONTENT_BYTES - 2) / 2 + 1);
    assert_eq!(
        ToolResultText::new(over_newlines),
        Err(TextBoundError::TooLarge {
            limit: MAX_TOOL_CONTENT_BYTES,
            actual: MAX_TOOL_CONTENT_BYTES + 2,
        }),
        "原文未超限但 JSON 转义后超限的 ToolResultText 必须拒绝"
    );
}

#[test]
fn context_input_op_and_all_edit_variants_golden_json_and_strict_rejection() {
    let image = ImageRef {
        sha256: [0xab; 32],
        media_type: ImageMediaType::Webp,
        byte_length: 2048,
    };
    let input = ContextInput::new(ch("tui/comment"), vec![text_sem("批注内容")])
        .expect("合法 ContextInput 应构造成功");
    let input_json = serde_json::to_string(&input).expect("ContextInput 序列化");
    assert_eq!(
        input_json,
        r#"{"channel":"tui/comment","message":{"role":"user","provider_origin":null,"parts":[{"semantic":{"text":{"text":"批注内容"}},"replay_state":null}]}}"#,
        "ContextInput 逐字节 JSON 输出不匹配"
    );
    assert_eq!(
        serde_json::from_str::<ContextInput>(&input_json).expect("ContextInput 反序列化"),
        input
    );
    assert!(
        serde_json::from_str::<ContextInput>(
            r#"{"channel":"tui","message":{"role":"user","provider_origin":null,"parts":[{"semantic":{"text":{"text":"ok"}},"replay_state":null}]},"extra":1}"#
        )
        .is_err(),
        "ContextInput 必须拒绝未知字段"
    );

    // 空编辑列表拒绝
    assert_eq!(ContextOp::new(ch("cli"), vec![]), Err(EmptyContextOpError));
    assert!(
        serde_json::from_str::<ContextOp>(r#"{"channel":"cli","edits":[]}"#).is_err(),
        "ContextOp 反序列化必须拒绝空编辑列表"
    );
    assert!(
        serde_json::from_str::<ContextOp>(
            r#"{"channel":"cli","edits":[{"kind":"omit_node","node":1}],"extra":true}"#
        )
        .is_err(),
        "ContextOp 反序列化必须拒绝未知字段"
    );

    // 全部 7 种 ContextEdit 变体的精确 JSON 形态与往返
    let op = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::omit_node(NodeId(1)),
            ContextEdit::replace_input(NodeId(2), vec![text_sem("新输入")]).unwrap(),
            ContextEdit::replace_step(NodeId(3), "精简结论").unwrap(),
            ContextEdit::omit_invocation(inv(INV_1)),
            ContextEdit::replace_tool_result(inv(INV_2), "精简结果").unwrap(),
            ContextEdit::strip_reasoning(NodeId(4)),
            ContextEdit::replace_image(NodeId(5), image, Some("已替换为说明".to_owned())).unwrap(),
            ContextEdit::replace_image(NodeId(6), image, None).unwrap(),
        ],
    )
    .expect("合法 ContextOp 应构造成功");

    let op_value = serde_json::to_value(&op).expect("ContextOp 序列化为 Value");
    let sha_hex = "ab".repeat(32);
    assert_eq!(
        op_value,
        json!({
            "channel": "cli",
            "edits": [
                { "kind": "omit_node", "node": 1 },
                {
                    "kind": "replace_input",
                    "node": 2,
                    "message": {
                        "role": "user",
                        "provider_origin": null,
                        "parts": [{ "semantic": { "text": { "text": "新输入" } }, "replay_state": null }]
                    }
                },
                { "kind": "replace_step", "node": 3, "text": "精简结论" },
                { "kind": "omit_invocation", "invocation": INV_1 },
                { "kind": "replace_tool_result", "invocation": INV_2, "text": "精简结果" },
                { "kind": "strip_reasoning", "node": 4 },
                {
                    "kind": "replace_image",
                    "node": 5,
                    "image": { "sha256": sha_hex, "media_type": "webp", "byte_length": 2048 },
                    "text": "已替换为说明"
                },
                {
                    "kind": "replace_image",
                    "node": 6,
                    "image": { "sha256": sha_hex, "media_type": "webp", "byte_length": 2048 },
                    "text": null
                }
            ]
        }),
        "ContextOp 与全部 ContextEdit 变体的 JSON 形状不匹配"
    );
    let back: ContextOp = serde_json::from_value(op_value).expect("ContextOp 反序列化");
    assert_eq!(back, op, "ContextOp JSON 往返应相等");

    assert!(
        serde_json::from_str::<ContextEdit>(r#"{"kind":"omit_node","node":1,"extra":1}"#).is_err(),
        "ContextEdit 必须拒绝未知字段"
    );
}

// ============================================================================
// 3. resolve() 路径入场校验、错误顺序、状态演进与 validate_op 只读一致性
// ============================================================================

#[test]
fn resolve_validates_path_uniqueness_and_step_entrance_invariants() {
    let input = ContextInput::new(ch("tui"), vec![text_sem("你好")]).unwrap();
    let op = ContextOp::new(ch("cli"), vec![ContextEdit::omit_node(NodeId(1))]).unwrap();

    // NodeId 重复（涵盖 Input / Step / Op）
    assert_eq!(
        resolve(&[
            PathRecord::Input {
                id: NodeId(1),
                input: &input,
            },
            PathRecord::Op {
                id: NodeId(1),
                op: &op,
            },
        ]),
        Err(ContextError::DuplicateNodeId { node: NodeId(1) })
    );

    // Step 角色非 Assistant
    let wrong_role = StepContent {
        assistant: NativeMessage::new(MessageRole::User, None, vec![text_part("假助手")]),
        results: vec![],
    };
    assert!(matches!(
        resolve(&[PathRecord::Step {
            id: NodeId(2),
            content: wrong_role,
        }]),
        Err(ContextError::InvalidStep {
            node: NodeId(2),
            ..
        })
    ));

    // Step 助手消息含图片（validate_image_placement 拒绝）
    let image_in_assistant = step_with_parts(
        None,
        vec![MessagePart::semantic_only(image_sem(img(1, 64)))],
        vec![],
    );
    assert!(matches!(
        resolve(&[PathRecord::Step {
            id: NodeId(2),
            content: image_in_assistant,
        }]),
        Err(ContextError::InvalidStep {
            node: NodeId(2),
            ..
        })
    ));

    // Step 工具调用与结果数量不一致
    let call_1 = bound_call(inv(INV_1), "read_file", 0, json!({"path": "a.rs"}));
    let count_mismatch = step_with_parts(
        None,
        vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
            call: call_1.clone(),
        })],
        vec![],
    );
    assert!(matches!(
        resolve(&[PathRecord::Step {
            id: NodeId(2),
            content: count_mismatch,
        }]),
        Err(ContextError::InvalidStep {
            node: NodeId(2),
            ..
        })
    ));

    // Step 工具调用与结果名称不匹配
    let name_mismatch = step_with_parts(
        None,
        vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
            call: call_1.clone(),
        })],
        vec![json_result(inv(INV_1), "write_file", json!("ok"))],
    );
    assert!(matches!(
        resolve(&[PathRecord::Step {
            id: NodeId(2),
            content: name_mismatch,
        }]),
        Err(ContextError::InvalidStep {
            node: NodeId(2),
            ..
        })
    ));

    // Step 工具调用与结果 InvocationId 不匹配
    let id_mismatch = step_with_parts(
        None,
        vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
            call: call_1.clone(),
        })],
        vec![json_result(inv(INV_2), "read_file", json!("ok"))],
    );
    assert!(matches!(
        resolve(&[PathRecord::Step {
            id: NodeId(2),
            content: id_mismatch,
        }]),
        Err(ContextError::InvalidStep {
            node: NodeId(2),
            ..
        })
    ));

    // 同一步内 InvocationId 重复
    let dup_in_step = step_with_parts(
        None,
        vec![
            MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: call_1.clone(),
            }),
            MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: bound_call(inv(INV_1), "read_file", 1, json!({"path": "b.rs"})),
            }),
        ],
        vec![
            json_result(inv(INV_1), "read_file", json!("a")),
            json_result(inv(INV_1), "read_file", json!("b")),
        ],
    );
    assert_eq!(
        resolve(&[PathRecord::Step {
            id: NodeId(2),
            content: dup_in_step,
        }]),
        Err(ContextError::DuplicateInvocationId {
            invocation: inv(INV_1),
        })
    );

    // 跨两个 Step 的 InvocationId 重复
    let valid_step_1 = step_with_parts(
        None,
        vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
            call: call_1.clone(),
        })],
        vec![json_result(inv(INV_1), "read_file", json!("a"))],
    );
    let valid_step_2 = valid_step_1.clone();
    assert_eq!(
        resolve(&[
            PathRecord::Step {
                id: NodeId(2),
                content: valid_step_1,
            },
            PathRecord::Step {
                id: NodeId(3),
                content: valid_step_2,
            },
        ]),
        Err(ContextError::DuplicateInvocationId {
            invocation: inv(INV_1),
        })
    );
}

#[test]
fn resolve_and_validate_op_enforce_four_stage_error_precedence() {
    let img_a = img(1, 100);
    let img_b = img(2, 200);
    let input = ContextInput::new(ch("tui"), vec![text_sem("问"), image_sem(img_a)]).unwrap();
    let call_1 = bound_call(inv(INV_1), "read_file", 0, json!({"path": "a.rs"}));
    let call_2 = bound_call(inv(INV_2), "list_dir", 1, json!({"path": "."}));
    let step = step_with_parts(
        Some(sample_origin()),
        vec![
            reasoning_part("思考", Some("rsig")),
            MessagePart::semantic_only(MessageSemantic::ToolCall { call: call_1 }),
            MessagePart::semantic_only(MessageSemantic::ToolCall { call: call_2 }),
            text_part("答"),
        ],
        vec![
            json_result(inv(INV_1), "read_file", json!("文件内容")),
            json_result(inv(INV_2), "list_dir", json!(["a.rs"])),
        ],
    );
    let op_3 = ContextOp::new(ch("cli"), vec![ContextEdit::strip_reasoning(NodeId(2))]).unwrap();

    // 1) 前置存在性：引用尚未出现的未来节点、根节点 0、以及 Op 引用自身 NodeId 均报 UnknownNode
    let self_ref_op = ContextOp::new(ch("cli"), vec![ContextEdit::omit_node(NodeId(3))]).unwrap();
    assert_eq!(
        resolve(&[
            PathRecord::Input {
                id: NodeId(1),
                input: &input,
            },
            PathRecord::Op {
                id: NodeId(3),
                op: &self_ref_op,
            },
        ]),
        Err(ContextError::UnknownNode { node: NodeId(3) }),
        "Op 引用自身 NodeId 应按前置不存在报 UnknownNode"
    );

    let future_inv_op =
        ContextOp::new(ch("cli"), vec![ContextEdit::omit_invocation(inv(INV_1))]).unwrap();
    assert_eq!(
        resolve(&[
            PathRecord::Op {
                id: NodeId(1),
                op: &future_inv_op,
            },
            PathRecord::Step {
                id: NodeId(2),
                content: step.clone(),
            },
        ]),
        Err(ContextError::UnknownInvocation {
            invocation: inv(INV_1),
        }),
        "Op 引用后续 Step 的 InvocationId 必须报 UnknownInvocation"
    );

    let base = resolve(&[
        PathRecord::Input {
            id: NodeId(1),
            input: &input,
        },
        PathRecord::Step {
            id: NodeId(2),
            content: step.clone(),
        },
        PathRecord::Op {
            id: NodeId(3),
            op: &op_3,
        },
    ])
    .expect("基线路径应归约成功");
    let base_before = base.clone();

    // 1) UnknownNode / UnknownInvocation
    assert_eq!(
        base.validate_op(
            &ContextOp::new(ch("cli"), vec![ContextEdit::omit_node(NodeId(0))]).unwrap()
        ),
        Err(ContextError::UnknownNode { node: NodeId(0) })
    );
    assert_eq!(
        base.validate_op(
            &ContextOp::new(ch("cli"), vec![ContextEdit::omit_invocation(inv(INV_3))]).unwrap()
        ),
        Err(ContextError::UnknownInvocation {
            invocation: inv(INV_3),
        })
    );

    // 2) NodeKindMismatch：Op 拒绝一切编辑；Input/Step 跨类型编辑拒绝
    for edit in [
        ContextEdit::omit_node(NodeId(3)),
        ContextEdit::replace_input(NodeId(3), vec![text_sem("x")]).unwrap(),
        ContextEdit::replace_step(NodeId(3), "x").unwrap(),
        ContextEdit::strip_reasoning(NodeId(3)),
        ContextEdit::replace_image(NodeId(3), img_a, None).unwrap(),
    ] {
        assert_eq!(
            base.validate_op(&ContextOp::new(ch("cli"), vec![edit]).unwrap()),
            Err(ContextError::NodeKindMismatch {
                node: NodeId(3),
                actual: PathNodeKind::Op,
            }),
            "前置 Op 节点必须拒绝一切编辑"
        );
    }
    assert_eq!(
        base.validate_op(
            &ContextOp::new(
                ch("cli"),
                vec![ContextEdit::replace_step(NodeId(1), "x").unwrap()]
            )
            .unwrap()
        ),
        Err(ContextError::NodeKindMismatch {
            node: NodeId(1),
            actual: PathNodeKind::Input,
        })
    );
    assert_eq!(
        base.validate_op(
            &ContextOp::new(ch("cli"), vec![ContextEdit::strip_reasoning(NodeId(1))]).unwrap()
        ),
        Err(ContextError::NodeKindMismatch {
            node: NodeId(1),
            actual: PathNodeKind::Input,
        })
    );
    assert_eq!(
        base.validate_op(
            &ContextOp::new(
                ch("cli"),
                vec![ContextEdit::replace_input(NodeId(2), vec![text_sem("x")]).unwrap()],
            )
            .unwrap()
        ),
        Err(ContextError::NodeKindMismatch {
            node: NodeId(2),
            actual: PathNodeKind::Step,
        })
    );
    assert_eq!(
        base.validate_op(
            &ContextOp::new(
                ch("cli"),
                vec![ContextEdit::replace_image(NodeId(2), img_a, None).unwrap()],
            )
            .unwrap()
        ),
        Err(ContextError::NodeKindMismatch {
            node: NodeId(2),
            actual: PathNodeKind::Step,
        })
    );

    // 3) NodeAlreadyOmitted 优先于 Step 局部状态：先 OmitInvocation 再 OmitNode，后续再操作该调用报 NodeAlreadyOmitted
    let omit_then_touch_inv = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::omit_invocation(inv(INV_1)),
            ContextEdit::omit_node(NodeId(2)),
            ContextEdit::replace_tool_result(inv(INV_1), "晚了").unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(
        base.validate_op(&omit_then_touch_inv),
        Err(ContextError::NodeAlreadyOmitted { node: NodeId(2) })
    );

    // 3) StepAlreadyReplaced 优先于 InvocationAlreadyOmitted；同时 Replaced Step 允许再次 ReplaceStep 与 OmitNode
    let replace_then_touch_omitted_inv = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::omit_invocation(inv(INV_1)),
            ContextEdit::replace_step(NodeId(2), "第一版总结").unwrap(),
            ContextEdit::replace_step(NodeId(2), "第二版总结").unwrap(),
            ContextEdit::omit_invocation(inv(INV_1)),
        ],
    )
    .unwrap();
    assert_eq!(
        base.validate_op(&replace_then_touch_omitted_inv),
        Err(ContextError::StepAlreadyReplaced { node: NodeId(2) })
    );

    let replace_then_strip = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::replace_step(NodeId(2), "总结").unwrap(),
            ContextEdit::strip_reasoning(NodeId(2)),
        ],
    )
    .unwrap();
    assert_eq!(
        base.validate_op(&replace_then_strip),
        Err(ContextError::StepAlreadyReplaced { node: NodeId(2) })
    );

    let replace_then_omit = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::replace_step(NodeId(2), "总结").unwrap(),
            ContextEdit::omit_node(NodeId(2)),
        ],
    )
    .unwrap();
    assert_eq!(
        base.validate_op(&replace_then_omit),
        Ok(()),
        "已 ReplaceStep 的节点允许再执行 OmitNode"
    );

    // 4) 局部元素状态：InvocationAlreadyOmitted 与 ImageNotFound
    let double_omit_inv = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::omit_invocation(inv(INV_1)),
            ContextEdit::replace_tool_result(inv(INV_1), "x").unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(
        base.validate_op(&double_omit_inv),
        Err(ContextError::InvocationAlreadyOmitted {
            invocation: inv(INV_1),
        })
    );

    let missing_img = ContextOp::new(
        ch("cli"),
        vec![ContextEdit::replace_image(NodeId(1), img_b, None).unwrap()],
    )
    .unwrap();
    assert_eq!(
        base.validate_op(&missing_img),
        Err(ContextError::ImageNotFound {
            node: NodeId(1),
            image: img_b,
        })
    );

    // 同批 ContextOp 顺序叠加：ReplaceInput 引入 img_b 后紧接着 ReplaceImage(img_b) 合法
    let chained_input_edit = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::replace_input(NodeId(1), vec![image_sem(img_b)]).unwrap(),
            ContextEdit::replace_image(NodeId(1), img_b, Some("图已替换".to_owned())).unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(base.validate_op(&chained_input_edit), Ok(()));

    // validate_op 全程只读无副作用
    assert_eq!(
        base, base_before,
        "validate_op 不得修改 EffectiveContext 状态"
    );
}

#[test]
fn resolve_step_local_edits_always_sanitize_signatures_and_preserve_unedited_steps() {
    let call_1 = bound_call(inv(INV_1), "read_file", 1, json!({"path": "a.rs"}));
    let call_2 = bound_call(inv(INV_2), "list_dir", 3, json!({"path": "."}));

    // Step 1 未经编辑，必须完整保留 provider_origin、逐片段 replay_state 与空文本签名载体
    let untouched_step = step_with_parts(
        Some(sample_origin()),
        vec![
            MessagePart {
                semantic: text_sem(""),
                replay_state: Some(OpaqueProviderState::new("thought-sig-keep").unwrap()),
            },
            reasoning_part("保留推理", Some("reason-sig-keep")),
            text_part("保留正文"),
        ],
        vec![],
    );

    // Step 2 原本不含 Reasoning，对其执行 StripReasoning 仍须清除签名、空文本载体并记入 operated_nodes
    let no_reasoning_step = step_with_parts(
        Some(sample_origin()),
        vec![
            MessagePart {
                semantic: text_sem("甲"),
                replay_state: Some(OpaqueProviderState::new("t-sig-1").unwrap()),
            },
            MessagePart {
                semantic: text_sem(""),
                replay_state: Some(OpaqueProviderState::new("empty-carrier").unwrap()),
            },
            MessagePart {
                semantic: text_sem("乙"),
                replay_state: Some(OpaqueProviderState::new("t-sig-2").unwrap()),
            },
        ],
        vec![],
    );

    // Step 3 测试 OmitInvocation（成对删除 + 合并相邻 Reasoning）与 ReplaceToolResult（canonical_json + digest）
    let tool_step = step_with_parts(
        Some(sample_origin()),
        vec![
            reasoning_part("思一", Some("r1")),
            MessagePart {
                semantic: MessageSemantic::ToolCall { call: call_1 },
                replay_state: Some(OpaqueProviderState::new("c1-sig").unwrap()),
            },
            reasoning_part("思二", Some("r2")),
            MessagePart {
                semantic: MessageSemantic::ToolCall {
                    call: call_2.clone(),
                },
                replay_state: Some(OpaqueProviderState::new("c2-sig").unwrap()),
            },
            MessagePart {
                semantic: text_sem(""),
                replay_state: Some(OpaqueProviderState::new("empty-tail").unwrap()),
            },
        ],
        vec![
            json_result(inv(INV_1), "read_file", json!("big")),
            StoredToolResult {
                invocation_id: inv(INV_2),
                name: "list_dir".to_owned(),
                payload: ResultPayload::Succeeded(SuccessContent::OutputTooLarge {
                    digest: [0x22; 32],
                    byte_length: 90_000,
                }),
            },
        ],
    );

    // Step 4 仅含 Reasoning + 空文本签名载体，StripReasoning 后变空并自动跳过（SkippedEmpty）
    let reasoning_only_step = step_with_parts(
        Some(sample_origin()),
        vec![
            reasoning_part("仅推理", Some("r-only")),
            MessagePart {
                semantic: text_sem(""),
                replay_state: Some(OpaqueProviderState::new("carrier-only").unwrap()),
            },
        ],
        vec![],
    );

    // 先编辑后面的 NodeId(4)，再编辑前面的 NodeId(2)，验证 operated_nodes 按路径出现顺序输出
    let op = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::strip_reasoning(NodeId(4)),
            ContextEdit::omit_invocation(inv(INV_1)),
            ContextEdit::replace_tool_result(inv(INV_2), "精简列表").unwrap(),
            ContextEdit::strip_reasoning(NodeId(2)),
        ],
    )
    .unwrap();

    let ctx = resolve(&[
        PathRecord::Step {
            id: NodeId(1),
            content: untouched_step.clone(),
        },
        PathRecord::Step {
            id: NodeId(2),
            content: no_reasoning_step,
        },
        PathRecord::Step {
            id: NodeId(3),
            content: tool_step,
        },
        PathRecord::Step {
            id: NodeId(4),
            content: reasoning_only_step,
        },
        PathRecord::Op {
            id: NodeId(5),
            op: &op,
        },
    ])
    .expect("归约应成功");

    let inspection = ctx.inspect();
    assert_eq!(
        inspection.operated_nodes,
        vec![
            OperatedNode {
                node: NodeId(2),
                status: NodeStatus::Modified,
            },
            OperatedNode {
                node: NodeId(3),
                status: NodeStatus::Modified,
            },
            OperatedNode {
                node: NodeId(4),
                status: NodeStatus::SkippedEmpty,
            },
        ],
        "operated_nodes 必须按路径顺序输出，且无 Reasoning 的 Step 经 StripReasoning 后也必须进入"
    );

    let projected = ctx.project();
    assert_eq!(
        projected.len(),
        3,
        "Step 4 清空后应被跳过，仅保留 Step 1/2/3"
    );

    // Step 1 原样保留
    assert_eq!(projected[0], ConversationItem::Step(untouched_step));

    // Step 2：剥离 origin/replay_state，剔除空文本并合并相邻 Text("甲") + Text("乙") -> Text("甲乙")
    assert_eq!(
        projected[1],
        ConversationItem::Step(StepContent {
            assistant: NativeMessage::new(MessageRole::Assistant, None, vec![text_part("甲乙")]),
            results: vec![],
        })
    );

    // Step 3：INV_1 成对移出，相邻 Reasoning 合并为 "思一思二"，INV_2 载荷替换为规范 JSON 字符串与摘要，provider_origin/replay_state 清零
    let expected_val = Value::String("精简列表".to_owned());
    let expected_canonical = canonical_json(&expected_val);
    assert_eq!(
        projected[2],
        ConversationItem::Step(StepContent {
            assistant: NativeMessage::new(
                MessageRole::Assistant,
                None,
                vec![
                    reasoning_part("思一思二", None),
                    MessagePart::semantic_only(MessageSemantic::ToolCall { call: call_2 }),
                ],
            ),
            results: vec![StoredToolResult {
                invocation_id: inv(INV_2),
                name: "list_dir".to_owned(),
                payload: ResultPayload::Succeeded(SuccessContent::Json {
                    value: expected_val,
                    digest: tool_content_digest(&expected_canonical),
                    byte_length: expected_canonical.len() as u64,
                }),
            }],
        })
    );

    // 补充验证：OmitInvocation 移出唯一工具调用导致 Step 清空（SkippedEmpty），随后 ReplaceStep 恢复为纯助手结论，以及 OmitInvocation 合并两侧 Text
    let call_a = bound_call(inv(INV_1), "read_file", 1, json!({"path": "a.rs"}));
    let call_b = bound_call(inv(INV_2), "list_dir", 0, json!({"path": "."}));
    let text_call_text = step_with_parts(
        Some(sample_origin()),
        vec![
            text_part("前"),
            MessagePart::semantic_only(MessageSemantic::ToolCall { call: call_a }),
            text_part("后"),
        ],
        vec![json_result(inv(INV_1), "read_file", json!("x"))],
    );
    let call_only = step_with_parts(
        Some(sample_origin()),
        vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
            call: call_b,
        })],
        vec![json_result(inv(INV_2), "list_dir", json!("y"))],
    );
    let op_omit_calls = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::omit_invocation(inv(INV_1)),
            ContextEdit::omit_invocation(inv(INV_2)),
        ],
    )
    .unwrap();
    let ctx_mid = resolve(&[
        PathRecord::Step {
            id: NodeId(10),
            content: text_call_text.clone(),
        },
        PathRecord::Step {
            id: NodeId(11),
            content: call_only.clone(),
        },
        PathRecord::Op {
            id: NodeId(12),
            op: &op_omit_calls,
        },
    ])
    .expect("成对移出调用归约应成功");
    assert_eq!(
        ctx_mid.inspect().operated_nodes,
        vec![
            OperatedNode {
                node: NodeId(10),
                status: NodeStatus::Modified,
            },
            OperatedNode {
                node: NodeId(11),
                status: NodeStatus::SkippedEmpty,
            },
        ]
    );
    assert_eq!(
        ctx_mid.project(),
        vec![ConversationItem::Step(StepContent {
            assistant: NativeMessage::new(MessageRole::Assistant, None, vec![text_part("前后")]),
            results: vec![],
        })],
        "OmitInvocation 移出中间 ToolCall 后两侧文本应合并，纯调用 Step 清空后应跳过"
    );

    let op_replace = ContextOp::new(
        ch("cli"),
        vec![ContextEdit::replace_step(NodeId(11), "已归并结论").unwrap()],
    )
    .unwrap();
    let ctx_replaced = resolve(&[
        PathRecord::Step {
            id: NodeId(10),
            content: text_call_text,
        },
        PathRecord::Step {
            id: NodeId(11),
            content: call_only,
        },
        PathRecord::Op {
            id: NodeId(12),
            op: &op_omit_calls,
        },
        PathRecord::Op {
            id: NodeId(13),
            op: &op_replace,
        },
    ])
    .expect("ReplaceStep 恢复 SkippedEmpty Step 应成功");
    assert_eq!(
        ctx_replaced.project()[1],
        ConversationItem::Step(StepContent {
            assistant: NativeMessage::new(
                MessageRole::Assistant,
                None,
                vec![text_part("已归并结论")],
            ),
            results: vec![],
        })
    );
}

#[test]
fn resolve_replace_image_merges_text_skips_empty_and_recovers_on_replace_input() {
    let img_a = img(1, 100);
    let img_b = img(2, 250);

    // Input 1 含重复图片 img_a 与其他图片 img_b：ReplaceImage(img_a, Some) 替换全部匹配项并合并相邻文本
    let input_1 = ContextInput::new(
        ch("screenshot"),
        vec![
            text_sem("前"),
            image_sem(img_a),
            image_sem(img_a),
            text_sem("中"),
            image_sem(img_b),
            text_sem("后"),
        ],
    )
    .unwrap();

    // Input 2 仅含 img_a：先 ReplaceImage(None) 清空为 SkippedEmpty，再 ReplaceInput 恢复为有效输入
    let input_2 = ContextInput::new(ch("doc:spec.md"), vec![image_sem(img_a)]).unwrap();

    let op_clear = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::replace_image(NodeId(1), img_a, Some("[略]".to_owned())).unwrap(),
            ContextEdit::replace_image(NodeId(2), img_a, None).unwrap(),
        ],
    )
    .unwrap();

    let ctx_cleared = resolve(&[
        PathRecord::Input {
            id: NodeId(1),
            input: &input_1,
        },
        PathRecord::Input {
            id: NodeId(2),
            input: &input_2,
        },
        PathRecord::Op {
            id: NodeId(3),
            op: &op_clear,
        },
    ])
    .expect("清空前归约应成功");

    let insp_cleared = ctx_cleared.inspect();
    assert_eq!(
        insp_cleared.entries.len(),
        1,
        "Input 2 清空后不应进入 entries"
    );
    assert_eq!(
        insp_cleared.operated_nodes,
        vec![
            OperatedNode {
                node: NodeId(1),
                status: NodeStatus::Modified,
            },
            OperatedNode {
                node: NodeId(2),
                status: NodeStatus::SkippedEmpty,
            },
        ]
    );
    assert_eq!(
        insp_cleared.channel_counts,
        BTreeMap::from([(ch("screenshot"), 1)]),
        "清空跳过的 Input 2 不应计入 channel_counts"
    );
    assert_eq!(insp_cleared.images, vec![img_b]);
    assert_eq!(insp_cleared.total_image_bytes, 250);

    // 验证 Input 1 的重复 img_a 均已替换且合并为 "前[略][略]中" + img_b + "后"
    assert_eq!(
        ctx_cleared.project(),
        vec![ConversationItem::Message(NativeMessage::new(
            MessageRole::User,
            None,
            vec![
                text_part("<lattice-context channel=\"screenshot\">\n前[略][略]中"),
                MessagePart::semantic_only(image_sem(img_b)),
                text_part("后\n</lattice-context>"),
            ],
        ))]
    );

    // 对处于 SkippedEmpty 的 Input 2 执行 ReplaceInput 可恢复为有效输入且保留原 Channel
    let op_recover = ContextOp::new(
        ch("cli"),
        vec![ContextEdit::replace_input(NodeId(2), vec![text_sem("已恢复")]).unwrap()],
    )
    .unwrap();
    let ctx_recovered = resolve(&[
        PathRecord::Input {
            id: NodeId(1),
            input: &input_1,
        },
        PathRecord::Input {
            id: NodeId(2),
            input: &input_2,
        },
        PathRecord::Op {
            id: NodeId(3),
            op: &op_clear,
        },
        PathRecord::Op {
            id: NodeId(4),
            op: &op_recover,
        },
    ])
    .expect("恢复归约应成功");

    let insp_recovered = ctx_recovered.inspect();
    assert_eq!(insp_recovered.entries.len(), 2);
    assert_eq!(
        insp_recovered.operated_nodes[1],
        OperatedNode {
            node: NodeId(2),
            status: NodeStatus::Modified,
        }
    );
    assert_eq!(
        insp_recovered.channel_counts,
        BTreeMap::from([(ch("doc:spec.md"), 1), (ch("screenshot"), 1)])
    );
    assert_eq!(
        ctx_recovered.project()[1],
        ConversationItem::Message(NativeMessage::new(
            MessageRole::User,
            None,
            vec![text_part(
                "<lattice-context channel=\"doc:spec.md\">\n已恢复\n</lattice-context>"
            )],
        ))
    );
}

// ============================================================================
// 4. inspect() 客观统计（覆盖全部 5 种 ResultPayload 变体与重复图片）
// ============================================================================

#[test]
fn inspect_computes_exact_objective_byte_stats_across_all_result_payloads() {
    // 空路径产出全零统计与空投影
    let empty = resolve(&[]).expect("空路径归约应成功");
    assert_eq!(
        empty.inspect(),
        ContextInspection {
            entries: vec![],
            operated_nodes: vec![],
            channel_counts: BTreeMap::new(),
            total_text_bytes: 0,
            images: vec![],
            total_image_bytes: 0,
        }
    );
    assert!(empty.project().is_empty());

    let img_a = img(1, 100);
    let img_b = img(2, 300);

    let input_1 = ContextInput::new(
        ch("tui"),
        vec![text_sem("你好"), image_sem(img_a), image_sem(img_a)],
    )
    .unwrap();
    let input_2 = ContextInput::new(ch("tui"), vec![image_sem(img_b)]).unwrap();
    let input_3 = ContextInput::new(ch("cli"), vec![text_sem("将被移出")]).unwrap();

    // 构造覆盖全部 5 种 ResultPayload 变体的 Step（其中 Failed 含超 4,096 字节多字节中文截断边界）
    let inv_json = inv("11111111-1111-4111-8111-111111111111");
    let inv_large = inv("22222222-2222-4222-8222-222222222222");
    let inv_failed = inv("33333333-3333-4333-8333-333333333333");
    let inv_not_exec = inv("44444444-4444-4444-8444-444444444444");
    let inv_indet = inv("55555555-5555-4555-8555-555555555555");

    let long_utf8_err = "中".repeat(2_000); // 6,000 字节；截断至 <= 4,096 且落在 3 字节边界即 4,095 字节（1,365 个 '中'）
    let truncated_err = "中".repeat(4_096 / 3);
    assert_eq!(truncated_err.len(), 4_095);

    let res_json = json_result(inv_json, "t1", json!({"k": "v"}));
    let digest_large = [0x0f; 32];
    let res_large = StoredToolResult {
        invocation_id: inv_large,
        name: "t2".to_owned(),
        payload: ResultPayload::Succeeded(SuccessContent::OutputTooLarge {
            digest: digest_large,
            byte_length: 70_000,
        }),
    };
    let res_failed = StoredToolResult {
        invocation_id: inv_failed,
        name: "t3".to_owned(),
        payload: ResultPayload::Failed(ToolError {
            code: "io_error".to_owned(),
            message: long_utf8_err,
        }),
    };
    let res_not_exec = StoredToolResult {
        invocation_id: inv_not_exec,
        name: "t4".to_owned(),
        payload: ResultPayload::NotExecuted,
    };
    let res_indet = StoredToolResult {
        invocation_id: inv_indet,
        name: "t5".to_owned(),
        payload: ResultPayload::Indeterminate(IndeterminateCause::Cancelled),
    };

    let expected_json_bytes = canonical_json(&json!({"k": "v"})).len();
    let expected_large_bytes = canonical_json(&json!({
        "outcome": "succeeded",
        "content_unavailable": "output_too_large",
        "digest": "0f".repeat(32),
        "byte_length": 70_000u64,
    }))
    .len();
    let expected_failed_bytes = canonical_json(&json!({
        "error": { "code": "io_error", "message": truncated_err }
    }))
    .len();
    let expected_not_exec_bytes = canonical_json(&json!({"outcome": "not_executed"})).len();
    let expected_indet_bytes =
        canonical_json(&json!({"outcome": "indeterminate", "cause": "cancelled"})).len();
    let expected_tool_result_bytes = expected_json_bytes
        + expected_large_bytes
        + expected_failed_bytes
        + expected_not_exec_bytes
        + expected_indet_bytes;

    let step = step_with_parts(
        None,
        vec![
            reasoning_part("推理", None),
            text_part("正文"),
            MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: bound_call(inv_json, "t1", 2, json!({})),
            }),
            MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: bound_call(inv_large, "t2", 3, json!({})),
            }),
            MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: bound_call(inv_failed, "t3", 4, json!({})),
            }),
            MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: bound_call(inv_not_exec, "t4", 5, json!({})),
            }),
            MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: bound_call(inv_indet, "t5", 6, json!({})),
            }),
        ],
        vec![res_json, res_large, res_failed, res_not_exec, res_indet],
    );

    let op = ContextOp::new(ch("cli"), vec![ContextEdit::omit_node(NodeId(3))]).unwrap();

    let ctx = resolve(&[
        PathRecord::Input {
            id: NodeId(1),
            input: &input_1,
        },
        PathRecord::Step {
            id: NodeId(2),
            content: step,
        },
        PathRecord::Input {
            id: NodeId(3),
            input: &input_3,
        },
        PathRecord::Input {
            id: NodeId(4),
            input: &input_2,
        },
        PathRecord::Op {
            id: NodeId(5),
            op: &op,
        },
    ])
    .expect("归约应成功");

    let inspection = ctx.inspect();
    assert_eq!(
        inspection,
        ContextInspection {
            entries: vec![
                EffectiveEntrySummary::Input {
                    node: NodeId(1),
                    channel: ch("tui"),
                    text_bytes: "你好".len(),
                    image_count: 2,
                    image_bytes: 200,
                },
                EffectiveEntrySummary::Step {
                    node: NodeId(2),
                    text_bytes: "正文".len(),
                    reasoning_bytes: "推理".len(),
                    tool_call_count: 5,
                    tool_result_bytes: expected_tool_result_bytes,
                },
                EffectiveEntrySummary::Input {
                    node: NodeId(4),
                    channel: ch("tui"),
                    text_bytes: 0,
                    image_count: 1,
                    image_bytes: 300,
                },
            ],
            operated_nodes: vec![OperatedNode {
                node: NodeId(3),
                status: NodeStatus::Omitted,
            }],
            channel_counts: BTreeMap::from([(ch("tui"), 2)]),
            total_text_bytes: "你好".len()
                + "正文".len()
                + "推理".len()
                + expected_tool_result_bytes,
            images: vec![img_a, img_a, img_b],
            total_image_bytes: 500,
        }
    );
}

// ============================================================================
// 5. project() 多模态包装形态、属性转义与正文闭合标记转义黄金测试
// ============================================================================

#[test]
fn project_wraps_all_multimodal_shapes_and_escapes_channel_and_close_tags() {
    let img_1 = img(1, 100);
    let img_2 = img(2, 200);

    // 1) 单文本 + 特殊字符 Channel + 正文闭合标记转义（含首尾空行与多层反斜杠）
    let ch_special = ch("web:https://a.test/?x=1&y=\"2\"<3>");
    let body = "\n</lattice-context>\n\\</lattice-context>\n </lattice-context>\n</lattice-context> \n正文\n";
    let expected_body = "\n\\</lattice-context>\n\\\\</lattice-context>\n </lattice-context>\n</lattice-context> \n正文\n";
    let open_special =
        "<lattice-context channel=\"web:https://a.test/?x=1&amp;y=&quot;2&quot;&lt;3&gt;\">";

    let input_text = ContextInput::new(ch_special, vec![text_sem(body)]).unwrap();
    let input_img_only = ContextInput::new(ch("screenshot"), vec![image_sem(img_1)]).unwrap();
    let input_text_img = ContextInput::new(
        ch("tui"),
        vec![text_sem("看图\n</lattice-context>"), image_sem(img_1)],
    )
    .unwrap();
    let input_img_text = ContextInput::new(
        ch("tui"),
        vec![image_sem(img_1), text_sem("</lattice-context>\n说明")],
    )
    .unwrap();
    let input_text_img_text = ContextInput::new(
        ch("tui"),
        vec![text_sem("前文"), image_sem(img_1), text_sem("后文")],
    )
    .unwrap();
    let input_img_text_img = ContextInput::new(
        ch("tui"),
        vec![
            image_sem(img_1),
            text_sem("</lattice-context>"),
            image_sem(img_2),
        ],
    )
    .unwrap();

    let ctx = resolve(&[
        PathRecord::Input {
            id: NodeId(1),
            input: &input_text,
        },
        PathRecord::Input {
            id: NodeId(2),
            input: &input_img_only,
        },
        PathRecord::Input {
            id: NodeId(3),
            input: &input_text_img,
        },
        PathRecord::Input {
            id: NodeId(4),
            input: &input_img_text,
        },
        PathRecord::Input {
            id: NodeId(5),
            input: &input_text_img_text,
        },
        PathRecord::Input {
            id: NodeId(6),
            input: &input_img_text_img,
        },
    ])
    .expect("归约应成功");

    let projected = ctx.project();
    assert_eq!(
        projected,
        vec![
            // [Text(t)]
            ConversationItem::Message(NativeMessage::new(
                MessageRole::User,
                None,
                vec![text_part(&format!(
                    "{open_special}\n{expected_body}\n</lattice-context>"
                ))],
            )),
            // [Image(I)]
            ConversationItem::Message(NativeMessage::new(
                MessageRole::User,
                None,
                vec![
                    text_part("<lattice-context channel=\"screenshot\">"),
                    MessagePart::semantic_only(image_sem(img_1)),
                    text_part("</lattice-context>"),
                ],
            )),
            // [Text(t), Image(I)]
            ConversationItem::Message(NativeMessage::new(
                MessageRole::User,
                None,
                vec![
                    text_part("<lattice-context channel=\"tui\">\n看图\n\\</lattice-context>"),
                    MessagePart::semantic_only(image_sem(img_1)),
                    text_part("</lattice-context>"),
                ],
            )),
            // [Image(I), Text(t)]
            ConversationItem::Message(NativeMessage::new(
                MessageRole::User,
                None,
                vec![
                    text_part("<lattice-context channel=\"tui\">"),
                    MessagePart::semantic_only(image_sem(img_1)),
                    text_part("\\</lattice-context>\n说明\n</lattice-context>"),
                ],
            )),
            // [Text(t1), Image(I), Text(t2)]
            ConversationItem::Message(NativeMessage::new(
                MessageRole::User,
                None,
                vec![
                    text_part("<lattice-context channel=\"tui\">\n前文"),
                    MessagePart::semantic_only(image_sem(img_1)),
                    text_part("后文\n</lattice-context>"),
                ],
            )),
            // [Image(I1), Text(tm), Image(I2)]
            ConversationItem::Message(NativeMessage::new(
                MessageRole::User,
                None,
                vec![
                    text_part("<lattice-context channel=\"tui\">"),
                    MessagePart::semantic_only(image_sem(img_1)),
                    text_part("\\</lattice-context>"),
                    MessagePart::semantic_only(image_sem(img_2)),
                    text_part("</lattice-context>"),
                ],
            )),
        ],
        "全部多模态包装形态、属性转义与正文闭合标记转义必须精确匹配黄金输出"
    );

    // 经 ReplaceImage 替换为闭合标记文本时，投影阶段同样执行转义且重复归约结果确定一致
    let op_close_tag = ContextOp::new(
        ch("cli"),
        vec![
            ContextEdit::replace_image(NodeId(2), img_1, Some("</lattice-context>".to_owned()))
                .unwrap(),
        ],
    )
    .unwrap();
    let path_with_replace = [
        PathRecord::Input {
            id: NodeId(2),
            input: &input_img_only,
        },
        PathRecord::Op {
            id: NodeId(7),
            op: &op_close_tag,
        },
    ];
    let first = resolve(&path_with_replace).expect("第一次归约应成功");
    let second = resolve(&path_with_replace).expect("第二次归约应成功");
    assert_eq!(first.project(), second.project(), "纯函数投影必须确定一致");
    assert_eq!(
        first.project(),
        vec![ConversationItem::Message(NativeMessage::new(
            MessageRole::User,
            None,
            vec![text_part(
                "<lattice-context channel=\"screenshot\">\n\\</lattice-context>\n</lattice-context>"
            )],
        ))]
    );
}
