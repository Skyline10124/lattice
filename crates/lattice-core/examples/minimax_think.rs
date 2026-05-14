#[tokio::main]
async fn main() {
    let resolved = lattice_core::resolve("minimax-m2.7").expect("resolve");

    let msg = lattice_core::Message {
        role: lattice_core::Role::User,
        content: "What is 17 * 23? Think step by step.".into(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
    };

    match lattice_core::chat_complete(&resolved, &[msg], &[]).await {
        Ok(response) => {
            println!(
                "reasoning: {:?}",
                response
                    .reasoning_content
                    .as_ref()
                    .map(|r| &r[..r.len().min(300)])
            );
            println!("---");
            println!("content: {:?}", response.content);
        }
        Err(e) => println!("ERROR: {:?}", e),
    }
}
