#[tokio::main]
async fn main() {
    let resolved = lattice::resolve("deepseek-v4-pro").expect("resolve failed");
    println!(
        "provider={}, model={}, key={}",
        resolved.provider,
        resolved.api_model_id,
        resolved.api_key.is_some()
    );

    let msg = lattice::Message {
        role: lattice::Role::User,
        content: "Say hello in one sentence.".into(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    };

    let response = lattice::chat_complete(&resolved, &[msg], &[])
        .await
        .expect("chat failed");

    println!("content: {:?}", response.content);
    println!("finish: {}", response.finish_reason);
}
