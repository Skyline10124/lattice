// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 五种协议纯文本请求字节的逐字节对照测试。

use lattice_conversation::{
    BoundToolCall, ConversationItem, Effort, InvocationId, MessagePart, MessageRole,
    MessageSemantic, ModelMessageAddress, NativeMessage, Protocol, ProviderId, ReplayOrigin,
    ResultPayload, SessionId, StepContent, StoredToolResult, SuccessContent, ToolCallAddress,
    ToolError, TurnId, UnboundToolCall, canonical_json, tool_content_digest,
};
use serde_json::json;

use super::ProtocolParser;
use crate::protocol::{
    Cache, ChatCompletionsFeatures, EffortParam, Features, MessagesFeatures, ReasoningReplay,
    ThinkingBudget, ThinkingToggle,
};
use crate::reasoning::ReasoningPlan;
use crate::types::{ChatRequest, ModelTarget, ToolDefinition};

struct GoldenCase {
    name: &'static str,
    protocol: Protocol,
    request: ChatRequest,
    output_limit: Option<u64>,
    model_reasoning: Option<bool>,
    features: Features,
    reasoning: Option<ReasoningPlan>,
    origin: ReplayOrigin,
}

fn origin_for(protocol: Protocol, provider: &str) -> ReplayOrigin {
    ReplayOrigin {
        protocol,
        provider_id: ProviderId::parse(provider).expect("测试标识合法"),
    }
}

fn base_request(messages: Vec<ConversationItem>) -> ChatRequest {
    let mut req = ChatRequest::new(messages, ModelTarget::test_target("test", "test-model"));
    req.client_request_id = Some("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c".into());
    req
}

fn text_message(role: MessageRole, text: &str) -> ConversationItem {
    ConversationItem::Message(NativeMessage::new(
        role,
        None,
        vec![MessagePart::semantic_only(MessageSemantic::Text {
            text: text.into(),
        })],
    ))
}

fn sample_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new(
            "get_weather",
            "查询指定城市的实时天气",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "city": {"type": "string"},
                    "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}
                },
                "required": ["city"]
            }),
        ),
        ToolDefinition::new(
            "get_time",
            "查询指定时区的当前时间",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "timezone": {"type": "string"}
                },
                "required": ["timezone"]
            }),
        ),
    ]
}

fn bind_call(
    ordinal: u32,
    call_id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> BoundToolCall {
    let invocation_id = InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}"))
        .expect("测试调用标识合法");
    UnboundToolCall {
        provider_call_id: Some(call_id.into()),
        name: name.into(),
        arguments,
    }
    .bind(
        invocation_id,
        ToolCallAddress {
            message: ModelMessageAddress {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c")
                    .expect("会话标识合法"),
                turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c")
                    .expect("轮次标识合法"),
                model_step_ordinal: 0,
            },
            part_ordinal: ordinal,
        },
    )
}

fn succeeded_result(
    invocation_id: InvocationId,
    name: &str,
    value: serde_json::Value,
) -> StoredToolResult {
    let bytes = canonical_json(&value);
    StoredToolResult {
        invocation_id,
        name: name.into(),
        payload: ResultPayload::Succeeded(SuccessContent::Json {
            value,
            digest: tool_content_digest(&bytes),
            byte_length: bytes.len() as u64,
        }),
    }
}

fn failed_result(
    invocation_id: InvocationId,
    name: &str,
    code: &str,
    message: &str,
) -> StoredToolResult {
    StoredToolResult {
        invocation_id,
        name: name.into(),
        payload: ResultPayload::Failed(ToolError {
            code: code.into(),
            message: message.into(),
        }),
    }
}

fn tools_and_results_request(protocol: Protocol) -> ChatRequest {
    let call_weather = bind_call(
        1,
        "call_weather_01",
        "get_weather",
        json!({"city": "Tokyo", "unit": "celsius"}),
    );
    let call_time = bind_call(2, "call_time_02", "get_time", json!({"timezone": "JST"}));
    let weather_inv = call_weather.invocation_id;
    let time_inv = call_time.invocation_id;

    let assistant = NativeMessage::new(
        MessageRole::Assistant,
        Some(origin_for(protocol, "other")),
        vec![
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "先并发查询天气和时间。".into(),
            }),
            MessagePart::semantic_only(MessageSemantic::ToolCall { call: call_weather }),
            MessagePart::semantic_only(MessageSemantic::ToolCall { call: call_time }),
        ],
    );
    let results = vec![
        succeeded_result(
            weather_inv,
            "get_weather",
            json!({"city": "Tokyo", "temperature": 21}),
        ),
        failed_result(time_inv, "get_time", "timeout", "时区服务连接超时"),
    ];

    let mut req = base_request(vec![
        text_message(MessageRole::System, "工具失败时向用户如实说明。"),
        text_message(MessageRole::User, "东京现在的天气和时间是什么？"),
        ConversationItem::Step(StepContent { assistant, results }),
        text_message(MessageRole::User, "那就先总结天气结果。"),
    ]);
    req.tools = sample_tools();
    req
}

fn replay_step_from_fixture(protocol: Protocol, provider: &str, fixture_json: &str) -> StepContent {
    let source_origin = origin_for(protocol, provider);
    let wire: serde_json::Value = serde_json::from_str(fixture_json).expect("官方夹具解析成功");
    let unbound = crate::protocol::projection::wire_to_native(&source_origin, &wire)
        .expect("夹具解码为中立消息成功");
    let (assistant, results) = crate::test_reply::bind_replay_step(unbound);
    StepContent { assistant, results }
}

fn replay_fixture_for(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::ChatCompletions => {
            include_str!("../../tests/fixtures/projection/openai_assistant_tool_calls.json")
        }
        Protocol::Messages => {
            include_str!(
                "../../tests/fixtures/projection/anthropic_redacted_thinking_tool_use.json"
            )
        }
        Protocol::Responses => {
            include_str!("../../tests/fixtures/projection/responses_output.json")
        }
        Protocol::Interactions => {
            include_str!("../../tests/fixtures/projection/interactions_output.json")
        }
        Protocol::GenerateContent => {
            include_str!("../../tests/fixtures/projection/gemini_thought_signature.json")
        }
    }
}

fn protocol_prefix(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::ChatCompletions => "chat_completions",
        Protocol::Messages => "anthropic_messages",
        Protocol::Responses => "responses",
        Protocol::Interactions => "interactions",
        Protocol::GenerateContent => "generate_content",
    }
}

fn golden_cases() -> Vec<GoldenCase> {
    let mut cases = Vec::new();

    for protocol in [
        Protocol::ChatCompletions,
        Protocol::Messages,
        Protocol::Responses,
        Protocol::Interactions,
        Protocol::GenerateContent,
    ] {
        let prefix = protocol_prefix(protocol);
        let origin = origin_for(protocol, "test");

        // 1. 系统提示合并与单条用户文本。
        cases.push(GoldenCase {
            name: Box::leak(format!("{prefix}_system_and_user").into_boxed_str()),
            protocol,
            request: base_request(vec![
                text_message(MessageRole::System, "你是严谨的代码助手。"),
                text_message(MessageRole::System, "回答保持简洁。"),
                text_message(MessageRole::User, "请检查当前目录。"),
            ]),
            output_limit: None,
            model_reasoning: None,
            features: Features::defaults(protocol),
            reasoning: None,
            origin: origin.clone(),
        });

        // 2. 工具定义、助手调用、成功与失败工具结果，再接用户文本。
        cases.push(GoldenCase {
            name: Box::leak(format!("{prefix}_tools_and_results").into_boxed_str()),
            protocol,
            request: tools_and_results_request(protocol),
            output_limit: None,
            model_reasoning: None,
            features: Features::defaults(protocol),
            reasoning: None,
            origin: origin.clone(),
        });

        // 3a. 同来源回放（保留推理、签名、加密内容等不透明回放状态）。
        let same_step = replay_step_from_fixture(protocol, "test", replay_fixture_for(protocol));
        let same_features = match protocol {
            Protocol::ChatCompletions => Features::ChatCompletions(ChatCompletionsFeatures {
                reasoning_replay: ReasoningReplay::ReasoningContent,
                ..Default::default()
            }),
            _ => Features::defaults(protocol),
        };
        let mut same_req = base_request(vec![
            text_message(MessageRole::User, "查一下天气和时间"),
            ConversationItem::Step(same_step),
            text_message(MessageRole::User, "继续说明"),
        ]);
        same_req.tools = sample_tools();
        cases.push(GoldenCase {
            name: Box::leak(format!("{prefix}_replay_same_origin").into_boxed_str()),
            protocol,
            request: same_req,
            output_limit: None,
            model_reasoning: Some(true),
            features: same_features,
            reasoning: None,
            origin: origin.clone(),
        });

        // 3b. 跨来源回放（裁剪推理与逐片段回放状态，保留调用标识与正文）。
        let cross_step = replay_step_from_fixture(protocol, "other", replay_fixture_for(protocol));
        let mut cross_req = base_request(vec![
            text_message(MessageRole::User, "查一下天气和时间"),
            ConversationItem::Step(cross_step),
            text_message(MessageRole::User, "继续说明"),
        ]);
        cross_req.tools = sample_tools();
        cases.push(GoldenCase {
            name: Box::leak(format!("{prefix}_replay_cross_origin").into_boxed_str()),
            protocol,
            request: cross_req,
            output_limit: None,
            model_reasoning: None,
            features: same_features,
            reasoning: None,
            origin,
        });
    }

    // Chat Completions 专属 features 与推理参数组合。
    let cc_origin = origin_for(Protocol::ChatCompletions, "test");
    let cc_step = replay_step_from_fixture(
        Protocol::ChatCompletions,
        "test",
        replay_fixture_for(Protocol::ChatCompletions),
    );
    cases.push(GoldenCase {
        name: "chat_completions_replay_content_chunks",
        protocol: Protocol::ChatCompletions,
        request: base_request(vec![
            text_message(MessageRole::User, "查一下天气和时间"),
            ConversationItem::Step(cc_step),
            text_message(MessageRole::User, "继续说明"),
        ]),
        output_limit: None,
        model_reasoning: None,
        features: Features::ChatCompletions(ChatCompletionsFeatures {
            reasoning_replay: ReasoningReplay::ContentChunks,
            ..Default::default()
        }),
        reasoning: None,
        origin: cc_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "chat_completions_cache_and_reasoning_effort",
        protocol: Protocol::ChatCompletions,
        request: base_request(vec![
            text_message(MessageRole::System, "系统指令"),
            text_message(MessageRole::User, "开启显式缓存与推理档位"),
        ]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::ChatCompletions(ChatCompletionsFeatures {
            effort_param: EffortParam::ReasoningEffort,
            thinking_toggle: ThinkingToggle::ThinkingType,
            thinking_budget: ThinkingBudget::None,
            reasoning_replay: ReasoningReplay::None,
            cache_markers: true,
        }),
        reasoning: Some(ReasoningPlan::Effort {
            effort: Effort::High,
            enable_toggle: true,
        }),
        origin: cc_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "chat_completions_reasoning_object_and_enable_thinking",
        protocol: Protocol::ChatCompletions,
        request: base_request(vec![text_message(MessageRole::User, "使用推理对象参数")]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::ChatCompletions(ChatCompletionsFeatures {
            effort_param: EffortParam::ReasoningObject,
            thinking_toggle: ThinkingToggle::EnableThinking,
            thinking_budget: ThinkingBudget::None,
            reasoning_replay: ReasoningReplay::None,
            cache_markers: false,
        }),
        reasoning: Some(ReasoningPlan::Effort {
            effort: Effort::High,
            enable_toggle: true,
        }),
        origin: cc_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "chat_completions_thinking_disabled",
        protocol: Protocol::ChatCompletions,
        request: base_request(vec![text_message(MessageRole::User, "关闭思考开关")]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::ChatCompletions(ChatCompletionsFeatures {
            thinking_toggle: ThinkingToggle::ThinkingType,
            ..Default::default()
        }),
        reasoning: Some(ReasoningPlan::Toggle { enabled: false }),
        origin: cc_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "chat_completions_thinking_budget",
        protocol: Protocol::ChatCompletions,
        request: base_request(vec![text_message(MessageRole::User, "顶层思考预算")]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::ChatCompletions(ChatCompletionsFeatures {
            thinking_toggle: ThinkingToggle::EnableThinking,
            thinking_budget: ThinkingBudget::ThinkingBudget,
            ..Default::default()
        }),
        reasoning: Some(ReasoningPlan::Budget {
            effort: Effort::Medium,
            tokens: 4096,
            min: None,
            enable_toggle: true,
        }),
        origin: cc_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "chat_completions_reasoning_max_tokens",
        protocol: Protocol::ChatCompletions,
        request: base_request(vec![text_message(MessageRole::User, "对象内最大推理预算")]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::ChatCompletions(ChatCompletionsFeatures {
            thinking_budget: ThinkingBudget::ReasoningMaxTokens,
            ..Default::default()
        }),
        reasoning: Some(ReasoningPlan::Budget {
            effort: Effort::High,
            tokens: 8192,
            min: None,
            enable_toggle: false,
        }),
        origin: cc_origin,
    });

    // Messages 专属 features、推理参数与空用户消息。
    let msg_origin = origin_for(Protocol::Messages, "test");
    cases.push(GoldenCase {
        name: "anthropic_messages_cache_and_adaptive_thinking",
        protocol: Protocol::Messages,
        request: base_request(vec![
            text_message(MessageRole::System, "系统缓存断点"),
            text_message(MessageRole::User, "自适应思考与输出配置"),
        ]),
        output_limit: Some(16_384),
        model_reasoning: Some(true),
        features: Features::Messages(MessagesFeatures {
            cache: Cache::Automatic,
        }),
        reasoning: Some(ReasoningPlan::Effort {
            effort: Effort::High,
            enable_toggle: false,
        }),
        origin: msg_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "anthropic_messages_budget_thinking_no_cache",
        protocol: Protocol::Messages,
        request: base_request(vec![
            text_message(MessageRole::System, "无缓存系统提示"),
            text_message(MessageRole::User, "按预算开启思考"),
        ]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::Messages(MessagesFeatures { cache: Cache::None }),
        reasoning: Some(ReasoningPlan::Budget {
            effort: Effort::Medium,
            tokens: 4096,
            min: None,
            enable_toggle: false,
        }),
        origin: msg_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "anthropic_messages_empty_user",
        protocol: Protocol::Messages,
        request: base_request(vec![text_message(MessageRole::User, "")]),
        output_limit: Some(4096),
        model_reasoning: Some(false),
        features: Features::Messages(MessagesFeatures { cache: Cache::None }),
        reasoning: Some(ReasoningPlan::Toggle { enabled: false }),
        origin: msg_origin,
    });

    // Responses 专属 effort 与 include。
    let resp_origin = origin_for(Protocol::Responses, "test");
    cases.push(GoldenCase {
        name: "responses_effort_and_include",
        protocol: Protocol::Responses,
        request: base_request(vec![
            text_message(MessageRole::System, "回答准确"),
            text_message(MessageRole::User, "开启高推理档位与加密回放"),
        ]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::Responses,
        reasoning: Some(ReasoningPlan::Effort {
            effort: Effort::High,
            enable_toggle: false,
        }),
        origin: resp_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "responses_effort_off_and_include",
        protocol: Protocol::Responses,
        request: base_request(vec![text_message(
            MessageRole::User,
            "关闭推理档位但保留加密头",
        )]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::Responses,
        reasoning: Some(ReasoningPlan::Effort {
            effort: Effort::Off,
            enable_toggle: false,
        }),
        origin: resp_origin,
    });

    // Interactions 专属 thinking_level 与 thinking_summaries。
    let inter_origin = origin_for(Protocol::Interactions, "test");
    cases.push(GoldenCase {
        name: "interactions_thinking_level_and_summaries",
        protocol: Protocol::Interactions,
        request: base_request(vec![
            text_message(MessageRole::System, "保持简洁"),
            text_message(MessageRole::User, "测试思考等级与摘要"),
        ]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::Interactions,
        reasoning: Some(ReasoningPlan::Effort {
            effort: Effort::Medium,
            enable_toggle: false,
        }),
        origin: inter_origin,
    });

    // generateContent 专属 thinkingBudget 与 thinkingConfig。
    let gc_origin = origin_for(Protocol::GenerateContent, "test");
    cases.push(GoldenCase {
        name: "generate_content_thinking_budget_and_config",
        protocol: Protocol::GenerateContent,
        request: base_request(vec![
            text_message(MessageRole::System, "系统指令"),
            text_message(MessageRole::User, "开启思考预算与返回思考正文"),
        ]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::GenerateContent,
        reasoning: Some(ReasoningPlan::Budget {
            effort: Effort::Low,
            tokens: 2048,
            min: None,
            enable_toggle: false,
        }),
        origin: gc_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "generate_content_thinking_level",
        protocol: Protocol::GenerateContent,
        request: base_request(vec![text_message(MessageRole::User, "按档位配置思考")]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::GenerateContent,
        reasoning: Some(ReasoningPlan::Effort {
            effort: Effort::High,
            enable_toggle: false,
        }),
        origin: gc_origin.clone(),
    });
    cases.push(GoldenCase {
        name: "generate_content_thinking_off",
        protocol: Protocol::GenerateContent,
        request: base_request(vec![text_message(MessageRole::User, "零预算关闭思考")]),
        output_limit: None,
        model_reasoning: Some(true),
        features: Features::GenerateContent,
        reasoning: Some(ReasoningPlan::Effort {
            effort: Effort::Off,
            enable_toggle: false,
        }),
        origin: gc_origin,
    });

    cases
}

#[test]
fn plain_text_request_bytes_match_golden_fixtures() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("request_bytes");

    let cases = golden_cases();
    assert_eq!(cases.len(), 35, "纯文本 golden 用例数量应固定");
    for case in cases {
        let body = ProtocolParser::new(case.protocol)
            .request_body(
                &case.request,
                case.output_limit,
                case.model_reasoning,
                case.features,
                case.reasoning,
                &case.origin,
            )
            .expect("构造请求体应成功");
        let expected_len = crate::protocol::encoding::encoded_len(&body).expect("请求计长应成功");
        let actual = crate::protocol::encoding::encode_body_without_images(&body, expected_len)
            .expect("流式写出请求字节应成功");
        assert_eq!(
            actual.len(),
            expected_len,
            "纯文本计长与实际写出长度不一致（{}）",
            case.name
        );
        let path = dir.join(format!("{}.json", case.name));
        let expected = std::fs::read(&path).expect("读取 golden 夹具文件应成功");
        assert_eq!(
            actual.as_slice(),
            expected.as_slice(),
            "请求字节与固定夹具不一致（{}）\n实际：{}\n期望：{}",
            case.name,
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(&expected),
        );
    }
}

const SAMPLE_PNG_16: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
const SAMPLE_PNG_16_B64: &str = "iVBORw0KGgoAAAANSUhEUg==";

fn make_image_ref(bytes: &[u8]) -> (lattice_conversation::ImageRef, Vec<u8>) {
    use sha2::{Digest, Sha256};
    let media_type =
        lattice_conversation::ImageMediaType::sniff(bytes).expect("测试图片头魔数应有效");
    let digest = Sha256::digest(bytes);
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&digest);
    (
        lattice_conversation::ImageRef {
            sha256,
            media_type,
            byte_length: bytes.len() as u64,
        },
        bytes.to_vec(),
    )
}

#[derive(Default)]
struct MapImageSource {
    blobs: std::collections::BTreeMap<[u8; 32], Vec<u8>>,
}

impl MapImageSource {
    fn insert(&mut self, image: &lattice_conversation::ImageRef, bytes: Vec<u8>) {
        self.blobs.insert(image.sha256, bytes);
    }
}

impl crate::ImageSource for MapImageSource {
    fn with_reader(
        &self,
        encode: &mut dyn FnMut(&dyn crate::ImageReader) -> Result<(), crate::LlmError>,
    ) -> Result<(), crate::LlmError> {
        encode(self)
    }
}

impl crate::ImageReader for MapImageSource {
    fn with_image(
        &self,
        image: &lattice_conversation::ImageRef,
        consume: &mut dyn FnMut(&[u8]) -> Result<(), crate::LlmError>,
    ) -> Result<(), crate::LlmError> {
        let Some(bytes) = self.blobs.get(&image.sha256) else {
            return Err(crate::LlmError::Other(format!(
                "测试图片来源缺失图片 {}",
                crate::image_source::short_image_digest(&image.sha256)
            )));
        };
        consume(bytes)
    }
}

#[test]
fn user_image_requests_write_exact_handwritten_json_bytes_across_five_protocols() {
    assert_eq!(SAMPLE_PNG_16.len(), 16, "样例图片长度固定为 16 字节");
    let (png_ref, png_bytes) = make_image_ref(SAMPLE_PNG_16);
    assert_eq!(
        png_ref.media_type,
        lattice_conversation::ImageMediaType::Png
    );
    let mut source = MapImageSource::default();
    source.insert(&png_ref, png_bytes);

    let interleaved_user = ConversationItem::Message(NativeMessage::new(
        MessageRole::User,
        None,
        vec![
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "前文".into(),
            }),
            MessagePart::semantic_only(MessageSemantic::Image { image: png_ref }),
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "后文".into(),
            }),
        ],
    ));
    let image_only_user = ConversationItem::Message(NativeMessage::new(
        MessageRole::User,
        None,
        vec![MessagePart::semantic_only(MessageSemantic::Image {
            image: png_ref,
        })],
    ));

    let expected_cases: &[(Protocol, &str, ConversationItem, Option<u64>, &str)] = &[
        (
            Protocol::ChatCompletions,
            "chat_completions_interleaved",
            interleaved_user.clone(),
            None,
            r#"{"messages":[{"content":[{"text":"前文","type":"text"},{"image_url":{"url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUg=="},"type":"image_url"},{"text":"后文","type":"text"}],"role":"user"}],"model":"test-model","stream":true,"stream_options":{"include_usage":true}}"#,
        ),
        (
            Protocol::ChatCompletions,
            "chat_completions_image_only",
            image_only_user.clone(),
            None,
            r#"{"messages":[{"content":[{"image_url":{"url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUg=="},"type":"image_url"}],"role":"user"}],"model":"test-model","stream":true,"stream_options":{"include_usage":true}}"#,
        ),
        (
            Protocol::Responses,
            "responses_interleaved",
            interleaved_user.clone(),
            None,
            r#"{"input":[{"content":[{"text":"前文","type":"input_text"},{"detail":"auto","image_url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==","type":"input_image"},{"text":"后文","type":"input_text"}],"role":"user"}],"model":"test-model","prompt_cache_key":"sess-1","store":false,"stream":true}"#,
        ),
        (
            Protocol::Responses,
            "responses_image_only",
            image_only_user.clone(),
            None,
            r#"{"input":[{"content":[{"detail":"auto","image_url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==","type":"input_image"}],"role":"user"}],"model":"test-model","prompt_cache_key":"sess-1","store":false,"stream":true}"#,
        ),
        (
            Protocol::Messages,
            "anthropic_messages_interleaved",
            interleaved_user.clone(),
            Some(4096),
            r#"{"cache_control":{"type":"ephemeral"},"max_tokens":4096,"messages":[{"content":[{"text":"前文","type":"text"},{"source":{"data":"iVBORw0KGgoAAAANSUhEUg==","media_type":"image/png","type":"base64"},"type":"image"},{"text":"后文","type":"text"}],"role":"user"}],"model":"test-model","stream":true}"#,
        ),
        (
            Protocol::Messages,
            "anthropic_messages_image_only",
            image_only_user.clone(),
            Some(4096),
            r#"{"cache_control":{"type":"ephemeral"},"max_tokens":4096,"messages":[{"content":[{"source":{"data":"iVBORw0KGgoAAAANSUhEUg==","media_type":"image/png","type":"base64"},"type":"image"}],"role":"user"}],"model":"test-model","stream":true}"#,
        ),
        (
            Protocol::GenerateContent,
            "generate_content_interleaved",
            interleaved_user.clone(),
            None,
            r#"{"contents":[{"parts":[{"text":"前文"},{"inlineData":{"data":"iVBORw0KGgoAAAANSUhEUg==","mimeType":"image/png"}},{"text":"后文"}],"role":"user"}]}"#,
        ),
        (
            Protocol::GenerateContent,
            "generate_content_image_only",
            image_only_user.clone(),
            None,
            r#"{"contents":[{"parts":[{"inlineData":{"data":"iVBORw0KGgoAAAANSUhEUg==","mimeType":"image/png"}}],"role":"user"}]}"#,
        ),
        (
            Protocol::Interactions,
            "interactions_interleaved",
            interleaved_user,
            None,
            r#"{"input":[{"content":[{"text":"前文","type":"text"},{"data":"iVBORw0KGgoAAAANSUhEUg==","mime_type":"image/png","type":"image"},{"text":"后文","type":"text"}],"type":"user_input"}],"model":"test-model","store":false,"stream":true}"#,
        ),
        (
            Protocol::Interactions,
            "interactions_image_only",
            image_only_user,
            None,
            r#"{"input":[{"content":[{"data":"iVBORw0KGgoAAAANSUhEUg==","mime_type":"image/png","type":"image"}],"type":"user_input"}],"model":"test-model","store":false,"stream":true}"#,
        ),
    ];

    for (protocol, label, user_item, output_limit, expected_json) in expected_cases {
        assert!(
            expected_json.contains(SAMPLE_PNG_16_B64),
            "手写期望必须包含固定 base64（{label}）"
        );
        let mut req = ChatRequest::new(
            vec![user_item.clone()],
            ModelTarget::test_target("test", "test-model"),
        );
        req.client_request_id = Some("sess-1".into());
        let origin = origin_for(*protocol, "test");
        let body = ProtocolParser::new(*protocol)
            .request_body(
                &req,
                *output_limit,
                None,
                Features::defaults(*protocol),
                None,
                &origin,
            )
            .expect("含图请求投影应成功");
        let len = crate::protocol::encoding::encoded_len(&body).expect("含图请求计长应成功");
        let actual = crate::protocol::encoding::encode_body(&body, &source, len)
            .expect("含图请求编码应成功");
        assert_eq!(actual.len(), len, "含图请求计长与写出长度不一致（{label}）");
        assert_eq!(
            actual.as_slice(),
            expected_json.as_bytes(),
            "含图请求整包字节与手写期望不一致（{label}）\n实际：{}\n期望：{expected_json}",
            String::from_utf8_lossy(&actual),
        );
    }
}

#[test]
fn sizing_encoder_matches_streaming_blob_encoder_with_complex_histories_and_escapes() {
    // 覆盖 0、1、2 个 base64 填充号及四种图片格式。
    let (png_16, png_bytes) = make_image_ref(SAMPLE_PNG_16); // 16B -> "=="
    let (jpeg_12, jpeg_bytes) = make_image_ref(b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01"); // 12B -> 无填充
    let (webp_14, webp_bytes) = make_image_ref(b"RIFF\x06\x00\x00\x00WEBPVP"); // 14B -> "="
    let (gif_13, gif_bytes) = make_image_ref(b"GIF89a\x01\x00\x01\x00\x80\x00\x00"); // 13B -> "=="

    let mut source = MapImageSource::default();
    source.insert(&png_16, png_bytes);
    source.insert(&jpeg_12, jpeg_bytes);
    source.insert(&webp_14, webp_bytes);
    source.insert(&gif_13, gif_bytes);

    let mut heavy_escape = String::from("引号\"反斜杠\\换行\n回车\r制表\t退格\x08换页\x0c空字符\0");
    for c in 0x01u8..=0x1fu8 {
        heavy_escape.push(c as char);
    }
    heavy_escape.push_str("多字节中文与符号：界🦀𝄞\"\\\"\\\n\u{2028}\u{2029}");
    let heavy_escape = heavy_escape.repeat(16);

    for protocol in [
        Protocol::ChatCompletions,
        Protocol::Messages,
        Protocol::Responses,
        Protocol::Interactions,
        Protocol::GenerateContent,
    ] {
        let origin = origin_for(protocol, "test");
        let same_step = replay_step_from_fixture(protocol, "test", replay_fixture_for(protocol));

        let failed_call = bind_call(0, "call_err_99", "get_time", json!({"timezone": "UTC"}));
        let failed_inv = failed_call.invocation_id;
        let failed_step = StepContent {
            assistant: NativeMessage::new(
                MessageRole::Assistant,
                Some(origin_for(protocol, "other")),
                vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                    call: failed_call,
                })],
            ),
            results: vec![failed_result(
                failed_inv,
                "get_time",
                "bad_Escape",
                &heavy_escape,
            )],
        };

        let first_user = ConversationItem::Message(NativeMessage::new(
            MessageRole::User,
            None,
            vec![
                MessagePart::semantic_only(MessageSemantic::Text {
                    text: heavy_escape.clone(),
                }),
                MessagePart::semantic_only(MessageSemantic::Image { image: png_16 }),
                MessagePart::semantic_only(MessageSemantic::Image { image: jpeg_12 }),
                MessagePart::semantic_only(MessageSemantic::Text {
                    text: heavy_escape.clone(),
                }),
            ],
        ));
        let final_user = ConversationItem::Message(NativeMessage::new(
            MessageRole::User,
            None,
            vec![
                MessagePart::semantic_only(MessageSemantic::Image { image: webp_14 }),
                MessagePart::semantic_only(MessageSemantic::Text {
                    text: heavy_escape.clone(),
                }),
                MessagePart::semantic_only(MessageSemantic::Image { image: gif_13 }),
                MessagePart::semantic_only(MessageSemantic::Text {
                    text: heavy_escape.clone(),
                }),
            ],
        ));

        let mut req = base_request(vec![
            text_message(MessageRole::System, &heavy_escape),
            first_user,
            ConversationItem::Step(same_step),
            ConversationItem::Step(failed_step),
            final_user,
        ]);
        req.tools = sample_tools();

        let (features, plan, output_limit) = match protocol {
            Protocol::ChatCompletions => (
                Features::ChatCompletions(ChatCompletionsFeatures {
                    effort_param: EffortParam::ReasoningObject,
                    thinking_toggle: ThinkingToggle::ThinkingType,
                    thinking_budget: ThinkingBudget::None,
                    reasoning_replay: ReasoningReplay::ReasoningContent,
                    cache_markers: true,
                }),
                Some(ReasoningPlan::Effort {
                    effort: Effort::High,
                    enable_toggle: true,
                }),
                None,
            ),
            Protocol::Messages => (
                Features::Messages(MessagesFeatures {
                    cache: Cache::Automatic,
                }),
                Some(ReasoningPlan::Effort {
                    effort: Effort::High,
                    enable_toggle: false,
                }),
                Some(16_384),
            ),
            Protocol::Responses => (
                Features::Responses,
                Some(ReasoningPlan::Effort {
                    effort: Effort::High,
                    enable_toggle: false,
                }),
                None,
            ),
            Protocol::Interactions => (
                Features::Interactions,
                Some(ReasoningPlan::Effort {
                    effort: Effort::High,
                    enable_toggle: false,
                }),
                None,
            ),
            Protocol::GenerateContent => (
                Features::GenerateContent,
                Some(ReasoningPlan::Budget {
                    effort: Effort::High,
                    tokens: 4096,
                    min: None,
                    enable_toggle: false,
                }),
                None,
            ),
        };

        let body = ProtocolParser::new(protocol)
            .request_body(&req, output_limit, Some(true), features, plan, &origin)
            .expect("复杂含图历史构造请求体应成功");
        let sized = crate::protocol::encoding::encoded_len(&body).expect("复杂含图请求计长应成功");
        let encoded = crate::protocol::encoding::encode_body(&body, &source, sized)
            .expect("复杂含图请求流式写出应成功");
        assert_eq!(
            encoded.len(),
            sized,
            "{protocol:?} 复杂转义含图请求的计长与写出字节数必须完全一致"
        );
        let parsed: serde_json::Value =
            serde_json::from_slice(&encoded).expect("流式写出的字节必须是合法 JSON");
        assert!(parsed.is_object(), "{protocol:?} 顶层应为 JSON 对象");
    }
}

#[test]
fn chat_completions_cache_markers_only_mark_trailing_text_block_on_user_images() {
    let (png_ref, png_bytes) = make_image_ref(SAMPLE_PNG_16);
    let mut source = MapImageSource::default();
    source.insert(&png_ref, png_bytes);

    let features = Features::ChatCompletions(ChatCompletionsFeatures {
        cache_markers: true,
        ..Default::default()
    });
    let origin = origin_for(Protocol::ChatCompletions, "test");

    // 1. 末尾块为文本：只标记该末尾文本块，且键序为 cache_control、text、type。
    let trailing_text_req = base_request(vec![
        text_message(MessageRole::System, "系统提示"),
        ConversationItem::Message(NativeMessage::new(
            MessageRole::User,
            None,
            vec![
                MessagePart::semantic_only(MessageSemantic::Text {
                    text: "前段说明".into(),
                }),
                MessagePart::semantic_only(MessageSemantic::Image { image: png_ref }),
                MessagePart::semantic_only(MessageSemantic::Text {
                    text: "末尾说明".into(),
                }),
            ],
        )),
    ]);
    let body = ProtocolParser::new(Protocol::ChatCompletions)
        .request_body(&trailing_text_req, None, None, features, None, &origin)
        .unwrap();
    let len = crate::protocol::encoding::encoded_len(&body).unwrap();
    let bytes = crate::protocol::encoding::encode_body(&body, &source, len).unwrap();
    let raw_json = String::from_utf8(bytes.clone()).unwrap();
    assert!(
        raw_json
            .contains(r#"{"cache_control":{"type":"ephemeral"},"text":"末尾说明","type":"text"}"#),
        "末尾文本块应按字典序写入 cache_control、text、type：{raw_json}"
    );
    assert!(
        raw_json.contains(r#"{"text":"前段说明","type":"text"}"#),
        "前段文本块不应被添加缓存标记：{raw_json}"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        crate::test_reply::cache_controls(&parsed),
        vec![
            (
                "/messages/0/content/0/cache_control".into(),
                json!({"type": "ephemeral"})
            ),
            (
                "/messages/1/content/2/cache_control".into(),
                json!({"type": "ephemeral"})
            ),
        ],
        "末尾为文本时只有系统块与末条消息末块携带缓存标记"
    );

    // 2. 末尾块为图片（前有文本）及纯图片：不加标记，也不回退到前面的文本块。
    for user_parts in [
        vec![
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "图片前的文本".into(),
            }),
            MessagePart::semantic_only(MessageSemantic::Image { image: png_ref }),
        ],
        vec![MessagePart::semantic_only(MessageSemantic::Image {
            image: png_ref,
        })],
    ] {
        let req = base_request(vec![
            text_message(MessageRole::System, "系统提示"),
            ConversationItem::Message(NativeMessage::new(MessageRole::User, None, user_parts)),
        ]);
        let body = ProtocolParser::new(Protocol::ChatCompletions)
            .request_body(&req, None, None, features, None, &origin)
            .unwrap();
        let len = crate::protocol::encoding::encoded_len(&body).unwrap();
        let bytes = crate::protocol::encoding::encode_body(&body, &source, len).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            crate::test_reply::cache_controls(&parsed),
            vec![(
                "/messages/0/content/0/cache_control".into(),
                json!({"type": "ephemeral"})
            )],
            "末尾块为图片时用户消息内不得产生任何缓存标记"
        );
    }
}

#[test]
fn cross_provider_and_cross_protocol_replay_projects_user_images_to_target_protocol() {
    let (png_ref, png_bytes) = make_image_ref(SAMPLE_PNG_16);
    let mut source = MapImageSource::default();
    source.insert(&png_ref, png_bytes);

    let historical_step = replay_step_from_fixture(
        Protocol::Messages,
        "anthropic-source",
        include_str!("../../tests/fixtures/projection/anthropic_thinking_tool_use.json"),
    );
    let history = vec![
        ConversationItem::Message(NativeMessage::new(
            MessageRole::User,
            None,
            vec![
                MessagePart::semantic_only(MessageSemantic::Text {
                    text: "请结合截图查询天气".into(),
                }),
                MessagePart::semantic_only(MessageSemantic::Image { image: png_ref }),
            ],
        )),
        ConversationItem::Step(historical_step),
        text_message(MessageRole::User, "现在切换到新模型继续"),
    ];

    for target_protocol in [
        Protocol::ChatCompletions,
        Protocol::Responses,
        Protocol::Messages,
        Protocol::GenerateContent,
        Protocol::Interactions,
    ] {
        let target_origin = origin_for(target_protocol, "target-provider");
        let req = base_request(history.clone());
        let body = ProtocolParser::new(target_protocol)
            .request_body(
                &req,
                None,
                None,
                Features::defaults(target_protocol),
                None,
                &target_origin,
            )
            .expect("跨来源含图历史投影应成功");
        let len = crate::protocol::encoding::encoded_len(&body).unwrap();
        let bytes = crate::protocol::encoding::encode_body(&body, &source, len).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            text.contains(SAMPLE_PNG_16_B64),
            "{target_protocol:?} 跨来源回放必须保留并投影用户图片"
        );
        assert!(
            !text.contains("Need the weather first"),
            "{target_protocol:?} 跨来源回放必须裁剪旧来源推理内容"
        );
        assert!(
            !text.contains("ErUBCkYIAxgCIkD3c2FmZGVmNTQ2Ny0xMjM0LWFiY2QtZXhhbXBsZXNpZ25hdHVyZQ=="),
            "{target_protocol:?} 跨来源回放必须裁剪旧来源签名"
        );
    }
}

#[tokio::test]
async fn image_source_seam_enforces_no_images_length_check_blocking_thread_and_cancel() {
    use futures::StreamExt;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (png_ref, png_bytes) = make_image_ref(SAMPLE_PNG_16);
    let expected_short_digest = crate::image_source::short_image_digest(&png_ref.sha256);
    assert_eq!(
        expected_short_digest.len(),
        12,
        "诊断摘要固定为 12 位十六进制"
    );

    let image_chat_request = || {
        let mut req = ChatRequest::new(
            vec![ConversationItem::Message(NativeMessage::new(
                MessageRole::User,
                None,
                vec![
                    MessagePart::semantic_only(MessageSemantic::Text {
                        text: "看图".into(),
                    }),
                    MessagePart::semantic_only(MessageSemantic::Image { image: png_ref }),
                ],
            ))],
            ModelTarget::test_target("test", "model"),
        );
        req.client_request_id = Some("session-img".into());
        req
    };

    let plain_chat_request = || {
        let mut req = ChatRequest::new(
            vec![text_message(MessageRole::User, "纯文本消息")],
            ModelTarget::test_target("test", "model"),
        );
        req.client_request_id = Some("session-txt".into());
        req
    };

    let make_resolved = |url: &str| {
        let url = url.to_string();
        async move {
            let dir = tempfile::tempdir().unwrap();
            std::fs::set_permissions(
                dir.path(),
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .unwrap();
            std::fs::write(
                dir.path().join("providers.toml"),
                format!(
                    "[providers.test]\nprotocol = 'chat-completions'\nbase_url = '{url}'\nauth = 'none'\n"
                ),
            )
            .unwrap();
            let resolved = crate::RequestGateway::new(dir.path().into(), vec![])
                .unwrap()
                .open_turn()
                .resolve(&ModelTarget::test_target("test", "model"))
                .await
                .unwrap();
            (dir, resolved)
        }
    };

    // 1. NoImages 遇到含图请求返回不可重试中文 LlmError::Other，且不发起 HTTP。
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_dir, resolved) = make_resolved(&format!("http://{addr}")).await;
        let err = match resolved
            .stream_chat(
                image_chat_request(),
                Arc::new(crate::NoImages),
                crate::StreamControl::default(),
            )
            .await
        {
            Ok(_) => panic!("NoImages 处理含图请求必须失败"),
            Err(err) => err,
        };
        assert!(!err.retryable(), "NoImages 错误必须不可重试");
        let crate::LlmError::Other(msg) = &err else {
            panic!("应返回 LlmError::Other，实际：{err:?}");
        };
        assert!(
            msg.contains("当前图片来源无法读取图片") && msg.contains(&expected_short_digest),
            "诊断应为包含 12 位摘要的中文说明：{msg}"
        );
        assert_eq!(
            err.to_string(),
            format!("当前图片来源无法读取图片 {expected_short_digest}"),
            "最终显示的诊断应只有中文说明，不带英文前缀"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "NoImages 失败时不得发起任何 HTTP 连接"
        );
    }

    // 2. 读取器借出字节数与 ImageRef.byte_length 不符时编码失败，且不发起 HTTP。
    {
        struct MismatchedLengthSource(Vec<u8>);
        impl crate::ImageSource for MismatchedLengthSource {
            fn with_reader(
                &self,
                encode: &mut dyn FnMut(&dyn crate::ImageReader) -> Result<(), crate::LlmError>,
            ) -> Result<(), crate::LlmError> {
                encode(self)
            }
        }
        impl crate::ImageReader for MismatchedLengthSource {
            fn with_image(
                &self,
                _image: &lattice_conversation::ImageRef,
                consume: &mut dyn FnMut(&[u8]) -> Result<(), crate::LlmError>,
            ) -> Result<(), crate::LlmError> {
                consume(&self.0)
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_dir, resolved) = make_resolved(&format!("http://{addr}")).await;
        let err = match resolved
            .stream_chat(
                image_chat_request(),
                Arc::new(MismatchedLengthSource(png_bytes[..15].to_vec())),
                crate::StreamControl::default(),
            )
            .await
        {
            Ok(_) => panic!("图片字节长度不符时必须编码失败"),
            Err(err) => err,
        };
        assert!(!err.retryable(), "长度不符错误必须不可重试");
        let crate::LlmError::Other(msg) = &err else {
            panic!("长度不符应返回 LlmError::Other，实际：{err:?}");
        };
        assert!(
            msg.contains(&expected_short_digest) && msg.contains("15") && msg.contains("16"),
            "诊断应包含 12 位摘要与实际/记录字节长度：{msg}"
        );
        assert!(
            !msg.contains("PNG") && !msg.contains(SAMPLE_PNG_16_B64),
            "诊断不得包含图片字节内容：{msg}"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "图片长度不符时不得发起任何 HTTP 连接"
        );
    }

    // 2b. 来源打开读取作用域时返回其他种类的错误（例如超时），按不可重试的 Other 报告且不发起 HTTP。
    {
        struct ScopeOpenTimeoutSource;
        impl crate::ImageSource for ScopeOpenTimeoutSource {
            fn with_reader(
                &self,
                _encode: &mut dyn FnMut(&dyn crate::ImageReader) -> Result<(), crate::LlmError>,
            ) -> Result<(), crate::LlmError> {
                Err(crate::LlmError::Timeout {
                    message: "image store read timed out".into(),
                })
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_dir, resolved) = make_resolved(&format!("http://{addr}")).await;
        let err = match resolved
            .stream_chat(
                image_chat_request(),
                Arc::new(ScopeOpenTimeoutSource),
                crate::StreamControl::default(),
            )
            .await
        {
            Ok(_) => panic!("图片来源打开读取作用域失败时必须报错"),
            Err(err) => err,
        };
        assert!(!err.retryable(), "图片来源返回的超时也必须按不可重试报告");
        let crate::LlmError::Other(msg) = &err else {
            panic!("图片来源失败应归一为 LlmError::Other，实际：{err:?}");
        };
        assert_eq!(
            msg, "读取图片来源失败：HTTP request timed out: image store read timed out",
            "诊断应以中文说明开头并保留来源的原始错误文本"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "图片来源失败时不得发起任何 HTTP 连接"
        );
    }

    // 3. 纯文本请求不调用 ImageSource（调用即计数并 panic）。
    {
        struct NeverCalledSource(AtomicUsize);
        impl crate::ImageSource for NeverCalledSource {
            fn with_reader(
                &self,
                _encode: &mut dyn FnMut(&dyn crate::ImageReader) -> Result<(), crate::LlmError>,
            ) -> Result<(), crate::LlmError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                panic!("纯文本请求不得访问 ImageSource");
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut tmp = [0u8; 1024];
                let n = sock.read(&mut tmp).await.unwrap();
                assert!(n > 0);
                buf.extend_from_slice(&tmp[..n]);
            }
            let headers_str = String::from_utf8_lossy(&buf).to_ascii_lowercase();
            assert!(
                headers_str.contains("content-type: application/json"),
                "请求头必须携带 Content-Type: application/json：{headers_str}"
            );
            let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"纯文本正常\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
                sse.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });

        let (_dir, resolved) = make_resolved(&format!("http://{addr}")).await;
        let guard_source = Arc::new(NeverCalledSource(AtomicUsize::new(0)));
        let events = resolved
            .stream_chat(
                plain_chat_request(),
                guard_source.clone(),
                crate::StreamControl::default(),
            )
            .await
            .expect("纯文本请求应成功打开流")
            .collect::<Vec<_>>()
            .await;
        assert_eq!(
            guard_source.0.load(Ordering::SeqCst),
            0,
            "纯文本请求调用图片来源次数必须为 0"
        );
        assert!(
            matches!(
                events.last(),
                Some(crate::LlmEvent::Finished {
                    outcome: crate::ReplyOutcome::Complete { .. },
                    ..
                })
            ),
            "纯文本流应完整结束：{events:?}"
        );
        server.await.unwrap();
    }

    // 4. 含图请求在 blocking 线程内完成编码并携带 Content-Type: application/json 发出。
    {
        struct BlockingProbeSource {
            bytes: Vec<u8>,
            thread_ids: Mutex<Vec<std::thread::ThreadId>>,
            blocking_verified: AtomicBool,
        }
        impl crate::ImageSource for BlockingProbeSource {
            fn with_reader(
                &self,
                encode: &mut dyn FnMut(&dyn crate::ImageReader) -> Result<(), crate::LlmError>,
            ) -> Result<(), crate::LlmError> {
                self.thread_ids
                    .lock()
                    .unwrap()
                    .push(std::thread::current().id());
                // 在异步工作线程调用 Handle::block_on 会立即 panic；只有 blocking 线程允许这样重入。
                let handle =
                    tokio::runtime::Handle::try_current().expect("blocking 池线程持有运行时句柄");
                handle.block_on(tokio::task::yield_now());
                self.blocking_verified.store(true, Ordering::Release);
                encode(self)
            }
        }
        impl crate::ImageReader for BlockingProbeSource {
            fn with_image(
                &self,
                _image: &lattice_conversation::ImageRef,
                consume: &mut dyn FnMut(&[u8]) -> Result<(), crate::LlmError>,
            ) -> Result<(), crate::LlmError> {
                self.thread_ids
                    .lock()
                    .unwrap()
                    .push(std::thread::current().id());
                consume(&self.bytes)
            }
        }

        let caller_tid = std::thread::current().id();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let header_end = loop {
                let mut tmp = [0u8; 1024];
                let n = sock.read(&mut tmp).await.unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&tmp[..n]);
                if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let header_text = String::from_utf8_lossy(&raw[..header_end]).to_ascii_lowercase();
            assert!(
                header_text.contains("content-type: application/json"),
                "含图请求必须发送 Content-Type: application/json：{header_text}"
            );
            let content_len: usize = header_text
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            while raw.len() < header_end + content_len {
                let mut tmp = [0u8; 1024];
                let n = sock.read(&mut tmp).await.unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&tmp[..n]);
            }
            let body_slice = &raw[header_end..header_end + content_len];
            let body_str = std::str::from_utf8(body_slice).unwrap();
            assert!(
                body_str.contains(SAMPLE_PNG_16_B64),
                "服务端收到的请求体应包含编码后的图片：{body_str}"
            );
            let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"收到图片\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
                sse.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });

        let (_dir, resolved) = make_resolved(&format!("http://{addr}")).await;
        let probe = Arc::new(BlockingProbeSource {
            bytes: png_bytes.clone(),
            thread_ids: Mutex::new(Vec::new()),
            blocking_verified: AtomicBool::new(false),
        });
        let events = resolved
            .stream_chat(
                image_chat_request(),
                probe.clone(),
                crate::StreamControl::default(),
            )
            .await
            .expect("含图请求应成功发送")
            .collect::<Vec<_>>()
            .await;
        assert!(
            probe.blocking_verified.load(Ordering::Acquire),
            "必须实际执行 blocking 线程校验"
        );
        let tids = probe.thread_ids.lock().unwrap().clone();
        assert_eq!(tids.len(), 2, "应依次记录 with_reader 与 with_image 线程");
        for tid in tids.iter() {
            assert_ne!(
                *tid, caller_tid,
                "含图请求编码必须在独立于当前异步任务的 blocking 线程上运行"
            );
        }
        assert!(
            matches!(
                events.last(),
                Some(crate::LlmEvent::Finished {
                    outcome: crate::ReplyOutcome::Complete { .. },
                    ..
                })
            ),
            "含图请求流应正常完成：{events:?}"
        );
        server.await.unwrap();
    }

    // 5. 取消标志在编码完成之后、发送之前生效时，不发起 HTTP 请求。
    {
        struct CancelAfterReadSource {
            bytes: Vec<u8>,
            cancel: Arc<AtomicBool>,
            read_count: AtomicUsize,
        }
        impl crate::ImageSource for CancelAfterReadSource {
            fn with_reader(
                &self,
                encode: &mut dyn FnMut(&dyn crate::ImageReader) -> Result<(), crate::LlmError>,
            ) -> Result<(), crate::LlmError> {
                encode(self)
            }
        }
        impl crate::ImageReader for CancelAfterReadSource {
            fn with_image(
                &self,
                _image: &lattice_conversation::ImageRef,
                consume: &mut dyn FnMut(&[u8]) -> Result<(), crate::LlmError>,
            ) -> Result<(), crate::LlmError> {
                let res = consume(&self.bytes);
                self.read_count.fetch_add(1, Ordering::SeqCst);
                self.cancel.store(true, Ordering::Release);
                res
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_dir, resolved) = make_resolved(&format!("http://{addr}")).await;
        let cancel = Arc::new(AtomicBool::new(false));
        let source = Arc::new(CancelAfterReadSource {
            bytes: png_bytes,
            cancel: cancel.clone(),
            read_count: AtomicUsize::new(0),
        });
        let events = resolved
            .stream_chat(
                image_chat_request(),
                source.clone(),
                crate::StreamControl {
                    cancel: Some(cancel),
                    deadline: None,
                },
            )
            .await
            .expect("编码完成后取消应通过流中断事件交付")
            .collect::<Vec<_>>()
            .await;
        assert_eq!(
            source.read_count.load(Ordering::SeqCst),
            1,
            "取消前应已完成图片读取与编码"
        );
        assert!(
            matches!(
                events.as_slice(),
                [
                    crate::LlmEvent::Started,
                    crate::LlmEvent::Finished {
                        outcome: crate::ReplyOutcome::Interrupted {
                            reason: crate::ReplyStop::Stream(crate::StreamStop::Cancelled),
                        },
                        ..
                    }
                ]
            ),
            "编码后取消应返回 Cancelled 终态：{events:?}"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "编码后取消不得向远端发起任何 HTTP 连接"
        );
    }
}
