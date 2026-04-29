use crate::ConversationTurn;

/// Summarize a session into memory entries. Returns extracted entries.
///
/// The full implementation will eventually call artemis_core::chat() to extract
/// facts, decisions, and context from session logs. Currently a no-op stub.
pub fn extract(_session_log: &[ConversationTurn]) -> Vec<String> {
    vec![]
}
