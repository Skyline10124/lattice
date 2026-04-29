use artemis_core::{resolve, chat, Message, Role, StreamEvent, ToolDefinition};
use futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::fs;

#[tokio::main]
async fn main() {
    // 1. Resolve model
    let resolved = resolve("deepseek-v4-pro").expect("resolve");
    println!("Using: {} via {}", resolved.api_model_id, resolved.provider);

    // 2. Define tools
    let tools = vec![
        ToolDefinition {
            name: "read_file".into(),
            description: "Read the contents of a file at the given path".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path relative to project root"}
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "list_directory".into(),
            description: "List files in a directory".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory path"}
                },
                "required": ["path"]
            }),
        },
    ];

    // 3. Build initial messages
    let prompt = "Review artemis-core/src/router.rs for any bugs or issues. \
                  First use read_file to read artemis-core/src/router.rs, \
                  then provide a brief review of any issues you find. \
                  Keep it concise — focus on the top 3 issues.";

    let mut messages: Vec<Message> = vec![Message {
        role: Role::User,
        content: prompt.to_string(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }];

    // 4. Conversation loop: stream, detect tool calls, execute, repeat
    loop {
        let mut stream = chat(&resolved, &messages, &tools)
            .await
            .expect("chat failed");

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_builders: HashMap<String, ToolCallAccum> = HashMap::new();

        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::Token { content: c } => {
                    content.push_str(&c);
                    print!("{}", c);
                }
                StreamEvent::Reasoning { content: r } => {
                    reasoning.push_str(&r);
                }
                StreamEvent::ToolCallStart { id, name } => {
                    tool_builders.insert(
                        id,
                        ToolCallAccum {
                            name,
                            arguments: String::new(),
                        },
                    );
                }
                StreamEvent::ToolCallDelta {
                    id,
                    arguments_delta,
                } => {
                    if let Some(tc) = tool_builders.get_mut(&id) {
                        tc.arguments.push_str(&arguments_delta);
                    }
                }
                StreamEvent::ToolCallEnd { .. } => {}
                StreamEvent::Done { .. } => {}
                StreamEvent::Error { message } => {
                    if message.contains("Stream ended") {
                        break;
                    }
                    eprintln!("\nStream error: {}", message);
                }
            }
        }

        // Build assistant message
        let tool_calls: Option<Vec<_>> = if tool_builders.is_empty() {
            None
        } else {
            Some(
                tool_builders
                    .into_iter()
                    .map(|(id, tc)| artemis_core::ToolCall {
                        id,
                        function: artemis_core::FunctionCall {
                            name: tc.name,
                            arguments: tc.arguments,
                        },
                    })
                    .collect(),
            )
        };

        messages.push(Message {
            role: Role::Assistant,
            content: content.clone(),
            reasoning_content: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            tool_calls: tool_calls.clone(),
            tool_call_id: None,
            name: None,
        });

        // Check if we need to execute tools
        match tool_calls {
            Some(calls) if !calls.is_empty() => {
                println!("\n--- Executing {} tool(s) ---", calls.len());
                for call in &calls {
                    let args_display = &call.function.arguments
                        [..call.function.arguments.len().min(100)];
                    println!(
                        "  Calling {} with args: {}",
                        call.function.name, args_display
                    );
                    let result = match call.function.name.as_str() {
                        "read_file" => {
                            let args: serde_json::Value =
                                serde_json::from_str(&call.function.arguments)
                                    .unwrap_or_default();
                            let path = args["path"].as_str().unwrap_or("");
                            match fs::read_to_string(
                                &format!("/home/astrin/artemis/{}", path),
                            ) {
                                Ok(content) => {
                                    if content.len() > 8000 {
                                        format!(
                                            "{}...(truncated, {} bytes total)",
                                            &content[..8000],
                                            content.len()
                                        )
                                    } else {
                                        content
                                    }
                                }
                                Err(e) => format!("Error reading file: {}", e),
                            }
                        }
                        "list_directory" => {
                            let args: serde_json::Value =
                                serde_json::from_str(&call.function.arguments)
                                    .unwrap_or_default();
                            let path = args["path"].as_str().unwrap_or(".");
                            match fs::read_dir(&format!(
                                "/home/astrin/artemis/{}",
                                path
                            )) {
                                Ok(entries) => entries
                                    .filter_map(|e| e.ok())
                                    .map(|e| {
                                        e.file_name().to_string_lossy().to_string()
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                Err(e) => format!("Error: {}", e),
                            }
                        }
                        _ => format!("Unknown tool: {}", call.function.name),
                    };
                    messages.push(Message {
                        role: Role::Tool,
                        content: result,
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: Some(call.id.clone()),
                        name: Some(call.function.name.clone()),
                    });
                }
                // Continue the loop to get the model's response
                continue;
            }
            _ => {
                // No tool calls — conversation is complete
                break;
            }
        }
    }

    println!();
}

/// Internal helper for accumulating tool call data during streaming.
struct ToolCallAccum {
    name: String,
    arguments: String,
}
