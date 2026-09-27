// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session 命令通过 LocalClient 操作后台；本层仅处理配置、参数与呈现。
//! 找回、查询和恢复先于当前 profile/provider 校验；引用写操作不在前端执行。
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

/// 全局会话库路径，仅供诊断显示；前端不打开该文件。
pub(crate) fn session_store_path() -> Result<PathBuf> {
    Ok(state_layout()?.lattice_root().join("lattice.redb"))
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
    #[arg(long = "session")]
    pub session_id: Option<String>,
    /// 新会话必须指定；已有会话在根节点时也必须指定。
    #[arg(long, required_unless_present_any = ["session_id", "turn_id"])]
    pub agent: Option<String>,
    #[arg(long, value_parser = parse_turn_id)]
    pub turn_id: Option<lattice_api::local::TurnId>,
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
    /// 提交请求；不带 --session 时在接受事务中创建会话。
    Send(SendArgs),
    /// 仅移动对话位置到节点 N；保留原分叉，不回退文件。
    Rewind {
        session_id: String,
        #[arg(value_parser = parse_node_id)]
        target: lattice_api::local::NodeId,
    },
    /// 列出全部会话。
    List,
    /// 显示含根节点的完整会话树。
    Log { session_id: String },
    /// 按 TurnId 显示全部 Step；--follow 继续观察。
    Turn {
        #[arg(value_parser = parse_turn_id)]
        turn_id: lattice_api::local::TurnId,
        #[arg(long)]
        follow: bool,
    },
    /// 取消接管不等于已持久 Cancelled；未接管不表示没有后台工作。
    Cancel {
        session_id: String,
        #[arg(value_parser = parse_turn_id)]
        turn_id: lattice_api::local::TurnId,
    },
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
            let detail = api
                .turn(lattice_api::local::TurnTarget {
                    session_id: None,
                    turn_id,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string(&detail)?);
            } else {
                print!("{}", render_turn(&detail));
            }
            if follow {
                return crate::commands::run::observe_turn(
                    &api,
                    detail.session_id,
                    turn_id,
                    verbose,
                    json,
                )
                .await;
            }
        }
        SessionAction::Cancel {
            session_id,
            turn_id,
        } => {
            let session_id = api.resolve_session(&session_id).await?.session_id;
            let owned = api
                .cancel(lattice_api::local::TurnTarget {
                    session_id: Some(session_id),
                    turn_id,
                })
                .await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"type":if owned {"cancel_acknowledged"} else {"cancel_not_owned"}, "session_id":session_id, "turn_id":turn_id, "owned":owned})
                );
            } else {
                println!(
                    "session {session_id} turn {turn_id}: {}",
                    if owned {
                        "后台已接管取消；持久终态仍待查询"
                    } else {
                        "后台在判定点未接管；不表示没有后台工作"
                    }
                );
            }
        }
        SessionAction::List => {
            let rows = api.list_sessions().await?;
            if json {
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                for row in rows {
                    println!(
                        "{} project={} current={} nodes={} active={} updated={}",
                        display_id(&row.session_id.to_string()),
                        row.project_store_key,
                        row.current.0,
                        row.node_count,
                        row.active_turn
                            .map_or_else(|| "none".into(), |id| id.to_string()),
                        row.updated_at
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
        SessionAction::Rewind { session_id, target } => {
            let id = api.resolve_session(&session_id).await?.session_id;
            let root =
                tokio::task::spawn_blocking(lattice_api::resolve_local_workspace_root).await??;
            let session = api.rewind(id, target, root.canonical_path().into()).await?;
            if json {
                println!("{}", serde_json::to_string(&session)?);
            } else {
                println!(
                    "session {} current={}; 文件未回退",
                    session.session_id, session.current.0
                );
            }
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn render_log(log: &lattice_api::local::SessionLog) -> String {
    use std::fmt::Write;
    let mut text = format!(
        "session {} current={} agent={} active={}\n",
        log.session.session_id,
        log.session.current.0,
        summarize(log.current_agent.as_deref().unwrap_or("none"), 120),
        log.session
            .active_turn
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
                turn.summary.as_ref().map_or_else(
                    || "running".into(),
                    |s| serde_json::to_string(s).expect("summary")
                )
            );
        }
        if let Some(rows) = children.get(&Some(row.node.id)) {
            stack.extend(rows.iter().rev().map(|child| (*child, depth + 1)));
        }
    }
    text
}

fn render_turn(turn: &lattice_api::local::TurnDetail) -> String {
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
            "step {}: {}",
            step.key.ordinal,
            serde_json::to_string(&step.state).expect("step")
        );
        for attempt in &step.attempts {
            let _ = writeln!(
                text,
                "  attempt {}: {}",
                attempt.attempt,
                serde_json::to_string(attempt).expect("attempt")
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
    fn session_surface_and_numeric_nodes() {
        let command = TestCli::command();
        assert_eq!(
            command
                .get_subcommands()
                .map(|c| c.get_name())
                .collect::<Vec<_>>(),
            ["send", "rewind", "list", "log", "turn", "cancel"]
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
        assert!(TestCli::try_parse_from(["session", "cancel", "abcd", id]).is_ok());
        assert!(TestCli::try_parse_from(["session", "cancel", "abcd", "invalid"]).is_err());
        let mut command = TestCli::command();
        let help = command
            .find_subcommand_mut("cancel")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("不表示没有后台工作"));
        assert!(help.contains("不等于已持久 Cancelled"));
    }
    #[test]
    fn tree_output_keeps_root_branches_current_and_notice() {
        let log: lattice_api::local::SessionLog = serde_json::from_value(serde_json::json!({
            "session": {"session_id":"11111111111111111111111111111111", "project_store_key":"a".repeat(64), "current":3, "active_turn":null, "created_at":"2026-09-27T00:00:00Z", "updated_at":"2026-09-27T00:00:00Z"},
            "current_agent":"beta",
            "nodes":[
                {"node":{"id":0,"parent":null,"turn_id":null,"kind":{"kind":"root"}},"current":false,"turn_start":null},
                {"node":{"id":1,"parent":0,"turn_id":"10000000-0000-4000-8000-000000000001","kind":{"kind":"user_message"}},"current":false,"turn_start":{"turn_id":"10000000-0000-4000-8000-000000000001","agent_name":"alpha","summary":{"outcome":"cancelled","notice":{"level":"info","text":"cancelled notice"}}}},
                {"node":{"id":2,"parent":1,"turn_id":"10000000-0000-4000-8000-000000000001","kind":{"kind":"step","ordinal":0}},"current":false,"turn_start":null},
                {"node":{"id":3,"parent":0,"turn_id":"20000000-0000-4000-8000-000000000002","kind":{"kind":"user_message"}},"current":true,"turn_start":{"turn_id":"20000000-0000-4000-8000-000000000002","agent_name":"beta","summary":null}}
            ]
        })).unwrap();
        let text = render_log(&log);
        assert!(text.contains("current=3 agent=beta"));
        assert!(text.contains("  0 root"));
        assert!(text.contains("    1 user"));
        assert!(text.contains("      2 step 0"));
        assert!(text.contains("  * 3 user"));
        assert!(text.contains("cancelled notice"));
        assert!(text.contains("alpha"));
    }
    #[test]
    fn turn_output_keeps_stop_without_node_or_attempts() {
        let turn: lattice_api::local::TurnDetail = serde_json::from_value(serde_json::json!({
            "session_id":"11111111111141118111111111111111",
            "turn_id":"10000000-0000-4000-8000-000000000001",
            "user_node":1,"agent_name":"alpha",
            "accepted_at":"2026-09-27T00:00:00Z","ended_at":"2026-09-27T00:00:01Z",
            "summary":{"outcome":"failed","notice":{"level":"error","text":"process exited"}},
            "usage":{"kind":"complete","total":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}},
            "steps":[{"key":{"turn_id":"10000000-0000-4000-8000-000000000001","ordinal":0},"attempts":[],"state":{"state":"closed","end":{"end":"stop","node":null,"cause":{"cause":"process_exit"}},"slots":[]}}]
        })).unwrap();
        let text = render_turn(&turn);
        assert!(text.contains("step 0:"), "{text}");
        assert!(text.contains("\"end\":\"stop\""), "{text}");
        assert!(text.contains("\"node\":null"), "{text}");
        assert!(text.contains("process_exit"), "{text}");
        assert!(text.contains("process exited"), "{text}");
    }
    #[test]
    fn summaries_and_ids_respect_character_boundaries() {
        assert_eq!(display_id("abc"), "abc");
        assert_eq!(display_id("abcdefghijklmnop"), "abcdefgh..");
        assert_eq!(summarize("hello world", 120), "hello world");
        assert_eq!(summarize("你好世界测试", 120), "你好世界测试");
        for (text, max) in [
            ("a".repeat(200), 50),
            ("你好世界测试中文显示宽度".into(), 7),
            ("hello世界".into(), 5),
        ] {
            let value = summarize(&text, max);
            assert!(value.width() <= max);
            assert!(value.ends_with('…'));
        }
    }
}
