// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice lancet` — LANCET inspection and self-check subcommands.
//!
//! - `lancet status`: reads the external project-keyed audit store via
//!   `audit_db_path_for_project` (no CEP server) and shows recent calls and
//!   the last guard verdict.
//! - `lancet doctor`: catalog count, transport smoke, and invariant-card parse.

use anyhow::{Context, Result};
use clap::Subcommand;
use colored::Colorize;
use lancet_api::{
    LancetDoctorOutput, LancetDoctorRequest, LancetStatusOutput, collect_status, run_doctor_checks,
};
#[cfg(test)]
use std::path::Path;

/// `lattice lancet` subcommand group.
#[derive(Subcommand)]
pub enum LancetAction {
    /// Show audit-derived CEP status: recent calls and last guard verdict.
    Status {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Run LANCET self-check.
    Doctor {},
}

/// Dispatch the `lancet` subcommand group. `None` action defaults to `Status`.
pub fn run(action: Option<LancetAction>, global_json: bool) -> Result<()> {
    match action.unwrap_or(LancetAction::Status { json: false }) {
        LancetAction::Status { json } => run_status(json || global_json),
        LancetAction::Doctor {} => run_doctor(global_json),
    }
}

// ---------------------------------------------------------------------------
// lancet status
// ---------------------------------------------------------------------------

fn run_status(json: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("current working directory")?;
    let output = collect_status(&project_root)?;

    if json {
        let value = status_json_value(&output);
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        render_status_text(&output);
    }
    Ok(())
}

fn status_json_value(output: &LancetStatusOutput) -> serde_json::Value {
    serde_json::json!({
        "recent_calls": output.recent_calls.iter().map(|c| serde_json::json!({
            "audit_id": c.audit_id,
            "method": c.method,
            "impact": c.impact,
            "verdict": c.verdict,
            "duration_ms": c.duration_ms,
        })).collect::<Vec<_>>(),
        "last_guard_verdict": output.last_guard_verdict,
    })
}

fn render_status_text(output: &LancetStatusOutput) {
    for line in status_text_lines(output) {
        println!("{line}");
    }
}

fn status_text_lines(output: &LancetStatusOutput) -> Vec<String> {
    let mut lines = vec!["lancet status".dimmed().to_string()];
    lines.push("Recent CEP Calls:".bold().to_string());
    if output.recent_calls.is_empty() {
        lines.push(format!("  {}", "none".dimmed()));
    } else {
        for call in &output.recent_calls {
            let verdict = call.verdict.as_deref().unwrap_or("?");
            let dur = call
                .duration_ms
                .map(|d| format!("{}ms", d))
                .unwrap_or_default();
            lines.push(format!(
                "  [{}] {} {} {} {}",
                call.audit_id,
                call.method.cyan(),
                call.impact.dimmed(),
                verdict,
                dur
            ));
        }
    }
    let verdict_str = output.last_guard_verdict.as_deref().unwrap_or("none");
    lines.push(String::new());
    lines.push(format!(
        "{}: {}",
        "Last Guard Verdict".bold(),
        verdict_str.dimmed()
    ));
    lines
}

// ---------------------------------------------------------------------------
// lancet doctor
// ---------------------------------------------------------------------------

/// Public entry point for LANCET runtime diagnostics.
pub fn run_doctor(json: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("current working directory")?;
    let request = LancetDoctorRequest::new(project_root);
    let output = run_doctor_checks(request);
    emit_doctor_result(&output, json)
}

#[cfg(test)]
pub(crate) fn run_doctor_inner_for_project_root(project_root: &Path, json: bool) -> Result<()> {
    let request = LancetDoctorRequest::new(project_root);
    let output = run_doctor_checks(request);
    emit_doctor_result(&output, json)
}

fn emit_doctor_result(output: &LancetDoctorOutput, json: bool) -> Result<()> {
    let all_ok = output.issues.is_empty();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&doctor_json_value(output))?
        );
    } else {
        println!("{}", "lancet doctor".dimmed());
        for item in &output.ok_items {
            println!("  {} {}", "\u{2713}".green(), item);
        }
        for warning in &output.warnings {
            println!("  {} {}", "!".yellow(), warning.yellow());
        }
        for issue in &output.issues {
            println!("  {} {}", "\u{2717}".red(), issue.red());
        }
    }

    if !all_ok {
        anyhow::bail!("lancet doctor: {} check(s) failed", output.issues.len());
    }
    Ok(())
}

fn doctor_json_value(output: &LancetDoctorOutput) -> serde_json::Value {
    let all_ok = output.issues.is_empty();
    let checks: Vec<_> = output
        .ok_items
        .iter()
        .map(|s| serde_json::json!({"status": "ok", "message": s}))
        .chain(
            output
                .warnings
                .iter()
                .map(|s| serde_json::json!({"status": "warn", "message": s})),
        )
        .chain(
            output
                .issues
                .iter()
                .map(|s| serde_json::json!({"status": "fail", "message": s})),
        )
        .collect();
    serde_json::json!({
        "ok": all_ok,
        "checks": checks,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_env::ScopedXdgStateHome;
    use clap::Parser;
    use lancet_native::audit::{AuditOutcome, CallAuditMetadata};
    use lancet_native::{CepAuditContext, CepAuditStore};
    use tempfile::TempDir;

    // -- Clap surface tests --

    /// Test wrapper that wraps LancetAction as a top-level subcommand.
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: LancetAction,
    }

    fn try_parse_action(args: &[&str]) -> std::result::Result<LancetAction, clap::Error> {
        let mut full = vec!["lancet"];
        full.extend_from_slice(args);
        TestCli::try_parse_from(full).map(|c| c.action)
    }

    #[test]
    fn lancet_status_json_flag_parses() {
        let action = try_parse_action(&["status", "--json"]);
        assert!(action.is_ok(), "should parse `lancet status --json`");
        match action.unwrap() {
            LancetAction::Status { json } => assert!(json),
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn lancet_status_no_args_parses() {
        let action = try_parse_action(&["status"]);
        assert!(action.is_ok());
        match action.unwrap() {
            LancetAction::Status { json } => assert!(!json),
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn lancet_doctor_parses() {
        let action = try_parse_action(&["doctor"]);
        assert!(action.is_ok());
        assert!(matches!(action.unwrap(), LancetAction::Doctor {}));
    }

    // -- Status tests --

    fn seed_audit_db(dir: &Path) {
        let db_path =
            lancet_api::audit_db_path_for_project(dir).expect("resolve external audit path");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let audit = CepAuditStore::open(&db_path).expect("open audit db");
        let ctx = CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            model_id: "test-model".into(),
            runtime_version: "test".into(),
        };
        // A session.todo_write row with metadata.
        let start = audit
            .start_call(
                &ctx,
                1,
                "session.todo_write",
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "todo written".into(),
                },
                Some("native_handler"),
                Some(r#"{"todo_ops":1}"#),
            )
            .unwrap();
        // A regular call row
        let start2 = audit
            .start_call(
                &ctx,
                2,
                "observe.workspace",
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start2,
                AuditOutcome::Success {
                    summary: "read ok".into(),
                },
                Some("native_handler"),
                None,
            )
            .unwrap();
    }

    #[test]
    fn cep_status_seeded_audit_json() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_audit_db(dir.path());

        let output = collect_status(dir.path()).unwrap();

        // Should have at least the two seeded rows
        assert!(!output.recent_calls.is_empty(), "should have recent calls");
        // Last guard verdict derived from newest row
        assert!(
            output.last_guard_verdict.is_some(),
            "should have last guard verdict"
        );
        // Check JSON schema keys
        let json = serde_json::json!({
            "recent_calls": output.recent_calls.iter().map(|c| serde_json::json!({
                "audit_id": c.audit_id,
                "method": c.method,
                "impact": c.impact,
                "verdict": c.verdict,
                "duration_ms": c.duration_ms,
            })).collect::<Vec<_>>(),
            "last_guard_verdict": output.last_guard_verdict,
        });
        assert!(
            json.get("recent_calls").is_some(),
            "JSON must have recent_calls"
        );
        assert!(
            json.get("last_guard_verdict").is_some(),
            "JSON must have last_guard_verdict"
        );

        // Verify method values in recent_calls
        let calls = json["recent_calls"].as_array().unwrap();
        let has_todo = calls
            .iter()
            .any(|c| c["method"].as_str() == Some("session.todo_write"));
        assert!(
            has_todo,
            "recent_calls should include session.todo_write row"
        );
    }

    // -- Doctor tests --

    #[test]
    fn cep_doctor_passes_on_repo_root() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root");

        let result = run_doctor_inner_for_project_root(workspace_root, false);
        assert!(
            result.is_ok(),
            "doctor should pass on repo root: {:?}",
            result
        );
    }
}
