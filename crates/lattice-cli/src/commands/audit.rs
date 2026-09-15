// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice audit` — query, tail, and replay the project InvocationStore.

use std::path::Path;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use lancet_api::{AuditReplayRequest, AuditRequest, query_audit, replay_audit, tail_audit};

/// 将操作员错误映射为固定的公开原因，避免回显私有路径和用户输入。
///
/// `OperatorAuditError::Config` 经 `lancet-api` 透传后只剩字符串，
/// 其中可能包含状态仓库路径和底层错误。因此依据错误构造处的固定前缀，
/// 从 `OPERATOR_FAILURE_SUMMARIES` 选择公开短语；未登记的原因按内部故障泛化。
///
/// 此映射依赖 `lancet-audit/src/operator.rs` 的错误构造文案。新增或修改
/// `Config` 原因时，需同步核对下方映射表，前缀匹配不提供类型化保证。
fn operator_error(error: lancet_api::LancetError) -> anyhow::Error {
    match error {
        lancet_api::LancetError::Config(message) => match operator_failure_summary(&message) {
            Some(summary) => anyhow::Error::new(lattice_api::local::LocalError {
                kind: lattice_api::local::LocalErrorKind::BadRequest,
                message: summary.to_string(),
                recovery: None,
            }),
            None => anyhow::Error::new(lancet_api::LancetError::Internal(message)),
        },
        error => anyhow::Error::new(error),
    }
}

/// `lancet-audit::operator` 的 `Config` 构造来源前缀 → 公开固定短语。
const OPERATOR_FAILURE_SUMMARIES: &[(&str, &str)] = &[
    (
        "invocation ledger not found",
        "invocation ledger not found for this project; run an agent turn in this project first",
    ),
    ("invalid session_id", "invalid session_id"),
    ("invalid turn_id", "invalid turn_id"),
    (
        "no invocations for turn_id=",
        "no invocations for the requested turn in the requested session",
    ),
    ("invalid ledger cursor", "invalid ledger cursor"),
    ("unknown invocation verdict", "unknown invocation verdict"),
];

fn operator_failure_summary(message: &str) -> Option<&'static str> {
    OPERATOR_FAILURE_SUMMARIES
        .iter()
        .find_map(|(prefix, summary)| message.starts_with(prefix).then_some(*summary))
}

#[derive(Args, Debug, Clone)]
pub struct AuditQueryArgs {
    /// Filter by canonical SessionId.
    #[arg(long)]
    pub session: Option<String>,
    /// Filter by canonical TurnId.
    #[arg(long)]
    pub turn: Option<String>,
    /// Filter by Tool method.
    #[arg(long)]
    pub method: Option<String>,
    /// Filter by lifecycle verdict.
    #[arg(long)]
    pub verdict: Option<String>,
    /// Pin query to an immutable ledger commit OID.
    #[arg(long = "at")]
    pub ledger_oid: Option<String>,
    /// Emit lifecycle transitions instead of current state.
    #[arg(long)]
    pub tail: bool,
    /// Exclusive ledger commit OID cursor for tail.
    #[arg(long)]
    pub since: Option<String>,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct AuditReplayArgs {
    /// Canonical SessionId.
    #[arg(long)]
    pub session: String,
    /// Canonical TurnId.
    #[arg(long)]
    pub turn: String,
    /// Pin replay to an immutable ledger commit OID.
    #[arg(long = "at")]
    pub ledger_oid: Option<String>,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AuditAction {
    /// Query current invocation state or tail transitions.
    Query(AuditQueryArgs),
    /// Replay one Turn's invocation records.
    Replay(AuditReplayArgs),
}

#[derive(Args, Debug, Clone)]
pub struct AuditCmd {
    #[command(subcommand)]
    pub action: Option<AuditAction>,
    #[command(flatten)]
    pub query: AuditQueryArgs,
}

pub async fn run(cmd: AuditCmd, global_json: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("current working directory")?;
    run_async(cmd, global_json, &project_root).await
}

async fn run_async(cmd: AuditCmd, global_json: bool, project_root: &Path) -> Result<()> {
    match cmd.action {
        Some(AuditAction::Replay(args)) => run_replay(args, global_json, project_root).await,
        Some(AuditAction::Query(args)) => run_query(args, global_json, project_root).await,
        None => run_query(cmd.query, global_json, project_root).await,
    }
}

async fn run_query(args: AuditQueryArgs, global_json: bool, project_root: &Path) -> Result<()> {
    let json = args.json || global_json;
    let mut request = AuditRequest::new(project_root);
    request.session_id = args.session;
    request.turn_id = args.turn;
    request.method = args.method;
    request.verdict = args.verdict;
    request.ledger_oid = args.ledger_oid;
    request.after = args.since;
    if args.tail {
        let output = tail_audit(request).await.map_err(operator_error)?;
        render_tail_output(&output, json)
    } else {
        let output = query_audit(request).await.map_err(operator_error)?;
        render_query_output(&output, json)
    }
}

async fn run_replay(args: AuditReplayArgs, global_json: bool, project_root: &Path) -> Result<()> {
    let mut request = AuditReplayRequest::new(project_root, args.session, args.turn);
    request.ledger_oid = args.ledger_oid;
    let output = replay_audit(request).await.map_err(operator_error)?;
    if args.json || global_json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "ledger={} session={} turn={} calls={}",
            output.trace.ledger_oid,
            output.trace.session_id,
            output.trace.turn_id,
            output.trace.calls.len()
        );
        for call in &output.trace.calls {
            println!(
                "  invocation_id={} method={} state={:?}",
                call.invocation_id, call.record.binding.method, call.record.state
            );
        }
    }
    Ok(())
}

fn render_query_output(output: &lancet_api::AuditQueryOutput, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(output)?);
    } else {
        println!("ledger={} rows={}", output.ledger_oid, output.row_count);
        for row in &output.rows {
            println!(
                "  invocation_id={} method={} state={:?}",
                row.invocation_id, row.record.binding.method, row.record.state
            );
        }
    }
    Ok(())
}

fn render_tail_output(output: &lancet_api::AuditTailOutput, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(output)?);
    } else {
        println!(
            "ledger={} tail_rows={} cursor={} done={}",
            output.ledger_oid,
            output.rows.len(),
            output.last_cursor.as_deref().unwrap_or("none"),
            output.done
        );
        for row in &output.rows {
            println!(
                "  commit={} invocation_id={} method={} state={:?}",
                row.ledger_oid, row.invocation_id, row.record.binding.method, row.record.state
            );
        }
    }
    Ok(())
}
