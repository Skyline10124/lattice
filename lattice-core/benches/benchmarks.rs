//! Criterion benchmarks for lattice-core.
//!
//! 1. JSON throughput — serde_json serialize/deserialize of core types
//! 2. Streaming parse — SSE chunk parsing (OpenAI + Anthropic formats)
//! 3. Concurrent requests — ModelRouter::resolve() under concurrency
//! 4. Model resolution — single resolve() latency for various inputs

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::collections::HashMap;

use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry, ResolvedModel};
use lattice_core::router::ModelRouter;
use lattice_core::streaming::TokenUsage;
use lattice_core::streaming::{AnthropicSseParser, OpenAiSseParser, SseParser, StreamEvent};
use lattice_core::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

// ────────────────────────────────────────────────────────────────────────────
// Helper: realistic test data generators
// ────────────────────────────────────────────────────────────────────────────

fn make_model_catalog_entry(id: &str, provider_count: usize) -> ModelCatalogEntry {
    let providers: Vec<CatalogProviderEntry> = (0..provider_count)
        .map(|i| CatalogProviderEntry {
            provider_id: format!("provider_{}", i),
            api_model_id: format!("{}-v{}", id, i),
            priority: i as u32 + 1,
            weight: 1,
            credential_keys: HashMap::from([(
                "api_key".to_string(),
                format!("PROVIDER_{}_API_KEY", i),
            )]),
            base_url: Some(format!("https://api.provider{}.example.com/v1", i)),
            api_protocol: if i % 2 == 0 {
                ApiProtocol::OpenAiChat
            } else {
                ApiProtocol::AnthropicMessages
            },
            provider_specific: HashMap::new(),
        })
        .collect();

    ModelCatalogEntry {
        canonical_id: id.to_string(),
        display_name: format!("Model {}", id),
        description: format!("A benchmark test model with id={}", id),
        context_length: 128_000,
        capabilities: vec![
            "chat".to_string(),
            "tool_call".to_string(),
            "streaming".to_string(),
        ],
        providers,
        aliases: vec![id.split('-').next().unwrap_or("m").to_string()],
    }
}

fn make_resolved_model(id: &str) -> ResolvedModel {
    ResolvedModel {
        canonical_id: id.to_string(),
        provider: "openai".to_string(),
        api_key: Some("sk-benchmark-test-key-1234567890abcdef".to_string()),
        base_url: "https://api.openai.com/v1".to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: id.to_string(),
        context_length: 128_000,
        provider_specific: HashMap::new(),
    }
}

fn make_conversation_messages(n: usize) -> Vec<Message> {
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                Message {
                    role: Role::User,
                    content: format!(
                        "Tell me about topic {} in detail. I'm interested in the history, \
                         current state, and future prospects. Please provide examples and \
                         references where possible.",
                        i
                    ),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                }
            } else {
                Message {
                    role: Role::Assistant,
                    content: format!(
                        "Topic {} has a rich history dating back to the early developments \
                         in the field. Currently, it's one of the most active areas of \
                         research with numerous breakthroughs happening each year.",
                        i
                    ),
                    reasoning_content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: format!("call_{}", i),
                        function: FunctionCall {
                            name: "search_topic".to_string(),
                            arguments: format!(r#"{{"query": "topic {}", "depth": "full"}}"#, i),
                        },
                    }]),
                    tool_call_id: None,
                    name: None,
                }
            }
        })
        .collect()
}

// ────────────────────────────────────────────────────────────────────────────
// SSE chunk fixtures
// ────────────────────────────────────────────────────────────────────────────

fn openai_content_chunk(text: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-bench","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{{"index":0,"delta":{{"content":"{text}"}},"finish_reason":null}}]}}"#
    )
}

fn openai_tool_call_start_chunk() -> String {
    r#"{"id":"chatcmpl-bench","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_bench_tc1","type":"function","function":{"name":"execute_query","arguments":""}}]},"finish_reason":null}]}"#.to_string()
}

fn openai_finish_chunk() -> String {
    r#"{"id":"chatcmpl-bench","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}"#.to_string()
}

fn openai_stream_chunks(n_tokens: usize) -> Vec<(String, String)> {
    let mut chunks = Vec::new();
    for i in 0..n_tokens {
        let text = format!("word{} ", i);
        chunks.push(("message".to_string(), openai_content_chunk(&text)));
    }
    chunks.push(("message".to_string(), openai_finish_chunk()));
    chunks.push(("message".to_string(), "[DONE]".to_string()));
    chunks
}

fn anthropic_text_delta_chunk(text: &str) -> (String, String) {
    (
        "content_block_delta".to_string(),
        format!(
            r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{text}"}}}}"#
        ),
    )
}

fn anthropic_message_start_chunk() -> (String, String) {
    (
        "message_start".to_string(),
        r#"{"type":"message_start","message":{"id":"msg_bench","content":[],"model":"claude-sonnet-4-6","role":"assistant","stop_reason":null,"usage":{"input_tokens":100,"output_tokens":1}}}"#.to_string(),
    )
}

fn anthropic_message_delta_chunk() -> (String, String) {
    (
        "message_delta".to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":50}}"#.to_string(),
    )
}

fn anthropic_tool_use_start_chunk() -> (String, String) {
    (
        "content_block_start".to_string(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_bench_1","name":"search_database","input":{}}}"#.to_string(),
    )
}

fn anthropic_input_json_delta_chunk(partial: &str) -> (String, String) {
    let escaped = partial.replace('"', "\\\"");
    (
        "content_block_delta".to_string(),
        format!(
            r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":"{escaped}"}}}}"#
        ),
    )
}

fn anthropic_stream_chunks(n_tokens: usize) -> Vec<(String, String)> {
    let mut chunks = Vec::new();
    chunks.push(anthropic_message_start_chunk());
    for i in 0..n_tokens {
        chunks.push(anthropic_text_delta_chunk(&format!("word{} ", i)));
    }
    chunks.push(anthropic_message_delta_chunk());
    chunks
}

fn anthropic_tool_call_stream() -> Vec<(String, String)> {
    let mut chunks = Vec::new();
    chunks.push(anthropic_message_start_chunk());
    chunks.push(anthropic_tool_use_start_chunk());
    chunks.push(anthropic_input_json_delta_chunk(r#"search_param"#));
    chunks.push(anthropic_input_json_delta_chunk(r#"_value"#));
    chunks.push((
        "content_block_stop".to_string(),
        r#"{"type":"content_block_stop","index":0}"#.to_string(),
    ));
    chunks.push(anthropic_message_delta_chunk());
    chunks
}

fn build_raw_sse_string(n: usize) -> String {
    let mut buf = String::new();
    for i in 0..n {
        let chunk = openai_content_chunk(&format!("token{} ", i));
        buf.push_str("data: ");
        buf.push_str(&chunk);
        buf.push_str("\n\n");
    }
    buf.push_str("data: [DONE]\n\n");
    buf
}

// ════════════════════════════════════════════════════════════════════════════
// CATEGORY 1: JSON Throughput
// ════════════════════════════════════════════════════════════════════════════

fn bench_json_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("json_throughput");

    // ── ModelCatalogEntry ──
    let entry_small = make_model_catalog_entry("bench-small", 2);
    let entry_large = make_model_catalog_entry("bench-large", 8);
    let json_small = serde_json::to_string(&entry_small).unwrap();
    let json_large = serde_json::to_string(&entry_large).unwrap();

    group.throughput(Throughput::Bytes(json_small.len() as u64));
    group.bench_function("serialize_model_catalog_entry_small", |b| {
        b.iter(|| serde_json::to_string(black_box(&entry_small)))
    });

    group.throughput(Throughput::Bytes(json_large.len() as u64));
    group.bench_function("serialize_model_catalog_entry_large", |b| {
        b.iter(|| serde_json::to_string(black_box(&entry_large)))
    });

    group.throughput(Throughput::Bytes(json_small.len() as u64));
    group.bench_function("deserialize_model_catalog_entry_small", |b| {
        b.iter(|| serde_json::from_str::<ModelCatalogEntry>(black_box(&json_small)))
    });

    group.throughput(Throughput::Bytes(json_large.len() as u64));
    group.bench_function("deserialize_model_catalog_entry_large", |b| {
        b.iter(|| serde_json::from_str::<ModelCatalogEntry>(black_box(&json_large)))
    });

    // ── ResolvedModel ──
    let resolved = make_resolved_model("bench-resolved");
    let resolved_json = serde_json::to_string(&resolved).unwrap();

    group.throughput(Throughput::Bytes(resolved_json.len() as u64));
    group.bench_function("serialize_resolved_model", |b| {
        b.iter(|| serde_json::to_string(black_box(&resolved)))
    });

    group.throughput(Throughput::Bytes(resolved_json.len() as u64));
    group.bench_function("deserialize_resolved_model", |b| {
        b.iter(|| serde_json::from_str::<ResolvedModel>(black_box(&resolved_json)))
    });

    // ── Messages ──
    let msgs_5 = make_conversation_messages(5);
    let msgs_20 = make_conversation_messages(20);
    let msgs5_json = serde_json::to_string(&msgs_5).unwrap();
    let msgs20_json = serde_json::to_string(&msgs_20).unwrap();

    group.throughput(Throughput::Bytes(msgs5_json.len() as u64));
    group.bench_function("serialize_messages_5", |b| {
        b.iter(|| serde_json::to_string(black_box(&msgs_5)))
    });

    group.throughput(Throughput::Bytes(msgs20_json.len() as u64));
    group.bench_function("serialize_messages_20", |b| {
        b.iter(|| serde_json::to_string(black_box(&msgs_20)))
    });

    group.throughput(Throughput::Bytes(msgs5_json.len() as u64));
    group.bench_function("deserialize_messages_5", |b| {
        b.iter(|| serde_json::from_str::<Vec<Message>>(black_box(&msgs5_json)))
    });

    // ── ToolDefinition ──
    let tool_def = ToolDefinition {
        name: "search_database".to_string(),
        description: "Search the database for information with complex filtering options"
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query text"},
                "limit": {"type": "integer", "description": "Max results"},
                "offset": {"type": "integer", "description": "Pagination offset"},
                "sort": {"type": "string", "enum": ["asc", "desc"]},
                "filters": {
                    "type": "object",
                    "properties": {
                        "category": {"type": "string"},
                        "date_range": {"type": "array", "items": {"type": "string"}}
                    }
                }
            },
            "required": ["query"]
        }),
    };
    let tool_json = serde_json::to_string(&tool_def).unwrap();

    group.throughput(Throughput::Bytes(tool_json.len() as u64));
    group.bench_function("serialize_tool_definition", |b| {
        b.iter(|| serde_json::to_string(black_box(&tool_def)))
    });

    group.throughput(Throughput::Bytes(tool_json.len() as u64));
    group.bench_function("deserialize_tool_definition", |b| {
        b.iter(|| serde_json::from_str::<ToolDefinition>(black_box(&tool_json)))
    });

    // ── StreamEvent variants ──
    let token_event = StreamEvent::Token {
        content: "Hello world from benchmark".to_string(),
    };
    let done_event = StreamEvent::Done {
        finish_reason: "stop".to_string(),
        usage: Some(TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        }),
    };
    let tool_call_start = StreamEvent::ToolCallStart {
        id: "call_bench_tc1".to_string(),
        name: "execute_query".to_string(),
    };

    let token_json = serde_json::to_string(&token_event).unwrap();
    let done_json = serde_json::to_string(&done_event).unwrap();
    let tc_start_json = serde_json::to_string(&tool_call_start).unwrap();

    group.throughput(Throughput::Bytes(token_json.len() as u64));
    group.bench_function("serialize_stream_event_token", |b| {
        b.iter(|| serde_json::to_string(black_box(&token_event)))
    });

    group.throughput(Throughput::Bytes(done_json.len() as u64));
    group.bench_function("serialize_stream_event_done", |b| {
        b.iter(|| serde_json::to_string(black_box(&done_event)))
    });

    group.throughput(Throughput::Bytes(tc_start_json.len() as u64));
    group.bench_function("deserialize_stream_event_tool_call_start", |b| {
        b.iter(|| serde_json::from_str::<StreamEvent>(black_box(&tc_start_json)))
    });

    // ── Full catalog data.json parse ──
    let catalog_json = include_str!("../src/catalog/data.json");
    group.throughput(Throughput::Bytes(catalog_json.len() as u64));
    group.bench_function("deserialize_full_catalog", |b| {
        b.iter(|| {
            let data: lattice_core::catalog::CatalogData =
                serde_json::from_str(black_box(catalog_json)).unwrap();
            black_box(data);
        })
    });

    group.finish();
}

// ════════════════════════════════════════════════════════════════════════════
// CATEGORY 2: Streaming Parse
// ════════════════════════════════════════════════════════════════════════════

fn bench_streaming_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming_parse");

    // ── OpenAI: single content chunk ──
    let single_chunk = openai_content_chunk("benchmark token content");
    group.bench_function("openai_single_content_chunk", |b| {
        let mut parser = OpenAiSseParser::new();
        b.iter(|| {
            parser
                .parse_chunk(black_box("message"), black_box(&single_chunk))
                .unwrap()
        })
    });

    // ── OpenAI: tool call sequence ──
    let tc_start = openai_tool_call_start_chunk();
    let tc_delta1 = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"arg_a1"}}]},"finish_reason":null}]}"#;
    let tc_delta2 = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"arg_b2"}}]},"finish_reason":null}]}"#;
    let tc_finish = openai_finish_chunk();

    group.bench_function("openai_tool_call_sequence", |b| {
        b.iter(|| {
            let mut parser = OpenAiSseParser::new();
            parser.parse_chunk("message", &tc_start).unwrap();
            parser.parse_chunk("message", &tc_delta1).unwrap();
            parser.parse_chunk("message", &tc_delta2).unwrap();
            parser.parse_chunk("message", &tc_finish).unwrap();
            black_box(parser);
        })
    });

    // ── OpenAI: 50-token stream ──
    let chunks_50 = openai_stream_chunks(50);
    group.bench_function("openai_50_token_stream", |b| {
        b.iter(|| {
            let mut parser = OpenAiSseParser::new();
            let mut count = 0;
            for (ev, data) in &chunks_50 {
                count += parser.parse_chunk(ev, data).unwrap().len();
            }
            black_box(count)
        })
    });

    // ── OpenAI: 200-token stream ──
    let chunks_200 = openai_stream_chunks(200);
    group.bench_function("openai_200_token_stream", |b| {
        b.iter(|| {
            let mut parser = OpenAiSseParser::new();
            let mut count = 0;
            for (ev, data) in &chunks_200 {
                count += parser.parse_chunk(ev, data).unwrap().len();
            }
            black_box(count)
        })
    });

    // ── Anthropic: single text delta ──
    let (_, anth_text) = anthropic_text_delta_chunk("Anthropic benchmark text delta");
    group.bench_function("anthropic_single_text_delta", |b| {
        let mut parser = AnthropicSseParser::new();
        b.iter(|| {
            parser
                .parse_chunk(black_box("content_block_delta"), black_box(&anth_text))
                .unwrap()
        })
    });

    // ── Anthropic: tool call sequence ──
    let anth_tc = anthropic_tool_call_stream();
    group.bench_function("anthropic_tool_call_sequence", |b| {
        b.iter(|| {
            let mut parser = AnthropicSseParser::new();
            let mut count = 0;
            for (ev, data) in &anth_tc {
                count += parser.parse_chunk(ev, data).unwrap().len();
            }
            black_box(count)
        })
    });

    // ── Anthropic: 50-token stream ──
    let anth_chunks_50 = anthropic_stream_chunks(50);
    group.bench_function("anthropic_50_token_stream", |b| {
        b.iter(|| {
            let mut parser = AnthropicSseParser::new();
            let mut count = 0;
            for (ev, data) in &anth_chunks_50 {
                count += parser.parse_chunk(ev, data).unwrap().len();
            }
            black_box(count)
        })
    });

    // ── Anthropic: 200-token stream ──
    let anth_chunks_200 = anthropic_stream_chunks(200);
    group.bench_function("anthropic_200_token_stream", |b| {
        b.iter(|| {
            let mut parser = AnthropicSseParser::new();
            let mut count = 0;
            for (ev, data) in &anth_chunks_200 {
                count += parser.parse_chunk(ev, data).unwrap().len();
            }
            black_box(count)
        })
    });

    // ── Raw SSE parsing ──
    let raw_sse_input = build_raw_sse_string(20);
    group.throughput(Throughput::Bytes(raw_sse_input.len() as u64));
    group.bench_function("parse_raw_sse_20_events", |b| {
        b.iter(|| lattice_core::streaming::parse_raw_sse(black_box(&raw_sse_input)))
    });

    group.finish();
}

// ════════════════════════════════════════════════════════════════════════════
// CATEGORY 3: Concurrent Requests
// ════════════════════════════════════════════════════════════════════════════

fn bench_concurrent_requests(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_requests");

    let router = ModelRouter::new();

    for concurrency in [1, 4, 16, 64].iter() {
        group.bench_function(BenchmarkId::new("resolve_concurrent", concurrency), |b| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            b.iter(|| {
                let n = *concurrency;
                rt.block_on(async {
                    let mut handles = Vec::with_capacity(n);
                    for _ in 0..n {
                        handles.push(tokio::spawn(async {}));
                    }
                    let mut results = Vec::with_capacity(n);
                    for i in 0..n {
                        let model = if i % 3 == 0 {
                            "sonnet"
                        } else if i % 3 == 1 {
                            "gpt-4o"
                        } else {
                            "deepseek"
                        };
                        results.push(router.resolve(black_box(model), None));
                    }
                    for h in handles {
                        h.await.unwrap();
                    }
                    results
                })
            })
        });
    }

    // ── Batch resolve throughput ──
    let all_models = router.list_models();
    group.bench_function("resolve_all_catalog_models", |b| {
        b.iter(|| {
            let results: Vec<_> = all_models
                .iter()
                .map(|m| router.resolve(black_box(m), None))
                .collect();
            black_box(results)
        })
    });

    group.finish();
}

// ════════════════════════════════════════════════════════════════════════════
// CATEGORY 4: Model Resolution
// ════════════════════════════════════════════════════════════════════════════

fn bench_model_resolution(c: &mut Criterion) {
    let mut group = c.benchmark_group("model_resolution");

    let router = ModelRouter::new();

    // ── Alias resolution ──
    group.bench_function("resolve_alias_sonnet", |b| {
        b.iter(|| router.resolve(black_box("sonnet"), None))
    });

    group.bench_function("resolve_alias_gpt5", |b| {
        b.iter(|| router.resolve(black_box("gpt5"), None))
    });

    // ── Direct canonical ID ──
    group.bench_function("resolve_canonical_gpt_4o", |b| {
        b.iter(|| router.resolve(black_box("gpt-4o"), None))
    });

    group.bench_function("resolve_canonical_claude_sonnet_4_6", |b| {
        b.iter(|| router.resolve(black_box("claude-sonnet-4-6"), None))
    });

    // ── Normalized input (dots, prefixes) ──
    group.bench_function("resolve_normalized_dot", |b| {
        b.iter(|| router.resolve(black_box("claude-sonnet-4.6"), None))
    });

    group.bench_function("resolve_normalized_prefix", |b| {
        b.iter(|| router.resolve(black_box("anthropic/claude-sonnet-4.6"), None))
    });

    // ── Unknown model (permissive fallback path) ──
    group.bench_function("resolve_unknown_model_permissive", |b| {
        b.iter(|| router.resolve(black_box("openai/gpt-benchmark-fake"), None))
    });

    // ── Failed resolution (ModelNotFound) ──
    group.bench_function("resolve_nonexistent_error", |b| {
        b.iter(|| router.resolve(black_box("nonexistent-model-xyz-999"), None))
    });

    // ── normalize_model_id ──
    group.bench_function("normalize_model_id_simple", |b| {
        b.iter(|| lattice_core::router::normalize_model_id(black_box("gpt-4o")))
    });

    group.bench_function("normalize_model_id_complex", |b| {
        b.iter(|| {
            lattice_core::router::normalize_model_id(black_box(
                "us.anthropic.claude-sonnet-4-6-v1:0",
            ))
        })
    });

    // ── Token estimation ──
    group.bench_function("estimate_text_1kb", |b| {
        let text = "A".repeat(1000);
        b.iter(|| lattice_core::tokens::TokenEstimator::estimate_text(black_box(&text)))
    });

    group.bench_function("estimate_text_10kb", |b| {
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(222);
        b.iter(|| lattice_core::tokens::TokenEstimator::estimate_text(black_box(&text)))
    });

    group.finish();
}

criterion_group!(json_throughput, bench_json_throughput,);
criterion_group!(streaming_parse, bench_streaming_parse,);
criterion_group!(concurrent_requests, bench_concurrent_requests,);
criterion_group!(model_resolution, bench_model_resolution,);

criterion_main!(
    json_throughput,
    streaming_parse,
    concurrent_requests,
    model_resolution,
);
