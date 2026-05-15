use std::io::Write;

use anyhow::Result;
use colored::Colorize;
use lattice_runtime::{RuntimeTurnEvent, TurnCompletion, TurnStatus};

pub(crate) fn require_successful_runtime_completion(
    completion: TurnCompletion,
) -> Result<TurnCompletion> {
    match completion.status {
        TurnStatus::Completed => Ok(completion),
        TurnStatus::Cancelled => anyhow::bail!("runtime turn was cancelled"),
        TurnStatus::Error => anyhow::bail!(
            "{}",
            completion.error.as_deref().unwrap_or("runtime turn failed")
        ),
    }
}

#[cfg(any(test, debug_assertions))]
#[allow(dead_code)] // Integration-test support uses this via the debug-only test_support module.
pub(crate) fn render_completion_value(completion: &TurnCompletion) -> serde_json::Value {
    serde_json::json!({
        "content": completion.assistant_text,
        "final_report": completion.final_report.as_deref().unwrap_or_default(),
        "total_tokens": completion
            .token_usage
            .as_ref()
            .map(|usage| usage.total_tokens)
            .unwrap_or_default(),
    })
}

pub(crate) struct RuntimeEventRenderer {
    verbose: bool,
    json: bool,
    content_buf: String,
    reasoning_buf: String,
}

impl RuntimeEventRenderer {
    pub(crate) fn new(verbose: bool, json: bool) -> Self {
        Self {
            verbose,
            json,
            content_buf: String::new(),
            reasoning_buf: String::new(),
        }
    }

    pub(crate) fn render(&mut self, event: &RuntimeTurnEvent) -> Result<()> {
        match event {
            RuntimeTurnEvent::Token { text, .. } => {
                if !self.json {
                    print!("{text}");
                    std::io::stdout().flush()?;
                }
                self.content_buf.push_str(text);
            }
            RuntimeTurnEvent::Reasoning { text, .. } => {
                self.reasoning_buf.push_str(text);
            }
            RuntimeTurnEvent::ToolStarted {
                name,
                batch_index,
                batch_count,
                ..
            } => {
                self.flush_reasoning();
                if self.verbose && !self.json {
                    if *batch_count > 1 {
                        eprintln!(
                            "\n{} {}/{} {}",
                            "tool".dimmed(),
                            batch_index,
                            batch_count,
                            name
                        );
                    } else {
                        eprintln!("\n{} {}", "tool".dimmed(), name);
                    }
                }
            }
            RuntimeTurnEvent::ToolFinished { name, result, .. } => {
                if self.verbose && !self.json {
                    let preview = result.chars().take(240).collect::<String>();
                    eprintln!("\n{} {} -> {}", "tool".dimmed(), name, preview);
                }
            }
            RuntimeTurnEvent::Completed { .. } => {
                self.flush_reasoning();
            }
            RuntimeTurnEvent::Error { message, .. } => {
                self.flush_reasoning();
                eprintln!("{} {}", "error:".red(), message);
            }
            RuntimeTurnEvent::ManifestReady { .. } => {}
            RuntimeTurnEvent::Cancelled { .. } => {
                self.flush_reasoning();
                eprintln!("{}", "cancelled".yellow());
            }
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self, total_tokens: Option<u64>) -> Result<()> {
        self.flush_reasoning();
        if self.json {
            let out = serde_json::json!({
                "content": self.content_buf,
                "total_tokens": total_tokens.unwrap_or_default(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!();
        }
        std::io::stdout().flush()?;
        Ok(())
    }

    fn flush_reasoning(&mut self) {
        if self.verbose && !self.json && !self.reasoning_buf.is_empty() {
            eprintln!("{} {}", "reasoning:".dimmed(), self.reasoning_buf.trim());
        }
        self.reasoning_buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion_with_status(status: TurnStatus, error: Option<&str>) -> TurnCompletion {
        TurnCompletion {
            session_id: "test-session".into(),
            turn_id: lattice_runtime::TurnId::new(),
            status,
            assistant_text: String::new(),
            token_usage: None,
            task_state_snapshot: lattice_agent::TaskState::new(),
            final_report: None,
            turn_manifests: Vec::new(),
            model: "test-model".into(),
            provider: "test-provider".into(),
            saved_session_id: None,
            profile_drift_warning: None,
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn runtime_error_completion_becomes_cli_error() {
        let err = require_successful_runtime_completion(completion_with_status(
            TurnStatus::Error,
            Some("provider failed"),
        ))
        .unwrap_err();

        assert_eq!(err.to_string(), "provider failed");
    }

    #[test]
    fn runtime_cancelled_completion_becomes_cli_error() {
        let err = require_successful_runtime_completion(completion_with_status(
            TurnStatus::Cancelled,
            None,
        ))
        .unwrap_err();

        assert_eq!(err.to_string(), "runtime turn was cancelled");
    }

    #[test]
    fn runtime_completed_completion_remains_successful() {
        let completion = completion_with_status(TurnStatus::Completed, Some("ignored on success"));

        assert!(require_successful_runtime_completion(completion).is_ok());
    }
}
