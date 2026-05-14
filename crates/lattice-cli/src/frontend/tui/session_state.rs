use lattice_core::types::Role;

use crate::session::SessionStore;

use super::state::ChatMessage;

#[allow(dead_code)] // Runtime-backed TUI turns no longer persist through this frontend helper.
pub(super) struct CompletedTurn {
    user: String,
    assistant: String,
}

#[allow(dead_code)] // Runtime-backed TUI turns no longer persist through this frontend helper.
pub(super) struct SaveCompletedTurnOutcome {
    pub(super) session: crate::session::Session,
    pub(super) error: Option<String>,
}

pub(super) fn messages_from_session(session: &crate::session::Session) -> Vec<ChatMessage> {
    session
        .messages
        .iter()
        .map(|msg| ChatMessage::new(role_from_session(&msg.role), msg.content.clone()))
        .collect()
}

pub(super) fn recount_tokens(messages: &[ChatMessage]) -> usize {
    use lattice_core::tokens::TokenEstimator;
    let mut total = 0usize;
    for msg in messages {
        total += TokenEstimator::estimate_text(&msg.content) as usize;
        if let Some(ref r) = msg.reasoning {
            total += TokenEstimator::estimate_text(r) as usize;
        }
    }
    total
}

#[allow(dead_code)] // Kept for legacy tests until frontend persistence helpers are deleted.
pub(super) fn completed_turn(
    save_sessions: bool,
    pending_user: Option<String>,
    messages: &[ChatMessage],
) -> Option<CompletedTurn> {
    if !save_sessions {
        return None;
    }

    let user = pending_user?;
    let start = messages
        .iter()
        .rposition(|msg| msg.role == Role::User)
        .map_or(0, |idx| idx.saturating_add(1));
    let assistant = messages
        .iter()
        .skip(start)
        .filter(|msg| msg.role == Role::Assistant && !msg.content.trim().is_empty())
        .map(|msg| msg.content.trim())
        .collect::<Vec<_>>()
        .join("\n\n");
    if assistant.is_empty() {
        return None;
    }

    Some(CompletedTurn { user, assistant })
}

#[allow(dead_code)] // Runtime owns persistence for active TUI turns.
pub(super) fn save_completed_turn(
    current_session: Option<crate::session::Session>,
    current_model: String,
    current_provider: String,
    turn: CompletedTurn,
) -> SaveCompletedTurnOutcome {
    let session = crate::session::finalize_session_turn(
        current_session,
        current_model,
        current_provider,
        turn.user,
        turn.assistant,
    );
    let manager = crate::session::session_manager();
    let error = manager
        .save(&session)
        .err()
        .map(|err| format!("Failed to save session: {err}"));

    SaveCompletedTurnOutcome { session, error }
}

fn role_from_session(role: &str) -> Role {
    match role.to_ascii_lowercase().as_str() {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_roles_map_to_ui_roles() {
        assert_eq!(role_from_session("assistant"), Role::Assistant);
        assert_eq!(role_from_session("system"), Role::System);
        assert_eq!(role_from_session("tool"), Role::Tool);
        assert_eq!(role_from_session("unknown"), Role::User);
    }

    #[test]
    fn completed_turn_ignores_empty_assistant() {
        let messages = vec![ChatMessage::new(Role::Assistant, "   ".into())];

        assert!(completed_turn(true, Some("hello".into()), &messages).is_none());
    }

    #[test]
    fn completed_turn_ignores_disabled_session_save() {
        let messages = vec![ChatMessage::new(Role::Assistant, "answer".into())];

        assert!(completed_turn(false, Some("hello".into()), &messages).is_none());
    }

    #[test]
    fn completed_turn_joins_split_assistant_segments() {
        let messages = vec![
            ChatMessage::new(Role::User, "question".into()),
            ChatMessage::new(Role::Assistant, "before tool".into()),
            ChatMessage::new(Role::Tool, "tool output".into()),
            ChatMessage::new(Role::Assistant, "after tool".into()),
        ];

        let turn = completed_turn(true, Some("question".into()), &messages).unwrap();

        assert_eq!(turn.assistant, "before tool\n\nafter tool");
    }
}
