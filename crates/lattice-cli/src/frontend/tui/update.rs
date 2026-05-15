use super::action::TuiAction;
use super::app::App;
use anyhow::Result;

/// Applies normalized actions to the TUI coordinator.
pub(super) async fn apply_action(app: &mut App, action: TuiAction) -> Result<()> {
    match action {
        TuiAction::Tick => app.tick(),
        TuiAction::Key(key) => app.handle_key(key).await?,
        TuiAction::Mouse(mouse) => app.handle_mouse(mouse).await?,
        TuiAction::Paste(text) => app.insert_text(&text),
        TuiAction::StreamToken {
            turn_id,
            content,
            reasoning,
            done,
            error,
        } => {
            let turn_finished = done || error.is_some();
            app.apply_stream_token(turn_id, content, reasoning, done, error);
            if turn_finished {
                app.reap_audit().await;
            }
        }
        TuiAction::ToolOutput {
            turn_id,
            call_id,
            name,
            arguments,
            result,
            file_before,
        } => {
            app.apply_tool_output(turn_id, call_id, name, arguments, result, file_before);
        }
        TuiAction::ModelInfo {
            turn_id,
            model,
            provider,
        } => {
            if app.accepts_turn(turn_id) {
                app.current_model = model;
                app.current_provider = provider;
            }
        }
        TuiAction::RuntimeTurnStarted {
            turn_id,
            session_id,
            runtime_turn_id,
        } => {
            app.mark_runtime_turn_started(turn_id, session_id, runtime_turn_id);
        }
        TuiAction::RuntimeTurnFinished {
            turn_id,
            session_id,
            runtime_turn_id,
            status,
            completion,
            error,
        } => {
            if app.accepts_runtime_turn(turn_id, &session_id, &runtime_turn_id) {
                app.apply_runtime_completion(
                    turn_id,
                    &session_id,
                    &runtime_turn_id,
                    status,
                    completion.as_ref(),
                    error,
                );
                app.reap_audit().await;
            }
        }
    }
    Ok(())
}
