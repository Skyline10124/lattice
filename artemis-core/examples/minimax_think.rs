#[tokio::main]
async fn main() {
    let resolved = artemis_core::resolve("minimax-m2.7").expect("resolve");

    let msg = artemis_core::Message {
        role: artemis_core::Role::User,
        content: "What is 17 * 23? Think step by step.".into(),
        tool_calls: None, tool_call_id: None, name: None, reasoning_content: None,
    };

    match artemis_core::chat_complete(&resolved, &[msg], &[]).await {
        Ok(response) => {
            println!("reasoning: {:?}", response.reasoning_content.as_ref().map(|r| &r[..r.len().min(300)]));
            println!("---");
            println!("content: {:?}", response.content);
        }
        Err(e) => println!("ERROR: {:?}", e),
    }
}
