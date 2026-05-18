use lattice_session::GraphMessage;

/// Convert a `GraphMessage` to an `lattice_llm::Message` for the LLM request.
pub(crate) fn convert_to_llm_message(msg: &GraphMessage) -> Result<lattice_llm::Message, String> {
    let role = match msg.role.as_str() {
        "system" => lattice_llm::Role::System,
        "user" => lattice_llm::Role::User,
        "assistant" => lattice_llm::Role::Assistant,
        "tool" => lattice_llm::Role::Tool,
        other => return Err(format!("unknown message role '{}'", other)),
    };

    Ok(lattice_llm::Message {
        role,
        content: msg.content.clone(),
        reasoning_content: msg.reasoning_content.clone(),
        tool_calls: msg.tool_calls.as_ref().map(|calls| {
            calls
                .iter()
                .map(|tc| lattice_llm::ToolCall {
                    id: tc.id.clone(),
                    function: lattice_llm::FunctionCall {
                        name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    },
                })
                .collect()
        }),
        tool_call_id: msg.tool_call_id.clone(),
        name: msg.name.clone(),
    })
}
