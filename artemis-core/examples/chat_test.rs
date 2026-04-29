use futures::StreamExt;

#[tokio::main]
async fn main() {
    let resolved = artemis_core::resolve("deepseek-v4-pro").expect("resolve failed");
    println!("provider={}, model={}, key={}",
        resolved.provider, resolved.api_model_id, resolved.api_key.is_some());

    let msg = artemis_core::Message {
        role: artemis_core::Role::User,
        content: "Say hello in one sentence.".into(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    };

    let response = artemis_core::chat_complete(&resolved, &[msg], &[])
        .await
        .expect("chat failed");

    println!("content: {:?}", response.content);
    println!("finish: {}", response.finish_reason);
}
