use std::path::Path;

use lattice_core::types::Role;

use super::state::ChatMessage;

pub(super) async fn run(
    security: &crate::security::RuntimeSecurity,
    workdir: &Path,
    command: &str,
) -> ChatMessage {
    let output = match security.execute_bash(workdir, command).await {
        Ok(output) if output.trim().is_empty() => "(no output)".to_string(),
        Ok(output) => output.trim().to_string(),
        Err(err) => format!("bash: {err}"),
    };

    let mut message = ChatMessage::new(Role::Tool, output);
    message.collapsed = message.content.lines().count() > 6;
    message
}
