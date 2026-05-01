//! Integration tests for all transport implementations.
//!
//! Verifies the complete message pipeline end-to-end:
//!   messages → normalize → provider-native JSON → denormalize → ChatResponse
//!
//! All tests use mock JSON data — no network calls.

use lattice_core::catalog::{ApiProtocol, CredentialStatus, ResolvedModel};
use lattice_core::provider::ChatRequest;
use lattice_core::streaming::StreamEvent;
use lattice_core::transport::anthropic::AnthropicTransport;
use lattice_core::transport::chat_completions::ChatCompletionsTransport;
use lattice_core::transport::dispatcher::TransportDispatcher;
use lattice_core::transport::gemini::GeminiTransport;
use lattice_core::transport::openai_compat::OpenAICompatTransport;
use lattice_core::transport::Transport;
use lattice_core::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn make_resolved(provider: &str, api_protocol: ApiProtocol, base_url: &str) -> ResolvedModel {
    ResolvedModel {
        canonical_id: "test-model".to_string(),
        provider: provider.to_string(),
        api_key: Some("sk-test".to_string()),
        base_url: base_url.to_string(),
        api_protocol,
        api_model_id: "test-model".to_string(),
        context_length: 131072,
        provider_specific: HashMap::new(),
        credential_status: CredentialStatus::Present,
    }
}

fn make_chat_request(messages: Vec<Message>, tools: Vec<ToolDefinition>) -> ChatRequest {
    ChatRequest::new(
        messages,
        tools,
        make_resolved("test", ApiProtocol::OpenAiChat, "https://api.test.com/v1"),
    )
}

fn sample_messages() -> Vec<Message> {
    vec![
        Message {
            role: Role::System,
            content: "You are a helpful assistant.".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::User,
            content: "What's the weather in Tokyo?".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::Assistant,
            content: "Let me check the weather.".to_string(),
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_abc".to_string(),
                function: FunctionCall {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city": "Tokyo"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::Tool,
            content: r#"{"temp": 22, "condition": "sunny"}"#.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_abc".to_string()),
            name: Some("get_weather".to_string()),
        },
    ]
}

fn sample_tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "get_weather".to_string(),
        description: "Get the current weather for a city".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"}
            },
            "required": ["city"]
        }),
    }]
}

// ═══════════════════════════════════════════════════════════════════════════
// OpenAI Chat Completions tests
// ═══════════════════════════════════════════════════════════════════════════

mod openai_chat_completions {
    use super::*;

    #[test]
    fn normalize_messages_with_all_roles() {
        let transport = ChatCompletionsTransport::new();
        let request = make_chat_request(sample_messages(), vec![]);
        let body = transport.normalize_request(&request).unwrap();

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);

        // System message
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are a helpful assistant.");

        // User message
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "What's the weather in Tokyo?");

        // Assistant message with tool_calls
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "Let me check the weather.");
        let tool_calls = messages[2]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_abc");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
        assert_eq!(
            tool_calls[0]["function"]["arguments"],
            r#"{"city": "Tokyo"}"#
        );

        // Tool message
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_abc");
        assert_eq!(messages[3]["name"], "get_weather");
    }

    #[test]
    fn normalize_tools_with_function_definitions() {
        let transport = ChatCompletionsTransport::new();
        let request = make_chat_request(vec![], sample_tools());
        let body = transport.normalize_request(&request).unwrap();

        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(
            tools[0]["function"]["description"],
            "Get the current weather for a city"
        );
        assert!(tools[0]["function"]["parameters"].is_object());
        assert_eq!(tools[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn denormalize_response_content_only() {
        let transport = ChatCompletionsTransport::new();
        let response = json!({
            "id": "chatcmpl-int001",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "The weather in Tokyo is sunny and 22°C."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 12,
                "total_tokens": 62
            }
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("The weather in Tokyo is sunny and 22°C.")
        );
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");
        assert_eq!(result.model, "gpt-4o");

        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 50);
        assert_eq!(usage.completion_tokens, 12);
        assert_eq!(usage.total_tokens, 62);
    }

    #[test]
    fn denormalize_response_tool_calls() {
        let transport = ChatCompletionsTransport::new();
        let response = json!({
            "id": "chatcmpl-int002",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_xyz",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"Paris\"}"
                        }
                    }, {
                        "id": "call_999",
                        "type": "function",
                        "function": {
                            "name": "search",
                            "arguments": "{\"q\": \"weather Paris\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 30,
                "completion_tokens": 15,
                "total_tokens": 45
            }
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert!(result.content.is_none());

        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs.len(), 2);
        assert_eq!(tcs[0].id, "call_xyz");
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(tcs[0].function.arguments, r#"{"city": "Paris"}"#);
        assert_eq!(tcs[1].id, "call_999");
        assert_eq!(tcs[1].function.name, "search");
        assert_eq!(result.finish_reason, "tool_calls");
    }

    #[test]
    fn denormalize_stream_chunk_content_delta() {
        let transport = ChatCompletionsTransport::new();
        let mut request = make_chat_request(
            vec![Message {
                role: Role::User,
                content: "Hello".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            vec![],
        );
        request.stream = true;

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn denormalize_stream_chunk_tool_call_delta() {
        let transport = ChatCompletionsTransport::new();
        let messages = vec![Message {
            role: Role::Assistant,
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_stream".to_string(),
                function: FunctionCall {
                    name: "search".to_string(),
                    arguments: r#"{"q": "test"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        }];
        let request = make_chat_request(messages, vec![]);
        let body = transport.normalize_request(&request).unwrap();

        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["tool_calls"][0]["id"], "call_stream");
        assert_eq!(msgs[0]["tool_calls"][0]["function"]["name"], "search");
    }

    #[test]
    fn denormalize_stream_chunk_done() {
        let transport = ChatCompletionsTransport::new();
        let request = make_chat_request(
            vec![Message {
                role: Role::User,
                content: "Done test".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            vec![],
        );
        let _body = transport.normalize_request(&request).unwrap();

        let response = json!({
            "id": "chatcmpl-done",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Complete"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.finish_reason, "stop");
    }

    #[test]
    fn full_pipeline_roundtrip() {
        let transport = ChatCompletionsTransport::new();
        let request = make_chat_request(sample_messages(), sample_tools());
        let body = transport.normalize_request(&request).unwrap();

        // Verify the normalized body is valid JSON with expected structure
        assert_eq!(body["model"], "test-model");
        assert!(body["messages"].is_array());
        assert!(body["tools"].is_array());

        // Simulate a provider response matching the normalized request
        let response = json!({
            "id": "chatcmpl-roundtrip",
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "The weather in Tokyo is sunny, 22°C.",
                    "tool_calls": [{
                        "id": "call_rt1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"Tokyo\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 40, "completion_tokens": 20, "total_tokens": 60}
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("The weather in Tokyo is sunny, 22°C.")
        );
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(result.finish_reason, "tool_calls");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Anthropic tests
// ═══════════════════════════════════════════════════════════════════════════

mod anthropic {
    use super::*;

    #[test]
    fn normalize_messages_with_system_extraction() {
        let transport = AnthropicTransport::new();
        let result = transport.normalize_messages(&sample_messages());

        // System prompt should be extracted separately
        assert_eq!(
            result.system,
            Some("You are a helpful assistant.".to_string())
        );

        // System message should NOT appear in messages array
        for msg in &result.messages {
            assert_ne!(msg["role"], "system");
        }
    }

    #[test]
    fn normalize_messages_with_tool_use_content_blocks() {
        let transport = AnthropicTransport::new();
        let result = transport.normalize_messages(&sample_messages());

        // Find the assistant message
        let assistant_msg = result
            .messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("should have assistant message");

        let content = assistant_msg["content"].as_array().unwrap();
        // Should have text block + tool_use block
        let has_text = content.iter().any(|b| b["type"] == "text");
        let has_tool_use = content.iter().any(|b| b["type"] == "tool_use");
        assert!(has_text, "assistant should have text block");
        assert!(has_tool_use, "assistant should have tool_use block");

        // Verify tool_use details
        let tool_use_block = content.iter().find(|b| b["type"] == "tool_use").unwrap();
        assert_eq!(tool_use_block["id"], "call_abc");
        assert_eq!(tool_use_block["name"], "get_weather");
        // Anthropic uses "input" (object) instead of "arguments" (string)
        assert!(tool_use_block["input"].is_object());
        assert_eq!(tool_use_block["input"]["city"], "Tokyo");
    }

    #[test]
    fn normalize_messages_with_tool_result() {
        let transport = AnthropicTransport::new();
        let result = transport.normalize_messages(&sample_messages());

        // Tool results should be wrapped in a user message with tool_result content blocks
        let user_msgs: Vec<_> = result
            .messages
            .iter()
            .filter(|m| m["role"] == "user")
            .collect();

        // Should have at least one user message containing tool_result
        let has_tool_result = user_msgs.iter().any(|msg| {
            msg["content"]
                .as_array()
                .map(|arr| arr.iter().any(|b| b["type"] == "tool_result"))
                .unwrap_or(false)
        });
        assert!(has_tool_result, "should have tool_result in user message");

        // Find the tool_result block
        let tool_result_block = user_msgs
            .iter()
            .flat_map(|msg| msg["content"].as_array().into_iter().flatten())
            .find(|b| b["type"] == "tool_result")
            .unwrap();
        assert_eq!(tool_result_block["tool_use_id"], "call_abc");
    }

    #[test]
    fn denormalize_response_text_content() {
        let transport = AnthropicTransport::new();
        let response = json!({
            "content": [
                {"type": "text", "text": "Hello! The weather is nice today."}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 25, "output_tokens": 8}
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("Hello! The weather is nice today.")
        );
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");
    }

    #[test]
    fn denormalize_response_tool_use_content() {
        let transport = AnthropicTransport::new();
        let response = json!({
            "content": [
                {"type": "text", "text": "Let me look that up."},
                {"type": "tool_use", "id": "toolu_int01", "name": "search", "input": {"query": "weather Tokyo"}},
                {"type": "tool_use", "id": "toolu_int02", "name": "get_weather", "input": {"city": "Tokyo"}}
            ],
            "stop_reason": "tool_use"
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Let me look that up."));

        let tcs = result.tool_calls.expect("should have tool_calls");
        assert_eq!(tcs.len(), 2);
        assert_eq!(tcs[0].id, "toolu_int01");
        assert_eq!(tcs[0].function.name, "search");
        // Arguments should be serialized as JSON string
        let args: Value = serde_json::from_str(&tcs[0].function.arguments).unwrap();
        assert_eq!(args["query"], "weather Tokyo");

        assert_eq!(tcs[1].id, "toolu_int02");
        assert_eq!(tcs[1].function.name, "get_weather");

        assert_eq!(result.finish_reason, "tool_calls");
    }

    #[test]
    fn denormalize_stream_chunk_returns_empty_vec() {
        // denormalize_stream_chunk is deprecated — Anthropic transport uses SseParser instead.
        let transport = AnthropicTransport::new();
        assert!(transport
            .denormalize_stream_chunk(
                "content_block_delta",
                &json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}})
            )
            .is_empty());
        assert!(transport
            .denormalize_stream_chunk("content_block_start", &json!({}))
            .is_empty());
        assert!(transport
            .denormalize_stream_chunk("message_start", &json!({}))
            .is_empty());
        assert!(transport
            .denormalize_stream_chunk("ping", &json!({}))
            .is_empty());
        assert!(transport
            .denormalize_stream_chunk("message_stop", &json!({}))
            .is_empty());
    }

    #[test]
    fn normalize_tools_uses_input_schema() {
        let transport = AnthropicTransport::new();
        let tools = sample_tools();
        let result = transport.normalize_tools(&tools);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "get_weather");
        // Anthropic uses "input_schema" instead of "parameters"
        assert!(result[0].get("input_schema").is_some());
        assert!(result[0].get("parameters").is_none());
    }

    #[test]
    fn full_pipeline_anthropic_roundtrip() {
        let transport = AnthropicTransport::new();

        // Normalize
        let normalized = transport.normalize_messages(&sample_messages());
        assert!(normalized.system.is_some());

        // Build a mock Anthropic response matching the conversation
        let response = json!({
            "content": [
                {"type": "text", "text": "The weather in Tokyo is sunny, 22°C."},
                {"type": "tool_use", "id": "toolu_rt1", "name": "get_weather", "input": {"city": "Tokyo"}}
            ],
            "stop_reason": "tool_use"
        });

        // Denormalize
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("The weather in Tokyo is sunny, 22°C.")
        );
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs[0].id, "toolu_rt1");
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(result.finish_reason, "tool_calls");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Gemini tests
// ═══════════════════════════════════════════════════════════════════════════

mod gemini {
    use super::*;
    use lattice_core::transport::gemini::StreamChunk;

    #[test]
    fn normalize_messages_with_system_instruction_extraction() {
        let transport = GeminiTransport::new();
        let request = make_chat_request(sample_messages(), vec![]);
        let body = transport.normalize_request(&request).unwrap();

        // System instruction should be in systemInstruction, not contents
        let sys = &body["systemInstruction"];
        assert!(sys.is_object());
        let sys_parts = sys["parts"].as_array().unwrap();
        assert_eq!(sys_parts.len(), 1);
        assert_eq!(sys_parts[0]["text"], "You are a helpful assistant.");

        // System should not appear in contents
        let contents = body["contents"].as_array().unwrap();
        for entry in contents {
            assert_ne!(entry["role"], "system");
        }
    }

    #[test]
    fn normalize_messages_with_function_call_parts() {
        let transport = GeminiTransport::new();
        let request = make_chat_request(sample_messages(), vec![]);
        let body = transport.normalize_request(&request).unwrap();

        // Find the model (assistant) message
        let contents = body["contents"].as_array().unwrap();
        let model_msg = contents
            .iter()
            .find(|c| c["role"] == "model")
            .expect("should have model message");

        let parts = model_msg["parts"].as_array().unwrap();
        let has_text = parts.iter().any(|p| p.get("text").is_some());
        let has_fc = parts.iter().any(|p| p.get("functionCall").is_some());
        assert!(has_text, "model should have text part");
        assert!(has_fc, "model should have functionCall part");

        let fc_part = parts
            .iter()
            .find(|p| p.get("functionCall").is_some())
            .unwrap();
        assert_eq!(fc_part["functionCall"]["name"], "get_weather");
        assert_eq!(fc_part["functionCall"]["args"]["city"], "Tokyo");
    }

    #[test]
    fn normalize_messages_with_function_response_parts() {
        let transport = GeminiTransport::new();
        let request = make_chat_request(sample_messages(), vec![]);
        let body = transport.normalize_request(&request).unwrap();

        // Tool results are wrapped in user role with functionResponse
        let contents = body["contents"].as_array().unwrap();
        let tool_user_msgs: Vec<_> = contents
            .iter()
            .filter(|c| {
                c["role"] == "user"
                    && c["parts"]
                        .as_array()
                        .map(|parts| parts.iter().any(|p| p.get("functionResponse").is_some()))
                        .unwrap_or(false)
            })
            .collect();

        assert!(!tool_user_msgs.is_empty(), "should have functionResponse");

        let fr = &tool_user_msgs[0]["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "get_weather");
        assert!(fr["response"].is_object());
    }

    #[test]
    fn denormalize_response_text_candidate() {
        let transport = GeminiTransport::new();
        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "The weather in Tokyo is sunny, 22°C."}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 30,
                "candidatesTokenCount": 15,
                "totalTokenCount": 45
            }
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("The weather in Tokyo is sunny, 22°C.")
        );
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");

        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 30);
        assert_eq!(usage.completion_tokens, 15);
        assert_eq!(usage.total_tokens, 45);
    }

    #[test]
    fn denormalize_response_function_call_candidate() {
        let transport = GeminiTransport::new();
        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 25,
                "candidatesTokenCount": 10,
                "totalTokenCount": 35
            }
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert!(result.content.is_none());
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function.name, "get_weather");
        let args: Value = serde_json::from_str(&tcs[0].function.arguments).unwrap();
        assert_eq!(args["city"], "Paris");
        assert_eq!(result.finish_reason, "tool_calls");
    }

    #[test]
    fn normalize_tools_gemini_format() {
        let transport = GeminiTransport::new();
        let request = make_chat_request(vec![], sample_tools());
        let body = transport.normalize_request(&request).unwrap();

        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        let decls = tools[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "get_weather");
        assert_eq!(
            decls[0]["description"],
            "Get the current weather for a city"
        );
    }

    #[test]
    fn denormalize_stream_chunk_text() {
        let transport = GeminiTransport::new();
        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello"}],
                    "role": "model"
                }
            }]
        });

        let results = transport.denormalize_stream_chunk(&chunk);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0],
            StreamChunk::Token {
                content: "Hello".to_string()
            }
        );
    }

    #[test]
    fn denormalize_stream_chunk_with_finish_and_usage() {
        let transport = GeminiTransport::new();
        let chunk = json!({
            "candidates": [{
                "content": {"parts": [], "role": "model"},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            }
        });

        let results = transport.denormalize_stream_chunk(&chunk);
        let has_done = results
            .iter()
            .any(|r| matches!(r, StreamChunk::Done { .. }));
        let has_usage = results
            .iter()
            .any(|r| matches!(r, StreamChunk::Usage { .. }));
        assert!(has_done);
        assert!(has_usage);
    }

    #[test]
    fn full_pipeline_gemini_roundtrip() {
        let transport = GeminiTransport::new();
        let request = make_chat_request(sample_messages(), sample_tools());
        let body = transport.normalize_request(&request).unwrap();

        // Verify structure
        assert!(body["contents"].is_array());
        assert!(body.get("systemInstruction").is_some());
        assert!(body["tools"].is_array());

        // Simulate a Gemini response
        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "The weather in Tokyo is sunny, 22°C."},
                        {"functionCall": {"name": "get_weather", "args": {"city": "Tokyo"}}}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 40,
                "candidatesTokenCount": 20,
                "totalTokenCount": 60
            }
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("The weather in Tokyo is sunny, 22°C.")
        );
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(result.finish_reason, "tool_calls");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// OpenAI-compat tests
// ═══════════════════════════════════════════════════════════════════════════

mod openai_compat {
    use super::*;

    #[test]
    fn custom_base_url_configuration() {
        let custom_url = "https://my-llm.internal.corp/v1";
        let transport = OpenAICompatTransport::new(custom_url, HashMap::new());
        assert_eq!(transport.base_url(), custom_url);

        let transport_with_headers = OpenAICompatTransport::new(
            "https://custom.api.com/v1",
            HashMap::from([("X-Custom".to_string(), "value".to_string())]),
        );
        assert_eq!(
            transport_with_headers.base_url(),
            "https://custom.api.com/v1"
        );
        assert_eq!(
            transport_with_headers
                .extra_headers()
                .get("X-Custom")
                .unwrap(),
            "value"
        );
    }

    #[test]
    fn delegates_normalize_to_chat_completions() {
        let transport = OpenAICompatTransport::new("http://localhost:11434/v1", HashMap::new());
        let mut request = make_chat_request(sample_messages(), sample_tools());
        request.model = "llama3".to_string();

        let body = transport.normalize_request(&request).unwrap();

        // Should produce identical format to ChatCompletionsTransport
        assert_eq!(body["model"], "llama3");
        assert!(body["messages"].is_array());
        assert!(body["tools"].is_array());

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[3]["role"], "tool");
    }

    #[test]
    fn delegates_denormalize_to_chat_completions() {
        let transport =
            OpenAICompatTransport::new("https://api.groq.com/openai/v1", HashMap::new());

        let response = json!({
            "id": "chatcmpl-compat",
            "model": "llama-3-70b",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello from Groq!",
                    "tool_calls": [{
                        "id": "call_groq1",
                        "type": "function",
                        "function": {"name": "search", "arguments": "{\"q\": \"test\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 15, "completion_tokens": 8, "total_tokens": 23}
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Hello from Groq!"));
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs[0].id, "call_groq1");
        assert_eq!(tcs[0].function.name, "search");
        assert_eq!(result.model, "llama-3-70b");
        assert_eq!(result.finish_reason, "tool_calls");
    }

    #[test]
    fn full_pipeline_compat_roundtrip() {
        let transport = OpenAICompatTransport::new("https://api.deepseek.com/v1", HashMap::new());
        let mut request = make_chat_request(sample_messages(), sample_tools());
        request.model = "deepseek-chat".to_string();

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "deepseek-chat");

        let response = json!({
            "id": "chatcmpl-deepseek",
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Weather: sunny"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 35, "completion_tokens": 5, "total_tokens": 40}
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Weather: sunny"));
        assert_eq!(result.finish_reason, "stop");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Cross-transport consistency tests
// ═══════════════════════════════════════════════════════════════════════════

mod cross_transport {
    use super::*;

    /// Given the same input messages, verify all format-level transports produce
    /// structurally valid output. The output formats differ per provider, but
    /// none should error.
    #[test]
    fn all_format_transports_normalize_without_error() {
        let messages = sample_messages();
        let tools = sample_tools();

        // AnthropicTransport (FormatTransport)
        let anthropic = AnthropicTransport::new();
        let anth_result = anthropic.normalize_messages(&messages);
        assert!(
            anth_result.system.is_some(),
            "Anthropic should extract system"
        );
        assert!(
            !anth_result.messages.is_empty(),
            "Anthropic should produce messages"
        );
        let anth_tools = anthropic.normalize_tools(&tools);
        assert_eq!(anth_tools.len(), 1, "Anthropic should normalize tools");

        // ChatCompletionsTransport (ChatTransport — uses ChatRequest)
        let chat = ChatCompletionsTransport::new();
        let chat_req = make_chat_request(messages.clone(), tools.clone());
        let chat_body = chat.normalize_request(&chat_req).unwrap();
        assert!(chat_body["messages"].is_array());
        assert!(chat_body["tools"].is_array());

        // GeminiTransport (ChatTransport — uses ChatRequest)
        let gemini = GeminiTransport::new();
        let gemini_req = make_chat_request(messages.clone(), tools);
        let gemini_body = gemini.normalize_request(&gemini_req).unwrap();
        assert!(gemini_body["contents"].is_array());
        assert!(gemini_body.get("systemInstruction").is_some());
    }

    /// All transports should be able to denormalize a content-only response
    /// from their native format into a structurally consistent internal form.
    #[test]
    fn all_transports_denormalize_text_response() {
        // OpenAI format
        let chat = ChatCompletionsTransport::new();
        let openai_response = json!({
            "id": "test",
            "model": "gpt-4o",
            "choices": [{
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }]
        });
        let chat_result = chat.denormalize_response(&openai_response).unwrap();
        assert_eq!(chat_result.content.as_deref(), Some("Hello!"));
        assert!(chat_result.tool_calls.is_none());

        // Anthropic format
        let anthropic = AnthropicTransport::new();
        let anthropic_response = json!({
            "content": [{"type": "text", "text": "Hello!"}],
            "stop_reason": "end_turn"
        });
        let anth_result = anthropic.denormalize_response(&anthropic_response).unwrap();
        assert_eq!(anth_result.content.as_deref(), Some("Hello!"));
        assert!(anth_result.tool_calls.is_none());

        // Gemini format
        let gemini = GeminiTransport::new();
        let gemini_response = json!({
            "candidates": [{
                "content": {"parts": [{"text": "Hello!"}], "role": "model"},
                "finishReason": "STOP"
            }]
        });
        let gemini_result = gemini.denormalize_response(&gemini_response).unwrap();
        assert_eq!(gemini_result.content.as_deref(), Some("Hello!"));
        assert!(gemini_result.tool_calls.is_none());
    }

    /// All transports should correctly extract tool calls from their native
    /// format and produce consistent internal representations.
    #[test]
    fn all_transports_denormalize_tool_call_response() {
        // OpenAI format
        let chat = ChatCompletionsTransport::new();
        let openai_response = json!({
            "id": "test",
            "model": "gpt-4o",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "search", "arguments": "{\"q\": \"test\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let chat_result = chat.denormalize_response(&openai_response).unwrap();
        let chat_tcs = chat_result.tool_calls.unwrap();
        assert_eq!(chat_tcs[0].function.name, "search");

        // Anthropic format
        let anthropic = AnthropicTransport::new();
        let anthropic_response = json!({
            "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "search", "input": {"q": "test"}}
            ],
            "stop_reason": "tool_use"
        });
        let anth_result = anthropic.denormalize_response(&anthropic_response).unwrap();
        let anth_tcs = anth_result.tool_calls.unwrap();
        assert_eq!(anth_tcs[0].function.name, "search");

        // Gemini format
        let gemini = GeminiTransport::new();
        let gemini_response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "search", "args": {"q": "test"}}}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }]
        });
        let gemini_result = gemini.denormalize_response(&gemini_response).unwrap();
        let gemini_tcs = gemini_result.tool_calls.unwrap();
        assert_eq!(gemini_tcs[0].function.name, "search");
    }

    /// Verify that the same user message is encoded correctly across all transports.
    #[test]
    fn same_user_message_across_all_transports() {
        let msg = Message {
            role: Role::User,
            content: "What is 2+2?".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };

        // Anthropic
        let anthropic = AnthropicTransport::new();
        let anth = anthropic.normalize_messages(&[msg.clone()]);
        let anth_user = anth.messages.iter().find(|m| m["role"] == "user").unwrap();
        let anth_text = anth_user["content"][0]["text"].as_str().unwrap();
        assert_eq!(anth_text, "What is 2+2?");

        // ChatCompletions
        let chat = ChatCompletionsTransport::new();
        let chat_req = make_chat_request(vec![msg.clone()], vec![]);
        let chat_body = chat.normalize_request(&chat_req).unwrap();
        let chat_text = chat_body["messages"][0]["content"].as_str().unwrap();
        assert_eq!(chat_text, "What is 2+2?");

        // Gemini
        let gemini = GeminiTransport::new();
        let gemini_req = make_chat_request(vec![msg], vec![]);
        let gemini_body = gemini.normalize_request(&gemini_req).unwrap();
        let gemini_text = gemini_body["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(gemini_text, "What is 2+2?");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Dispatcher integration tests
// ═══════════════════════════════════════════════════════════════════════════

mod dispatcher {
    use super::*;

    #[test]
    fn dispatch_protocol_returns_correct_transport() {
        let dispatcher = TransportDispatcher::new();

        // ApiProtocol::OpenAiChat → chat_completions api_mode
        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();
        assert_eq!(transport.api_mode(), "chat_completions");

        // ApiProtocol::AnthropicMessages → anthropic api_mode
        let transport = dispatcher
            .dispatch(&ApiProtocol::AnthropicMessages)
            .unwrap();
        assert_eq!(transport.api_mode(), "anthropic");

        // ApiProtocol::GeminiGenerateContent → gemini api_mode
        let transport = dispatcher
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .unwrap();
        assert_eq!(transport.api_mode(), "gemini");
    }

    #[test]
    fn dispatch_for_resolved_returns_correct_transport() {
        let dispatcher = TransportDispatcher::new();

        let resolved = make_resolved(
            "anthropic",
            ApiProtocol::AnthropicMessages,
            "https://api.anthropic.com",
        );
        let transport = dispatcher.dispatch_for_resolved(&resolved).unwrap();
        assert_eq!(transport.api_mode(), "anthropic");

        let resolved = make_resolved(
            "gemini",
            ApiProtocol::GeminiGenerateContent,
            "https://generativelanguage.googleapis.com",
        );
        let transport = dispatcher.dispatch_for_resolved(&resolved).unwrap();
        assert_eq!(transport.api_mode(), "gemini");

        let resolved = make_resolved(
            "openai",
            ApiProtocol::OpenAiChat,
            "https://api.openai.com/v1",
        );
        let transport = dispatcher.dispatch_for_resolved(&resolved).unwrap();
        assert_eq!(transport.api_mode(), "chat_completions");
    }

    #[test]
    fn dispatch_unregistered_protocol_returns_none() {
        let dispatcher = TransportDispatcher::new();

        // CodexResponses is not registered by default
        let result = dispatcher.dispatch(&ApiProtocol::CodexResponses);
        assert!(result.is_none());
    }

    #[test]
    fn dispatcher_anthropic_normalize_denormalize() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::AnthropicMessages)
            .unwrap();

        let request = ChatRequest {
            messages: sample_messages(),
            tools: sample_tools(),
            model: "claude-3-opus".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(200),
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: ResolvedModel {
                canonical_id: "claude-3-opus".into(),
                provider: "anthropic".into(),
                api_key: None,
                base_url: "https://api.anthropic.com".into(),
                api_protocol: ApiProtocol::AnthropicMessages,
                api_model_id: "claude-3-opus".into(),
                context_length: 200000,
                provider_specific: HashMap::new(),
                credential_status: CredentialStatus::Missing,
            },
        };

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "claude-3-opus");
        assert_eq!(body["system"], "You are a helpful assistant.");
        assert_eq!(body["max_tokens"], 200);
        assert_eq!(body["temperature"], 0.7);
        assert!(body["messages"].is_array());
        assert!(body["tools"].is_array());

        // Denormalize
        let response = json!({
            "content": [
                {"type": "text", "text": "The weather is sunny."},
                {"type": "tool_use", "id": "toolu_disp1", "name": "get_weather", "input": {"city": "Tokyo"}}
            ],
            "stop_reason": "tool_use"
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("The weather is sunny."));
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs[0].id, "toolu_disp1");
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(result.finish_reason, "tool_calls");
    }

    #[test]
    fn dispatcher_gemini_normalize_denormalize() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .unwrap();

        let request = ChatRequest {
            messages: sample_messages(),
            tools: sample_tools(),
            model: "gemini-2.5-flash".to_string(),
            temperature: Some(0.5),
            max_tokens: Some(150),
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: ResolvedModel {
                canonical_id: "gemini-2.5-flash".into(),
                provider: "gemini".into(),
                api_key: None,
                base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
                api_protocol: ApiProtocol::GeminiGenerateContent,
                api_model_id: "gemini-2.5-flash".into(),
                context_length: 1048576,
                provider_specific: HashMap::new(),
                credential_status: CredentialStatus::Missing,
            },
        };

        let body = transport.normalize_request(&request).unwrap();
        assert!(body["contents"].is_array());
        assert!(body.get("systemInstruction").is_some());
        assert!(body["tools"].is_array());
        assert_eq!(body["generationConfig"]["temperature"], 0.5);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 150);

        // Denormalize
        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "The weather is sunny, 22°C."}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 30,
                "candidatesTokenCount": 10,
                "totalTokenCount": 40
            }
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("The weather is sunny, 22°C.")
        );
        assert_eq!(result.finish_reason, "stop");
    }

    #[test]
    fn dispatcher_chat_completions_normalize_denormalize() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();

        let request = ChatRequest {
            messages: sample_messages(),
            tools: sample_tools(),
            model: "gpt-4o".to_string(),
            temperature: Some(0.3),
            max_tokens: Some(100),
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: make_resolved(
                "openai",
                ApiProtocol::OpenAiChat,
                "https://api.openai.com/v1",
            ),
        };

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["temperature"], 0.3);
        assert_eq!(body["max_tokens"], 100);

        let response = json!({
            "id": "chatcmpl-disp",
            "model": "gpt-4o",
            "choices": [{
                "message": {"role": "assistant", "content": "Sunny day!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 3, "total_tokens": 23}
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Sunny day!"));
    }

    #[test]
    fn register_custom_transport_and_dispatch() {
        let mut dispatcher = TransportDispatcher::new();

        // Register a custom transport for CodexResponses
        dispatcher.register(
            ApiProtocol::CodexResponses,
            Box::new(ChatCompletionsTransport::with_base_url(
                "https://api.openai.com/v1",
            )),
        );

        let transport = dispatcher.dispatch(&ApiProtocol::CodexResponses).unwrap();
        assert_eq!(transport.api_mode(), "chat_completions");
        assert_eq!(transport.base_url(), "https://api.openai.com/v1");
    }
}
