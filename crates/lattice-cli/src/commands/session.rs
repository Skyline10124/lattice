// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session 命令通过 LocalClient 操作后台；本层仅处理配置、参数与呈现。
//! 找回与查询先于当前 profile/provider 校验；引用写操作不在前端执行。
use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Lattice config root (`$XDG_CONFIG_HOME` or the Linux config directory).
pub(crate) fn lattice_config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg = xdg.trim();
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// CLI/TUI 的输入历史目录；领域数据由后台持有的全局 lattice.redb 存储。
pub(crate) fn default_session_dir() -> PathBuf {
    lattice_config_dir().join("lattice").join("sessions")
}

/// The private-state layout every durable store is addressed through
/// (ADR 0005 §3).
pub(crate) fn state_layout() -> Result<lattice_session::StateLayout> {
    Ok(lattice_session::StateLayout::resolve()?)
}

pub(crate) fn parse_turn_id(value: &str) -> Result<lattice_api::local::TurnId, String> {
    lattice_api::local::TurnId::parse(value).map_err(|_| "expected canonical TurnId UUID".into())
}

pub(crate) async fn local_client() -> Result<lattice_api::local::LocalClient> {
    let client = lattice_api::local::LocalClient::new(state_layout()?);
    client.ensure_running().await?;
    Ok(client)
}

#[derive(clap::Args, Debug)]
pub struct SendArgs {
    pub prompt: String,
    /// Existing session to continue, by ID or unique prefix; omit to start a new session.
    #[arg(long = "session")]
    pub session_id: Option<String>,
    /// Agent to use. Required for a new session, and for an existing session positioned at the root node.
    #[arg(long, required_unless_present = "session_id")]
    pub agent: Option<String>,
}

pub(crate) fn parse_node_id(value: &str) -> Result<lattice_api::local::NodeId, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("expected decimal NodeId".into());
    }
    value
        .parse::<u64>()
        .map(lattice_api::local::NodeId)
        .map_err(|_| "NodeId exceeds u64".into())
}

#[derive(Subcommand)]
pub enum SessionAction {
    /// Submit a request; without --session the session is created in the acceptance transaction.
    Send(SendArgs),
    /// Move the conversation position to node N only; the original fork is kept and files are not rolled back.
    Rewind {
        session_id: String,
        #[arg(value_parser = parse_node_id)]
        target: lattice_api::local::NodeId,
    },
    /// List all sessions.
    List,
    /// Show the full session tree, including the root node.
    Log { session_id: String },
    /// Show per-turn token usage and estimated cost on the current path, with totals.
    Usage { session_id: String },
    /// Show every Step of a Turn by TurnId; --follow keeps observing.
    Turn {
        #[arg(value_parser = parse_turn_id)]
        turn_id: lattice_api::local::TurnId,
        #[arg(long)]
        follow: bool,
    },
    /// A taken-over cancel is not yet a durable Cancelled outcome; a cancel not taken over does not mean no background work exists.
    Cancel { session_id: String },
}

pub async fn run(
    action: SessionAction,
    json: bool,
    verbose: bool,
) -> Result<std::process::ExitCode> {
    let api = local_client().await?;
    match action {
        SessionAction::Send(args) => {
            return crate::commands::run::run_session_prompt(&api, args, verbose, json).await;
        }
        SessionAction::Turn { turn_id, follow } => {
            return show_turn(
                &api,
                turn_id,
                follow,
                json,
                verbose,
                &mut std::io::stdout(),
                &mut std::io::stderr(),
            )
            .await;
        }
        SessionAction::Cancel { session_id } => {
            return cancel_current_turn(
                &api,
                &session_id,
                json,
                &mut std::io::stdout(),
                &mut std::io::stderr(),
            )
            .await;
        }
        SessionAction::List => {
            let rows = api.list_sessions().await?;
            if json {
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                for row in rows {
                    println!(
                        "{} project={} current={} nodes={} unclosed_turn={} owner={} updated={}",
                        display_id(&row.session.session_id.to_string()),
                        row.session.project_store_key,
                        row.session.current.0,
                        row.session.node_count,
                        row.session
                            .unclosed_turn
                            .map_or_else(|| "none".into(), |id| id.to_string()),
                        row.owner,
                        row.session.updated_at
                    );
                }
            }
        }
        SessionAction::Log { session_id } => {
            let id = api.resolve_session(&session_id).await?.session_id;
            let log = api.log(id).await?;
            if json {
                println!("{}", serde_json::to_string(&log)?);
            } else {
                print!("{}", render_log(&log));
            }
        }
        SessionAction::Usage { session_id } => {
            let usage = api.usage(&session_id).await?;
            if json {
                println!("{}", serde_json::to_string(&usage)?);
            } else {
                print!("{}", render_usage(&usage));
            }
        }
        SessionAction::Rewind { session_id, target } => {
            let id = api.resolve_session(&session_id).await?.session_id;
            let root =
                tokio::task::spawn_blocking(lattice_api::resolve_local_workspace_root).await??;
            let session = api.rewind(id, target, root.canonical_path().into()).await?;
            if json {
                println!("{}", serde_json::to_string(&session)?);
            } else {
                println!(
                    "session {} current={}; files were not rolled back",
                    session.session_id, session.current.0
                );
            }
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

pub(super) async fn show_turn(
    api: &lattice_api::local::LocalClient,
    turn_id: lattice_api::local::TurnId,
    follow: bool,
    json: bool,
    verbose: bool,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> Result<std::process::ExitCode> {
    let detail = if follow {
        api.wait_for_turn(turn_id).await?
    } else {
        match api.turn(turn_id).await? {
            lattice_api::local::TurnLookup::Accepted { turn } => turn,
            lattice_api::local::TurnLookup::InFlight => {
                if json {
                    writeln!(
                        out,
                        "{}",
                        serde_json::json!({"status":"in_flight","turn_id":turn_id})
                    )?;
                } else {
                    writeln!(out, "turn {turn_id}: in flight; admission is still pending")?;
                }
                return Ok(std::process::ExitCode::SUCCESS);
            }
        }
    };
    if follow {
        return super::run::follow_with_output(api, detail, verbose, json, out, err).await;
    }
    if json {
        writeln!(out, "{}", serde_json::to_string(&detail)?)?;
    } else {
        write!(out, "{}", render_turn(&detail))?;
    }
    Ok(std::process::ExitCode::SUCCESS)
}
pub(super) async fn cancel_current_turn(
    api: &lattice_api::local::LocalClient,
    prefix: &str,
    json: bool,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> Result<std::process::ExitCode> {
    use lattice_api::local::{CancelStatus, LocalUpdate};
    let session_id = api.resolve_session(prefix).await?.session_id;
    let head = api.session_head(session_id).await?;
    // 查头后就固定目标；回执丢失不能丢掉本次取消的身份，也不能追取消新一轮。
    let Some(turn_id) = head.turn_id.filter(|_| head.owner) else {
        if json {
            writeln!(
                out,
                "{}",
                serde_json::json!({"type":"cancel","session_id":session_id,"turn_id":null,"status":"none"})
            )?;
        } else {
            writeln!(out, "session {session_id}: no running turn to cancel")?;
        }
        return Ok(std::process::ExitCode::SUCCESS);
    };
    let (status, owned, error) = match api.cancel(turn_id).await {
        Ok(true) => (CancelStatus::Taken, Some(true), None),
        Ok(false) => (CancelStatus::NotTaken, Some(false), None),
        Err(error) => (CancelStatus::Unknown, None, Some(error)),
    };
    if json {
        let mut record = serde_json::to_value(LocalUpdate::Cancel {
            turn_id,
            status,
            owned,
            error: error.clone(),
        })?;
        record["session_id"] = serde_json::to_value(session_id)?;
        writeln!(out, "{record}")?;
    } else {
        let message = match status {
            CancelStatus::Taken => "taken; the durable outcome is still to be queried",
            CancelStatus::NotTaken => "not_taken; background work may still exist",
            _ => "unknown; query the original TurnId",
        };
        writeln!(out, "session {session_id} turn {turn_id}: {message}")?;
        if let Some(error) = &error {
            super::run::render_human_error(&serde_json::to_value(error)?, err)?;
        }
    }
    Ok(std::process::ExitCode::from(u8::from(error.is_some())))
}

fn render_log(log: &lattice_api::local::SessionLog) -> String {
    use std::fmt::Write;
    let mut text = format!(
        "session {} current={} agent={} unclosed_turn={}\n",
        log.session.session_id,
        log.session.current.0,
        summarize(log.current_agent.as_deref().unwrap_or("none"), 120),
        log.unclosed_turn
            .map_or_else(|| "none".into(), |id| id.to_string())
    );
    let mut children = std::collections::BTreeMap::<_, Vec<_>>::new();
    for row in &log.nodes {
        children.entry(row.node.parent).or_default().push(row);
    }
    let mut stack: Vec<_> = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|row| (*row, 0))
        .collect();
    while let Some((row, depth)) = stack.pop() {
        let kind = match row.node.kind {
            lattice_api::local::NodeKind::Root => "root".into(),
            lattice_api::local::NodeKind::UserMessage => "user".into(),
            lattice_api::local::NodeKind::Step { ordinal } => format!("step {ordinal}"),
        };
        let _ = writeln!(
            text,
            "{}{} {} {kind}",
            "  ".repeat(depth),
            if row.current { "*" } else { " " },
            row.node.id.0
        );
        if let Some(turn) = &row.turn_start {
            let _ = writeln!(
                text,
                "{}turn {} agent={} summary={}",
                "  ".repeat(depth + 1),
                turn.turn_id,
                turn.agent_name,
                serde_json::to_string(&turn.summary).expect("summary")
            );
        }
        if let Some(rows) = children.get(&Some(row.node.id)) {
            stack.extend(rows.iter().rev().map(|child| (*child, depth + 1)));
        }
    }
    text
}

fn render_usage(usage: &lattice_api::local::SessionUsage) -> String {
    use std::fmt::Write;
    let mut text = format!(
        "session {} position={}\n",
        usage.session_id, usage.position.0
    );
    text.push_str("#\tTURN\tPROVIDER/MODEL\tATTEMPTS\tINPUT\tCACHE_READ\tCACHE_WRITE\tOUTPUT\tREASONING\tESTIMATED USD\n");
    for turn in &usage.turns {
        let _ = writeln!(
            text,
            "{}\t{}\t{}/{}\t{}",
            turn.ordinal,
            turn.turn_id,
            turn.provider,
            summarize(&turn.model, 120),
            render_usage_values(&turn.usage)
        );
    }
    let _ = writeln!(text, "total\t-\t-\t{}", render_usage_values(&usage.total));
    text
}

fn render_usage_values(usage: &lattice_api::local::UsageSummary) -> String {
    let tokens = [
        &usage.input,
        &usage.cache_read,
        &usage.cache_write,
        &usage.output,
        &usage.reasoning,
    ]
    .map(|metric| render_metric(metric, |value| value.to_string()));
    let cost = render_metric(&usage.estimated_cost_usd, |value| format!("≈${value:.2}"));
    format!("{}\t{}\t{cost}", usage.attempts, tokens.join("\t"))
}

fn render_metric<T: Copy>(
    metric: &lattice_api::local::MetricSummary<T>,
    format_value: impl FnOnce(T) -> String,
) -> String {
    let Some(known) = metric.known else {
        return "-".into();
    };
    let mut value = format_value(known);
    if !metric.is_complete() {
        value.push_str(if metric.overflowed {
            " (partial, overflow)"
        } else {
            " (partial)"
        });
    }
    value
}

pub(super) fn render_turn(turn: &lattice_api::local::TurnDetail) -> String {
    use std::fmt::Write;
    let mut text = format!(
        "turn {} session={} agent={}\nsummary={}\nusage={}\n",
        turn.turn_id,
        turn.session_id,
        turn.agent_name,
        serde_json::to_string(&turn.summary).expect("summary"),
        serde_json::to_string(&turn.usage).expect("usage")
    );
    for step in &turn.steps {
        let _ = writeln!(
            text,
            "step {} node={} progress={} end={} updated_at={}",
            step.key.ordinal,
            step.node.0,
            serde_json::to_string(&step.progress).expect("progress"),
            serde_json::to_string(&step.end).expect("end"),
            step.updated_at
        );
        for attempt in &step.attempts {
            let _ = writeln!(
                text,
                "  attempt {}: {}",
                attempt.attempt,
                serde_json::to_string(attempt).expect("attempt")
            );
        }
        for slot in &step.slots {
            let _ = writeln!(
                text,
                "  slot {}: {}",
                slot.invocation_id,
                serde_json::to_string(slot).expect("slot")
            );
        }
    }
    text
}

fn display_id(id: &str) -> String {
    if id.len() > 8 {
        let end = id.char_indices().nth(8).map(|(i, _)| i).unwrap_or(id.len());
        format!("{}..", &id[..end])
    } else {
        id.to_string()
    }
}

/// Truncate `text` to at most `max` display columns, appending `…` if cut.
///
/// NOTE: `lattice-agent/src/agent_loop/mod.rs::truncate_to_width` contains a
/// similar column-width truncation loop. Keep both in sync if this logic changes.
fn summarize(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.width() <= max {
        return one_line;
    }
    let mut width = 0_usize;
    let mut end = 0_usize;
    let ellipsis = '\u{2026}';
    let ellipsis_width = ellipsis.width().unwrap_or(1);
    let limit = max.saturating_sub(ellipsis_width);
    for (i, c) in one_line.char_indices() {
        let cw = c.width().unwrap_or(0);
        if width + cw > limit {
            break;
        }
        width += cw;
        end = i + c.len_utf8();
    }
    let mut out = one_line[..end].to_string();
    out.push(ellipsis);
    out
}

#[cfg(test)]
mod session_cli {
    use super::*;
    use clap::{CommandFactory, Parser};
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: SessionAction,
    }
    #[test]
    fn saturated_usage_is_not_presented_as_an_exact_total() {
        let metric = lattice_api::local::MetricSummary {
            known: Some(u64::MAX),
            unknown_attempts: 0,
            overflowed: true,
        };
        assert_eq!(
            render_metric(&metric, |value| value.to_string()),
            format!("{} (partial, overflow)", u64::MAX)
        );
    }

    #[test]
    fn session_surface_and_numeric_nodes() {
        let command = TestCli::command();
        assert_eq!(
            command
                .get_subcommands()
                .map(|c| c.get_name())
                .collect::<Vec<_>>(),
            ["send", "rewind", "list", "log", "usage", "turn", "cancel"]
        );
        for n in ["0", "18446744073709551615"] {
            assert!(TestCli::try_parse_from(["session", "rewind", "abcd", n]).is_ok());
        }
        for n in ["-1", "+1", "aabb", "18446744073709551616"] {
            assert!(TestCli::try_parse_from(["session", "rewind", "abcd", n]).is_err());
        }
        assert!(TestCli::try_parse_from(["session", "send", "hello", "--agent", "coder"]).is_ok());
        assert!(TestCli::try_parse_from(["session", "send", "hello", "--session", "abcd"]).is_ok());
        assert!(TestCli::try_parse_from(["session", "send", "hello"]).is_err());
        assert!(TestCli::try_parse_from(["session", "log"]).is_err());
        assert!(TestCli::try_parse_from(["session", "rewind", "abcd"]).is_err());
        let id = "10000000-0000-4000-8000-000000000001";
        assert!(TestCli::try_parse_from(["session", "turn", id]).is_ok());
        assert!(TestCli::try_parse_from(["session", "cancel", "abcd"]).is_ok());
        assert!(
            TestCli::try_parse_from([
                "session",
                "send",
                "hello",
                "--agent",
                "coder",
                "--turn-id",
                id
            ])
            .is_err()
        );
        assert!(TestCli::try_parse_from(["session", "cancel", "abcd", "invalid"]).is_err());
        let mut command = TestCli::command();
        let help = command
            .find_subcommand_mut("cancel")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("does not mean no background work exists"));
        assert!(help.contains("not yet a durable Cancelled outcome"));
    }
    #[test]
    fn tree_output_keeps_root_branches_current_and_notice() {
        let log: lattice_api::local::SessionLog = serde_json::from_value(serde_json::json!({
            "session": {"session_id":"11111111111111111111111111111111", "project_store_key":"a".repeat(64), "current":4, "created_at":"2026-09-27T00:00:00Z", "updated_at":"2026-09-27T00:00:00Z"},
            "current_agent":"beta", "unclosed_turn":"20000000-0000-4000-8000-000000000002",
            "nodes":[
                {"node":{"id":0,"parent":null,"turn_id":null,"kind":{"kind":"root"}},"current":false,"turn_start":null},
                {"node":{"id":1,"parent":0,"turn_id":"10000000-0000-4000-8000-000000000001","kind":{"kind":"user_message"}},"current":false,"turn_start":{"turn_id":"10000000-0000-4000-8000-000000000001","agent_name":"alpha","summary":{"outcome":"cancelled","progress":{"progress":"closed","end":{"end":"stop","cause":{"cause":"cancelled"}}},"notice":{"level":"info","text":"cancelled notice"}}}},
                {"node":{"id":2,"parent":1,"turn_id":"10000000-0000-4000-8000-000000000001","kind":{"kind":"step","ordinal":0}},"current":false,"turn_start":null},
                {"node":{"id":3,"parent":0,"turn_id":"20000000-0000-4000-8000-000000000002","kind":{"kind":"user_message"}},"current":false,"turn_start":{"turn_id":"20000000-0000-4000-8000-000000000002","agent_name":"beta","summary":{"outcome":"failed","progress":{"progress":"reserved"},"notice":{"level":"error","text":"process exited"}}}},
                {"node":{"id":4,"parent":3,"turn_id":"20000000-0000-4000-8000-000000000002","kind":{"kind":"step","ordinal":0}},"current":true,"turn_start":null}
            ]
        })).unwrap();
        let text = render_log(&log);
        assert!(text.contains("current=4 agent=beta"));
        assert!(text.contains("  0 root"));
        assert!(text.contains("    1 user"));
        assert!(text.contains("      2 step 0"));
        assert!(text.contains("    3 user"));
        assert!(text.contains("    * 4 step 0"));
        assert!(text.contains("cancelled notice"));
        assert!(text.contains("alpha"));
    }
    #[test]
    fn turn_output_keeps_reserved_node_and_process_exit_conclusion() {
        let turn: lattice_api::local::TurnDetail = serde_json::from_value(serde_json::json!({
            "session_id":"11111111111141118111111111111111",
            "turn_id":"10000000-0000-4000-8000-000000000001",
            "user_node":1,"agent_name":"alpha",
            "accepted_at":"2026-09-27T00:00:00Z","ended_at":"2026-09-27T00:00:01Z",
            "summary":{"outcome":"failed","progress":{"progress":"reserved"},"notice":{"level":"error","text":"process exited"}},
            "usage":lattice_api::UsageSummary::from_attempts([]),
            "steps":[{"node":2,"key":{"turn_id":"10000000-0000-4000-8000-000000000001","ordinal":0},"progress":{"progress":"reserved"},"end":{"end":"stop","cause":{"cause":"process_exit"}},"attempts":[],"slots":[],"updated_at":"2026-09-27T00:00:01Z"}]
        })).unwrap();
        let text = render_turn(&turn);
        assert!(text.contains("step 0 node=2"), "{text}");
        assert!(text.contains("\"end\":\"stop\""), "{text}");
        assert!(text.contains("\"progress\":\"reserved\""), "{text}");
        assert!(text.contains("process_exit"), "{text}");
        assert!(text.contains("process exited"), "{text}");
    }
}
