// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! 操作员查询只经后台 IPC，不打开数据库、不加载执行配置。
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use lattice_api::local::{
    AuditSelection, ChangeSeq, LocalClient, SessionId, ToolResultOutcome, TurnId,
};
use std::path::Path;

fn parse_change(value: &str) -> Result<ChangeSeq, String> {
    super::session::parse_node_id(value).map(|id| ChangeSeq(id.0))
}
fn parse_outcome(value: &str) -> Result<String, String> {
    outcome(value)
        .map(|_| value.into())
        .map_err(|_| "expected succeeded/failed/not_executed/indeterminate".into())
}
fn outcome(value: &str) -> Result<ToolResultOutcome> {
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| lattice_api::ApiError::BadRequest("invalid outcome".into()).into())
}
#[derive(Args, Debug, Clone)]
pub struct AuditQueryArgs {
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub turn: Option<String>,
    #[arg(long)]
    pub method: Option<String>,
    #[arg(long,value_parser=parse_outcome)]
    pub outcome: Option<String>,
    /// Read invocation changes ordered by the global ChangeSeq.
    #[arg(long)]
    pub tail: bool,
    /// Exclusive change sequence to start after; only valid with --tail.
    #[arg(long,requires="tail",value_parser=parse_change)]
    pub since: Option<ChangeSeq>,
    #[arg(long)]
    pub json: bool,
}
#[derive(Args, Debug, Clone)]
pub struct AuditReplayArgs {
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub turn: String,
    #[arg(long)]
    pub json: bool,
}
#[derive(Subcommand, Debug, Clone)]
pub enum AuditAction {
    Query(AuditQueryArgs),
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
    let workspace = std::env::current_dir().context("current working directory")?;
    let client = super::session::local_client().await?;
    match cmd.action {
        Some(AuditAction::Replay(args)) => {
            let session = SessionId::parse(&args.session)
                .map_err(|_| lattice_api::ApiError::BadRequest("invalid SessionId".into()))?;
            let turn = TurnId::parse(&args.turn)
                .map_err(|_| lattice_api::ApiError::BadRequest("invalid TurnId".into()))?;
            let replay = client.audit_replay(workspace, session, turn).await?;
            if args.json || global_json {
                println!("{}", serde_json::to_string(&replay)?);
            } else {
                println!(
                    "session={} turn={} latest_change={} calls={}",
                    replay.session_id,
                    replay.turn_id,
                    replay.latest_change.0,
                    replay.calls.len()
                );
                for row in replay.calls {
                    println!(
                        "{} {} {:?}",
                        row.invocation_id, row.binding.method, row.state
                    );
                }
            }
            Ok(())
        }
        Some(AuditAction::Query(args)) => run_query(&client, args, global_json, &workspace).await,
        None => run_query(&client, cmd.query, global_json, &workspace).await,
    }
}
pub(super) async fn run_query(
    client: &LocalClient,
    args: AuditQueryArgs,
    global_json: bool,
    workspace: &Path,
) -> Result<()> {
    let selection = AuditSelection {
        session_id: args
            .session
            .as_deref()
            .map(SessionId::parse)
            .transpose()
            .map_err(|_| lattice_api::ApiError::BadRequest("invalid SessionId".into()))?,
        turn_id: args
            .turn
            .as_deref()
            .map(TurnId::parse)
            .transpose()
            .map_err(|_| lattice_api::ApiError::BadRequest("invalid TurnId".into()))?,
        method: args.method,
        outcome: args.outcome.as_deref().map(outcome).transpose()?,
    };
    if args.tail {
        let page = client
            .audit_tail(
                workspace.into(),
                selection,
                args.since.unwrap_or(ChangeSeq(0)),
                256,
            )
            .await?;
        if args.json || global_json {
            println!("{}", serde_json::to_string(&page)?);
        } else {
            println!(
                "latest_change={} next_after={} done={}",
                page.latest_change.0, page.next_after.0, page.done
            );
            for change in page.rows {
                println!(
                    "{} {} {} {:?}",
                    change.sequence.0,
                    change.row.invocation_id,
                    change.row.binding.method,
                    change.row.state
                );
            }
        }
    } else {
        let page = client
            .audit_query(workspace.into(), selection, None, 256)
            .await?;
        if args.json || global_json {
            println!("{}", serde_json::to_string(&page)?);
        } else {
            println!(
                "latest_change={} next={:?}",
                page.latest_change.0, page.next
            );
            for row in page.rows {
                println!(
                    "{} {} {:?}",
                    row.invocation_id, row.binding.method, row.state
                );
            }
        }
    }
    Ok(())
}
