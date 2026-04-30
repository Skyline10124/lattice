use lattice_core::{chat_complete, resolve, Message, Role};

#[tokio::main]
async fn main() {
    let models = [
        ("glm-5.1", "OpenAI", false),
        ("glm-5", "OpenAI", false),
        ("kimi-k2.6", "OpenAI", false),
        ("kimi-k2.5", "OpenAI", false),
        ("deepseek-v4-pro", "OpenAI", true),
        ("deepseek-v4-flash", "OpenAI", false),
        ("mimo-v2-pro", "OpenAI", false),
        ("mimo-v2-omni", "OpenAI", false),
        ("mimo-v2.5-pro", "OpenAI", false),
        ("mimo-v2.5", "OpenAI", false),
        ("minimax-m2.7", "Anthropic", true),
        ("minimax-m2.5", "Anthropic", true),
        ("qwen3.6-plus", "Alibaba/OpenAI", false),
        ("qwen3.5-plus", "Alibaba/OpenAI", false),
    ];

    for (model, family, has_thinking) in &models {
        let resolved = match resolve(model) {
            Ok(r) => {
                if r.provider != "opencode-go" {
                    continue;
                }
                r
            }
            Err(_) => {
                println!("{} RESOLVE FAILED", model);
                continue;
            }
        };

        let msg = Message {
            role: Role::User,
            content: "Say hi in one word.".into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        };

        match chat_complete(&resolved, &[msg], &[]).await {
            Ok(r) => {
                let think = if let Some(ref t) = r.reasoning_content {
                    format!("[think:{}b]", t.len())
                } else {
                    String::new()
                };
                let content = r.content.as_deref().unwrap_or("(empty)");
                let preview = &content[..content.len().min(60)];
                let status = if *has_thinking && r.reasoning_content.is_some() {
                    "OK+think"
                } else if *has_thinking {
                    "OK(no think)"
                } else {
                    "OK"
                };
                println!("{} {} {} {} {}", model, family, status, think, preview);
            }
            Err(e) => println!("{} {} ERROR: {:?}", model, family, e),
        }
    }
}
