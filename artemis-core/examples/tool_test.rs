use artemis_core::{chat_complete, resolve, Message, Role, ToolDefinition};
use serde_json::json;

#[tokio::main]
async fn main() {
    let resolved = resolve("deepseek-v4-pro").expect("resolve");

    let tools = vec![ToolDefinition {
        name: "get_weather".into(),
        description: "Get current weather for a city".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"}
            },
            "required": ["city"]
        }),
    }];

    let messages = vec![Message {
        role: Role::User,
        content: "What's the weather in Beijing? Use the get_weather tool.".into(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }];

    let response = chat_complete(&resolved, &messages, &tools)
        .await
        .expect("chat");
    println!("content: {:?}", response.content);
    println!("tool_calls: {:?}", response.tool_calls);
    println!("finish: {}", response.finish_reason);
}
