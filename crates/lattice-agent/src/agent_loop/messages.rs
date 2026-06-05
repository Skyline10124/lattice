// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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

/// Append a converted message to the provider request cache (M-21).
pub(crate) fn append_llm_message(
    llm_messages: &mut Vec<lattice_llm::Message>,
    msg: &GraphMessage,
) -> Result<(), String> {
    llm_messages.push(convert_to_llm_message(msg)?);
    Ok(())
}

/// Build the initial provider message list from the turn's GraphMessage buffer.
pub(crate) fn graph_messages_to_llm(
    messages: &[GraphMessage],
) -> Result<Vec<lattice_llm::Message>, String> {
    messages.iter().map(convert_to_llm_message).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_session::GraphToolCall;

    #[test]
    fn append_llm_message_extends_provider_cache() {
        let mut llm_messages = vec![lattice_llm::Message {
            role: lattice_llm::Role::User,
            content: "hi".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];

        let assistant = GraphMessage {
            role: "assistant".to_string(),
            content: "calling".to_string(),
            reasoning_content: None,
            tool_calls: Some(vec![GraphToolCall {
                id: "t1".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path":"a.txt"}"#.to_string(),
            }]),
            tool_call_id: None,
            name: None,
        };
        append_llm_message(&mut llm_messages, &assistant).expect("append");

        assert_eq!(llm_messages.len(), 2);
        assert_eq!(llm_messages[1].role, lattice_llm::Role::Assistant);
        assert_eq!(llm_messages[1].tool_calls.as_ref().unwrap().len(), 1);
    }
}
