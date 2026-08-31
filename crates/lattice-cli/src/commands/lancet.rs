// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice lancet` — LANCET inspection and self-check subcommands.
//!
//! - `lancet status`: reads the project InvocationStore without composing a
//!   Tool runtime and shows recent typed invocation states.
//! - `lancet doctor`: validates the workspace and generated closed catalog.

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
    /// Show recent typed invocation states from the project ledger.
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
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        render_status_text(&output);
    }
    Ok(())
}

fn render_status_text(output: &LancetStatusOutput) {
    for line in status_text_lines(output) {
        println!("{line}");
    }
}

fn status_text_lines(output: &LancetStatusOutput) -> Vec<String> {
    let mut lines = vec!["lancet status".dimmed().to_string()];
    lines.push(format!(
        "Ledger: {}",
        output.ledger_oid.as_deref().unwrap_or("none")
    ));
    lines.push("Recent Invocations:".bold().to_string());
    if output.recent_calls.is_empty() {
        lines.push(format!("  {}", "none".dimmed()));
    } else {
        for call in &output.recent_calls {
            lines.push(format!(
                "  {} {} {:?}",
                call.invocation_id,
                call.record.binding.method.cyan(),
                call.record.state
            ));
        }
    }
    lines.push(String::new());
    lines.push(format!(
        "{}: {}",
        "Last Verdict".bold(),
        output.last_verdict.as_deref().unwrap_or("none").dimmed()
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
    use clap::Parser;

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
    // -- Doctor tests --

    #[test]
    fn generated_catalog_doctor_passes_on_repo_root() {
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
