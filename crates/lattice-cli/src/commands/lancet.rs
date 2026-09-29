// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice lancet` — LANCET inspection and self-check subcommands.
//!
//! - `lancet status`: 经后台读取当前项目调用状态，不组合 Tool runtime。
//! - `lancet doctor`: validates the workspace and generated closed catalog.

use anyhow::{Context, Result};
use clap::Subcommand;
use colored::Colorize;
use lancet_api::{LancetDoctorOutput, LancetDoctorRequest, run_doctor_checks};
use lattice_api::local::{LocalClient, OperatorStatus};
#[cfg(test)]
use std::path::Path;

/// `lattice lancet` subcommand group.
#[derive(Subcommand)]
pub enum LancetAction {
    /// 显示项目近期调用状态与全局变更高水位。
    Status {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Run LANCET self-check.
    Doctor {},
}

/// Dispatch the `lancet` subcommand group. `None` action defaults to `Status`.
pub async fn run(action: Option<LancetAction>, global_json: bool) -> Result<()> {
    match action.unwrap_or(LancetAction::Status { json: false }) {
        LancetAction::Status { json } => run_status(json || global_json).await,
        LancetAction::Doctor {} => run_doctor(global_json),
    }
}

// ---------------------------------------------------------------------------
// lancet status
// ---------------------------------------------------------------------------

async fn run_status(json: bool) -> Result<()> {
    let workspace = std::env::current_dir().context("current working directory")?;
    let client = super::session::local_client().await?;
    status_with_client(&client, workspace, json).await
}

pub(super) async fn status_with_client(
    client: &LocalClient,
    workspace: std::path::PathBuf,
    json: bool,
) -> Result<()> {
    let output: OperatorStatus = client.operator_status(workspace).await?;
    if json {
        println!("{}", serde_json::to_string(&output)?);
    } else {
        println!("lancet status\nlatest_change={}", output.latest_change.0);
        for row in output.recent {
            println!(
                "{} {} {:?}",
                row.invocation_id, row.binding.method, row.state
            );
        }
    }
    Ok(())
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
