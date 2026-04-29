# 第一个调用

## Rust

```rust
use artemis_core;

#[tokio::main]
async fn main() {
    // 1. 解析模型
    let resolved = artemis_core::resolve("deepseek-v4-pro")?;

    // 2. 构造消息
    let msg = artemis_core::Message {
        role: artemis_core::Role::User,
        content: "用一句话打招呼".into(),
        tool_calls: None, tool_call_id: None,
        name: None, reasoning_content: None,
    };

    // 3. 非流式调用
    let response = artemis_core::chat_complete(&resolved, &[msg], &[]).await?;
    println!("{}", response.content.unwrap_or_default());
}
```

## 带工具调用

```rust
let tools = vec![artemis_core::ToolDefinition {
    name: "get_weather".into(),
    description: "获取城市天气".into(),
    parameters: serde_json::json!({
        "type": "object",
        "properties": {
            "city": {"type": "string"}
        },
        "required": ["city"]
    }),
}];

let response = artemis_core::chat_complete(&resolved, &messages, &tools).await?;
if let Some(calls) = response.tool_calls {
    for call in calls {
        println!("调用 {}: {}", call.function.name, call.function.arguments);
        // 执行工具，结果回传给模型
    }
}
```

## 思考模式

DeepSeek v4-pro 和 MiniMax M2.7 自动启用：

```rust
let response = artemis_core::chat_complete(&resolved, &[msg], &[]).await?;
println!("思考: {:?}", response.reasoning_content);
println!("回答: {:?}", response.content);
```

模型名匹配策略：`deepseek-v4-pro` 自动启用 thinking + effort=high，其他模型不启用。
