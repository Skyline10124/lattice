#[tokio::main]
async fn main() {
    let resolved = lattice_core::resolve("minimax-m2.7").expect("resolve failed");
    println!(
        "provider={}, model={}, base_url={}, protocol={:?}",
        resolved.provider, resolved.api_model_id, resolved.base_url, resolved.api_protocol
    );

    let msg = lattice_core::Message {
        role: lattice_core::Role::User,
        content: "Say hello in one sentence.".into(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
    };

    match lattice_core::chat_complete(&resolved, &[msg], &[]).await {
        Ok(response) => {
            println!("content: {:?}", response.content);
            println!(
                "reasoning: {:?}",
                response
                    .reasoning_content
                    .as_ref()
                    .map(|r| &r[..r.len().min(150)])
            );
        }
        Err(e) => println!("ERROR: {:?}", e),
    }
}
