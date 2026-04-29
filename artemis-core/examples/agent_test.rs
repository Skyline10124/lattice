use artemis_core::{resolve, chat, Role, Message};
use futures::StreamExt;

#[tokio::main]
async fn main() {
    let resolved = resolve("deepseek-v4-pro").expect("resolve");
    println!("provider={}, model={}", resolved.provider, resolved.api_model_id);
    
    let msg = Message {
        role: Role::User, content: "Say hi".into(),
        reasoning_content: None, tool_calls: None, tool_call_id: None, name: None,
    };
    
    match chat(&resolved, &[msg.clone()], &[]).await {
        Ok(mut stream) => {
            while let Some(event) = stream.next().await {
                match event {
                    artemis_core::StreamEvent::Token { content } => print!("{}", content),
                    artemis_core::StreamEvent::Done { .. } => println!(" [Done]"),
                    artemis_core::StreamEvent::Error { message } => eprintln!("Error: {}", message),
                    _ => {}
                }
            }
        }
        Err(e) => eprintln!("ERR: {:?}", e),
    }
}
