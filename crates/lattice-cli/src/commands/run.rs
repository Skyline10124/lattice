// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CLI 通过 LocalClient 提交、找回和观察 Turn；终态退出以固定持久视图为准。
//! JSONL 保留公开 TurnEvent，并增加 submission/acceptance/snapshot/gap 等控制记录。
//! 观察断连、超时或 EOF 不取消任务，也不宣称 Turn 已失败。
//! 退出码：0 为成功结果或成功查询，1 为执行/连接/持久失败，2 为输入或配置错误。
use anyhow::Result;
use colored::Colorize;
use std::io::Write;
use std::path::{Path, PathBuf};

use std::time::Duration;
use tokio::time;

use lattice_api::local::{
    Acceptance, FrozenConfiguration, LocalClient, LocalErrorKind, Observation, SessionObservation,
    TurnId, TurnObservation, TurnRequest,
};
use lattice_api::{LoadedAgentProfile, TurnEvent, TurnOutcome, TurnUsageSummary};

pub(crate) use lattice_api::STREAM_IDLE_TIMEOUT_SECS as STREAM_TIMEOUT_SECS;
#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalSignal {
    Failed { reason: String },
    Cancelled { reason: Option<String> },
}

/// Commit signal accumulated from `TurnCommitted` or `CommitFailed`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CommitSignal {
    Committed,
    Failed { reason: String },
}

/// Accumulated state from a live `TurnEvent` stream, used to derive
/// the CLI exit code after the stream drains.
///
/// `usage_total` is summed across multiple `ModelFinished` events;
/// `TurnCommitted` is NOT a usage source.
#[derive(Debug, Default)]
struct RunRenderState {
    last_terminal: Option<TerminalSignal>,
    last_commit: Option<CommitSignal>,
    last_outcome: Option<TurnOutcome>,
    tokens_emitted_without_newline: bool,
    mid_token_stderr_newline_applied: bool,
    session_id: Option<String>,
    commit_id: Option<String>,
    branch: Option<String>,
    usage_total: Option<TurnUsageSummary>,
    tool_finished_count: usize,
}

/// Update `RunRenderState` from each `TurnEvent` variant.
fn apply_event_to_state(event: &TurnEvent, state: &mut RunRenderState) {
    match event {
        TurnEvent::TurnStarted {
            session_id, branch, ..
        } => {
            state.session_id = Some(session_id.clone());
            state.branch = branch.clone();
        }
        TurnEvent::ModelFinished { usage: Some(u), .. } => {
            if let Some(existing) = &mut state.usage_total {
                existing.prompt_tokens += u.prompt_tokens;
                existing.completion_tokens += u.completion_tokens;
                existing.total_tokens += u.total_tokens;
            } else {
                state.usage_total = Some(u.clone());
            }
        }
        TurnEvent::TokenDelta { content } if !content.is_empty() => {
            state.tokens_emitted_without_newline = !content.ends_with('\n');
            if !state.tokens_emitted_without_newline {
                state.mid_token_stderr_newline_applied = false;
            }
        }
        TurnEvent::ToolCallFinished { .. } => {
            state.tool_finished_count += 1;
        }
        TurnEvent::TurnFailed { reason } => {
            state.last_terminal = Some(TerminalSignal::Failed {
                reason: reason.clone(),
            });
        }
        TurnEvent::TurnCancelled { reason } => {
            state.last_terminal = Some(TerminalSignal::Cancelled {
                reason: reason.clone(),
            });
        }
        TurnEvent::TurnCommitting => {}
        TurnEvent::TurnCommitted {
            session_id,
            commit_id,
            outcome,
            ..
        } => {
            state.last_commit = Some(CommitSignal::Committed);
            state.session_id = Some(session_id.clone());
            state.commit_id = Some(commit_id.clone());
            state.last_outcome = Some(outcome.clone());
        }
        TurnEvent::CommitFailed { reason } => {
            state.last_commit = Some(CommitSignal::Failed {
                reason: reason.clone(),
            });
        }
        TurnEvent::EvidenceDurabilityFailed { attempts } => {
            state.last_commit = Some(CommitSignal::Failed {
                reason: format!("terminal evidence durability failed after {attempts} attempt(s)"),
            });
        }
        TurnEvent::RecoveryRequired { reason } => {
            state.last_commit = Some(CommitSignal::Failed {
                reason: format!("recovery required: {reason}"),
            });
        }
        // All other variants (ModelStarted, ReasoningDelta,
        // ToolCallStarted, ModelFinished without usage): no state mutation.
        _ => {}
    }
}

/// Derive exit code from accumulated stream signals (rows 1-7).
///
/// Row 8 (ApiError::NotFound -> exit 2) and Row 9 (ApiError::Internal -> exit 1)
/// are handled in `run_inner` before the stream starts.
fn outcome_label(outcome: &TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Completed { .. } => "completed",
        TurnOutcome::Failed { .. } => "failed",
        TurnOutcome::Interrupted { .. } => "interrupted",
        TurnOutcome::Cancelled { .. } => "cancelled",
    }
}

fn exit_code_from_state(state: &RunRenderState) -> std::process::ExitCode {
    // Row 4: CommitFailed -> 1 (regardless of last_outcome)
    if matches!(state.last_commit, Some(CommitSignal::Failed { .. })) {
        return std::process::ExitCode::from(1);
    }
    // Defensive: if TurnFailed/TurnCancelled was seen, exit 1 regardless of commit status
    if state.last_terminal.is_some() {
        return std::process::ExitCode::from(1);
    }
    // Row 1: TurnCommitted{Completed} -> 0
    if matches!(state.last_outcome, Some(TurnOutcome::Completed { .. })) {
        return std::process::ExitCode::SUCCESS;
    }
    // Row 2/3: TurnCommitted{Error|Cancelled} -> 1
    if matches!(
        state.last_outcome,
        Some(
            TurnOutcome::Failed { .. }
                | TurnOutcome::Interrupted { .. }
                | TurnOutcome::Cancelled { .. }
        )
    ) {
        return std::process::ExitCode::from(1);
    }
    // Row 7: Stream ended without any terminal/commit signal -> 1
    std::process::ExitCode::from(1)
}

/// Render a `TurnEvent` as one JSONL line on `out`.
///
/// Each call emits exactly one `serde_json::to_string` line followed
/// by `\n` via `writeln!`. No ANSI escapes are produced.
fn render_json_event<W: std::io::Write>(event: &TurnEvent, out: &mut W) -> std::io::Result<()> {
    let line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    writeln!(out, "{}", line)
}

/// Render a `TurnEvent` for human consumption.
///
/// `TokenDelta` content goes to `out`; all status/lifecycle lines go to `err`.
/// Before writing any status line to `err`, if `state.tokens_emitted_without_newline`
/// is true, the mid-token newline trick is applied: flush `out`, emit `\n` on `err`.
/// This function does not mutate `state` — `apply_event_to_state` owns that.
fn render_human_event<WOut: Write, WErr: Write>(
    event: &TurnEvent,
    state: &mut RunRenderState,
    out: &mut WOut,
    err: &mut WErr,
    verbose: bool,
) -> std::io::Result<()> {
    match event {
        TurnEvent::TokenDelta { content } => {
            write!(out, "{}", content)?;
        }
        TurnEvent::TurnStarted {
            session_id, branch, ..
        } => {
            if verbose {
                flush_before_status(out, err, state)?;
                if let Some(b) = branch {
                    writeln!(err, "{} turn {} on {}", "·".dimmed(), session_id, b)?;
                } else {
                    writeln!(err, "{} session {}", "·".dimmed(), session_id)?;
                }
            }
        }
        TurnEvent::ModelStarted { provider, model } => {
            if verbose {
                flush_before_status(out, err, state)?;
                if let Some(p) = provider {
                    writeln!(err, "{} model {}/{model}", "·".dimmed(), p)?;
                } else {
                    writeln!(err, "{} model {model}", "·".dimmed())?;
                }
            }
        }
        TurnEvent::LlmRequestError {
            attempt,
            max_attempts,
            error_kind,
            will_retry,
            delay_ms,
            message,
            ..
        } => {
            if verbose {
                flush_before_status(out, err, state)?;
                if *will_retry {
                    let delay = delay_ms.unwrap_or(0);
                    writeln!(
                        err,
                        "{} llm retry {attempt}/{max_attempts} after {delay}ms ({error_kind}): {message}",
                        "·".dimmed()
                    )?;
                } else {
                    writeln!(
                        err,
                        "{} llm failed {attempt}/{max_attempts} ({error_kind}): {message}",
                        "✗".red()
                    )?;
                }
            }
        }
        TurnEvent::ReasoningDelta { content } => {
            if verbose {
                flush_before_status(out, err, state)?;
                writeln!(err, "reasoning: {}", content.dimmed())?;
            }
        }
        TurnEvent::ModelFinished { finish_reason, .. } => {
            if verbose {
                flush_before_status(out, err, state)?;
                writeln!(err, "{} model finished {finish_reason}", "✓".green())?;
            }
        }
        TurnEvent::ToolCallStarted { name, .. } => {
            flush_before_status(out, err, state)?;
            writeln!(err, "{} tool {name}", "·".dimmed())?;
        }
        TurnEvent::ToolCallFinished {
            name,
            ok,
            output_len,
            error_summary,
            ..
        } => {
            flush_before_status(out, err, state)?;
            if *ok {
                writeln!(err, "{} tool {name} {output_len} chars", "✓".green())?;
            } else {
                let summary = error_summary.as_deref().unwrap_or("error");
                writeln!(
                    err,
                    "{} tool {name} {output_len} chars: {summary}",
                    "✗".red()
                )?;
            }
        }
        TurnEvent::TurnFailed { reason } => {
            flush_before_status(out, err, state)?;
            writeln!(err, "{} failed: {reason}", "✗".red())?;
        }
        TurnEvent::TurnCancelled { reason } => {
            flush_before_status(out, err, state)?;
            if let Some(r) = reason {
                writeln!(err, "{} cancelled: {r}", "✗".red())?;
            } else {
                writeln!(err, "{} cancelled", "✗".red())?;
            }
        }
        TurnEvent::TurnCommitting => {
            flush_before_status(out, err, state)?;
            writeln!(err, "{} committing", "·".dimmed())?;
        }
        TurnEvent::TurnCommitted {
            commit_id, outcome, ..
        } => {
            flush_before_status(out, err, state)?;
            let outcome_str = outcome_label(outcome);
            writeln!(err, "{} committed {commit_id} {outcome_str}", "✓".green())?;
        }
        TurnEvent::CommitFailed { reason } => {
            flush_before_status(out, err, state)?;
            writeln!(err, "{} commit failed: {reason}", "✗".red())?;
        }
        TurnEvent::EvidenceDurabilityFailed { attempts } => {
            flush_before_status(out, err, state)?;
            writeln!(
                err,
                "{} terminal evidence durability failed after {attempts} attempt(s)",
                "✗".red()
            )?;
        }
        TurnEvent::RecoveryRequired { reason } => {
            flush_before_status(out, err, state)?;
            writeln!(err, "{} recovery required: {reason}", "✗".red())?;
        }
        TurnEvent::CepCallStarted { method, .. } => {
            if verbose {
                flush_before_status(out, err, state)?;
                writeln!(err, "{} cep {method}", "·".dimmed())?;
            }
        }
        TurnEvent::CepCallFinished { method, ok, .. } => {
            if verbose {
                flush_before_status(out, err, state)?;
                if *ok {
                    writeln!(err, "{} cep {method}", "✓".green())?;
                } else {
                    writeln!(err, "{} cep {method}", "✗".red())?;
                }
            }
        }
    }
    Ok(())
}

/// Apply mid-token newline trick: if stdout has emitted tokens
/// without a trailing newline, flush stdout and emit `\n` on stderr so
/// the status line begins on its own row in mixed-stream terminals.
fn flush_before_status<WOut: Write, WErr: Write>(
    out: &mut WOut,
    err: &mut WErr,
    state: &mut RunRenderState,
) -> std::io::Result<()> {
    if state.tokens_emitted_without_newline && !state.mid_token_stderr_newline_applied {
        out.flush()?;
        writeln!(err)?;
        state.mid_token_stderr_newline_applied = true;
    }
    Ok(())
}

/// Render the verbose epilogue after the stream drains.
///
/// Emits dimmed labels for populated state fields on stderr only:
/// session, commit, status, usage (from RunRenderState.usage_total, NOT TurnCommitted),
/// tools (from tool_finished_count).
fn render_verbose_epilogue<WErr: Write>(
    state: &RunRenderState,
    err: &mut WErr,
) -> std::io::Result<()> {
    if let Some(ref session_id) = state.session_id {
        writeln!(err, "{}: {}", "session".dimmed(), session_id)?;
    }
    if let Some(ref commit_id) = state.commit_id {
        writeln!(err, "{}: {}", "commit".dimmed(), commit_id)?;
    }
    if let Some(ref outcome) = state.last_outcome {
        writeln!(err, "{}: {}", "outcome".dimmed(), outcome_label(outcome))?;
    }
    if let Some(ref usage) = state.usage_total {
        writeln!(
            err,
            "{}: {} tok (prompt: {}, completion: {})",
            "usage".dimmed(),
            usage.total_tokens,
            usage.prompt_tokens,
            usage.completion_tokens
        )?;
    }
    if state.tool_finished_count > 0 {
        writeln!(
            err,
            "{}: {} executed",
            "tools".dimmed(),
            state.tool_finished_count
        )?;
    }
    Ok(())
}

/// 客户端先保留身份，再找回旧请求；只有确需新接受时加载配置。
pub async fn run_session_prompt(
    api: &LocalClient,
    prompt: String,
    session_id: String,
    turn_id: Option<TurnId>,
    verbose: bool,
    json: bool,
) -> Result<std::process::ExitCode> {
    let session = api.open_session(&session_id).await?;
    let turn_id = turn_id.unwrap_or_else(lattice_api::local::new_turn_id);
    if json {
        println!(
            "{}",
            serde_json::json!({"type":"submission", "session_id":session.session_id, "turn_id":turn_id})
        );
    } else {
        eprintln!("session {} turn {}", session.session_id, turn_id);
    }
    std::io::stdout().flush()?;
    std::io::stderr().flush()?;
    let root = tokio::task::spawn_blocking(lattice_api::resolve_local_workspace_root).await??;
    let request = TurnRequest {
        session_id: session.session_id.clone(),
        turn_id,
        workspace: root.canonical_path().into(),
        input: prompt,
    };
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let acceptance = {
        let submit = async {
            let prior = api.lookup_or_recover(request.clone()).await?;
            if prior != Acceptance::NeedsConfiguration {
                return Ok::<_, anyhow::Error>(prior);
            }
            let name = session.active_agent_name.clone();
            let configuration = tokio::task::spawn_blocking(move || {
                let loaded = load_named_profile(&name, root.canonical_path())
                    .map_err(|_| lattice_api::ApiError::BadRequest("invalid profile".into()))?;
                if loaded.profile.agent.name != name {
                    return Err(lattice_api::ApiError::BadRequest(
                        "agent profile name differs".into(),
                    )
                    .into());
                }
                FrozenConfiguration::from_env(loaded, &root)
            })
            .await??;
            Ok(api.submit(request, Some(configuration)).await?)
        };
        tokio::pin!(submit);
        tokio::select! {
            result = &mut submit => Some(result?),
            _ = sigint.recv() => None,
        }
    };
    let Some(acceptance) = acceptance else {
        // 可能已接管但回执未读；保留 ID，只请求取消和查询，不另分配请求。
        match time::timeout(
            Duration::from_secs(5),
            api.cancel(&session.session_id, turn_id),
        )
        .await
        {
            Ok(receipt) => render_cancel(Ok(receipt), json),
            Err(_) => connection_notice("cancel acknowledgement unknown", json),
        }
        return observe_with_signal(
            api,
            &session.session_id,
            turn_id,
            verbose,
            json,
            sigint,
            true,
        )
        .await;
    };
    if json {
        println!(
            "{}",
            serde_json::json!({"type":"acceptance", "session_id":session.session_id, "result":acceptance})
        );
    }
    if !json {
        let message = match &acceptance {
            Acceptance::Accepted { .. } => "已确认接受",
            Acceptance::Existing { .. } => "找回既有请求；未重复执行",
            Acceptance::Recovered { .. } => "旧 Turn 已恢复关闭；本次新 prompt 未接受",
            Acceptance::ConfirmationUnknown { .. } => "接受确认未知；保留同一 TurnId 查询或重试",
            Acceptance::NeedsConfiguration => "尚未接受",
        };
        eprintln!("{message}");
    }
    let selected = match acceptance {
        Acceptance::Accepted { turn_id } | Acceptance::ConfirmationUnknown { turn_id } => turn_id,
        Acceptance::Existing {
            turn: TurnObservation::Pending(turn),
        } => turn.turn_id,
        Acceptance::Existing {
            turn: TurnObservation::Committed(turn),
        }
        | Acceptance::Recovered { turn } => turn.turn_id,
        Acceptance::NeedsConfiguration => {
            anyhow::bail!("backend did not accept frozen configuration")
        }
    };
    observe_with_signal(
        api,
        &session.session_id,
        selected,
        verbose,
        json,
        sigint,
        false,
    )
    .await
}

/// 纯观察路径不加载 profile、不恢复；流结束和 idle 仅改变连接状态。
pub async fn observe_turn(
    api: &LocalClient,
    session_id: &str,
    turn_id: TurnId,
    follow: bool,
    verbose: bool,
    json: bool,
) -> Result<std::process::ExitCode> {
    if !follow {
        let view = api.query(session_id, Some(turn_id)).await?;
        render_view(api, &view, json).await?;
        return Ok(std::process::ExitCode::SUCCESS);
    }
    let sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    observe_with_signal(api, session_id, turn_id, verbose, json, sigint, false).await
}

async fn observe_with_signal(
    api: &LocalClient,
    session_id: &str,
    turn_id: TurnId,
    verbose: bool,
    json: bool,
    mut sigint: tokio::signal::unix::Signal,
    mut cancel_requested: bool,
) -> Result<std::process::ExitCode> {
    let mut stream = api.subscribe(session_id, turn_id, None).await?;
    let mut state = RunRenderState::default();
    let mut cancel: Option<tokio::task::JoinHandle<lattice_api::local::Result<bool>>> = None;
    let mut deadline = time::Instant::now() + Duration::from_secs(STREAM_TIMEOUT_SECS);
    let result = async {
    let exit = loop {
        tokio::select! {
            _ = sigint.recv() => {
                if cancel_requested {
                    break std::process::ExitCode::from(1);
                }
                cancel_requested = true;
                let client = api.clone();
                let id = session_id.to_owned();
                cancel = Some(tokio::spawn(async move { client.cancel(&id, turn_id).await }));
            }
            receipt = async { cancel.as_mut().expect("guarded cancel").await }, if cancel.is_some() => {
                cancel = None;
                render_cancel(receipt, json);
            }
            next = stream.next() => {
                deadline = time::Instant::now() + Duration::from_secs(STREAM_TIMEOUT_SECS);
                let observation = match next {
                    Ok(Some(observation)) => observation,
                    Ok(None) => { connection_notice("observation ended; execution state unknown", json); break std::process::ExitCode::from(1); }
                    Err(error) => { report_error(&error.into(), json); break std::process::ExitCode::from(1); }
                };
                match observation {
                    Observation::Event { event, .. } => {
                        if json { render_json_event(&event, &mut std::io::stdout())?; }
                        else { render_human_event(&event, &mut state, &mut std::io::stdout(), &mut std::io::stderr(), verbose)?; }
                        apply_event_to_state(&event, &mut state);
                        std::io::stdout().flush()?;
                    }
                    Observation::Gap { cursor } => {
                        if json { writeln!(std::io::stdout(), "{}", serde_json::json!({"type":"gap", "cursor":cursor}))?; }
                        else { writeln!(std::io::stderr(), "\n观察存在缺口；以下固定结果替换临时输出，不是逐 token 重放。")?; }
                    }
                    Observation::Snapshot { view, owned, .. } => {
                        render_view(api, &view, json).await?;
                        if !owned { break durable_exit(&view); }
                    }
                    Observation::Finished { view, error, .. } => {
                        let read_result = render_view(api, &view, json).await;
                        if let Some(error) = error {
                            report_error(&error.into(), json);
                            if let Err(read_error) = read_result { report_error(&read_error, json); }
                            break std::process::ExitCode::from(1);
                        }
                        if let Err(read_error) = read_result {
                            // 完成回执只确认执行收尾；正文未取到仍是命令失败，不抹去上面的固定终态。
                            connection_notice("completion received; fixed transcript unavailable; reconnect to read the result", json);
                            report_error(&read_error, json);
                            break std::process::ExitCode::from(1);
                        }
                        break durable_exit(&view);
                    }
                }
            }
            _ = time::sleep_until(deadline) => {
                connection_notice("observation timed out; execution state unknown; reconnect with the same TurnId", json);
                break std::process::ExitCode::from(1);
            }
        }
    };
    Ok::<_, anyhow::Error>(exit)
    }.await;
    // 第二次 Ctrl+C 只结束观察；已开始的取消发送仍等确认，未知不能伪报成功。
    if let Some(task) = cancel {
        match time::timeout(Duration::from_secs(5), task).await {
            Ok(receipt) => render_cancel(receipt, json),
            Err(_) => connection_notice("cancel acknowledgement unknown", json),
        }
    }
    if verbose && !json {
        render_verbose_epilogue(&state, &mut std::io::stderr())?;
    }
    result
}

fn render_cancel(
    receipt: Result<lattice_api::local::Result<bool>, tokio::task::JoinError>,
    json: bool,
) {
    match receipt {
        Ok(Ok(true)) => {
            if json {
                let _ = writeln!(
                    std::io::stdout(),
                    "{}",
                    serde_json::json!({"type":"cancel_acknowledged", "owned":true})
                );
            } else {
                let _ = writeln!(std::io::stderr(), "后台已接管取消；持久终态仍待查询");
            }
        }
        Ok(Ok(false)) => connection_notice(
            "Turn has no live owner; cancellation not confirmed; query or recover",
            json,
        ),
        _ => connection_notice("cancel acknowledgement unknown", json),
    }
}

fn connection_notice(message: &str, json: bool) {
    if json {
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({"type":"connection", "message":message})
        );
    } else {
        let _ = writeln!(std::io::stderr(), "{message}");
    }
}

fn durable_exit(view: &SessionObservation) -> std::process::ExitCode {
    let mut state = RunRenderState::default();
    if let Some(TurnObservation::Committed(turn)) = &view.requested_turn {
        state.last_commit = Some(CommitSignal::Committed);
        state.last_outcome = Some(turn.outcome.clone());
    }
    exit_code_from_state(&state)
}

pub async fn render_view(api: &LocalClient, view: &SessionObservation, json: bool) -> Result<()> {
    if json {
        writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({"type":"snapshot", "view":view})
        )?;
    } else {
        writeln!(
            std::io::stderr(),
            "session {} active={}",
            view.session.session_id,
            view.active_turn
                .as_ref()
                .map(|turn| turn.turn_id.to_string())
                .unwrap_or_else(|| "none".into())
        )?;
    }
    let selected = match &view.requested_turn {
        Some(TurnObservation::Committed(turn)) => {
            Some((turn.commit_id.as_str(), outcome_label(&turn.outcome)))
        }
        Some(TurnObservation::Pending(turn)) => Some((turn.journal_oid.as_str(), "pending")),
        None => view
            .active_turn
            .as_ref()
            .map(|turn| (turn.journal_oid.as_str(), "pending")),
    };
    if let Some((object_id, status)) = selected {
        let messages = api
            .read_messages(&view.session.session_id, object_id)
            .await?;
        if json {
            for message in messages {
                writeln!(
                    std::io::stdout(),
                    "{}",
                    serde_json::json!({"type":"transcript", "object_id":object_id, "state":status, "message":message})
                )?;
            }
        } else {
            writeln!(
                std::io::stdout(),
                "\n--- 固定可见正文 {object_id} ({status})；持久确认以完成回执为准 ---"
            )?;
            for message in messages {
                for part in message.parts {
                    use lattice_api::local::MessageSemantic;
                    match part.semantic {
                        MessageSemantic::Text { text } => writeln!(std::io::stdout(), "{text}")?,
                        MessageSemantic::ToolResult {
                            name,
                            outcome,
                            content,
                            ..
                        } => {
                            writeln!(
                                std::io::stdout(),
                                "{name} · {}\n{}",
                                outcome.as_str(),
                                content.as_deref().unwrap_or("结果材料不可用")
                            )?;
                        }
                        MessageSemantic::Reasoning { .. } | MessageSemantic::ToolCall { .. } => {}
                    }
                }
            }
        }
    }
    Ok(())
}

fn error_record(error: &anyhow::Error) -> (u8, serde_json::Value) {
    let local = error
        .downcast_ref::<lattice_api::local::LocalError>()
        .cloned()
        .or_else(|| {
            error
                .downcast_ref::<lattice_api::ApiError>()
                .cloned()
                .map(Into::into)
        });
    let code = match local.as_ref().map(|error| error.kind) {
        Some(LocalErrorKind::BadRequest | LocalErrorKind::NotFound) => 2,
        _ => 1,
    };
    let value = match local {
        Some(error) => {
            serde_json::json!({"type":"error", "kind":error.kind, "message":error.message, "recovery":error.recovery})
        }
        None => {
            serde_json::json!({"type":"error", "kind":"internal", "message":"command failed; inspect configuration and durable state"})
        }
    };
    (code, value)
}

pub fn report_error(error: &anyhow::Error, json: bool) -> std::process::ExitCode {
    let (code, value) = error_record(error);
    if json {
        let _ = writeln!(std::io::stdout(), "{value}");
    } else {
        let _ = writeln!(
            std::io::stderr(),
            "error: {}",
            value["message"].as_str().unwrap_or("command failed")
        );
    }
    std::process::ExitCode::from(code)
}

pub(crate) fn load_named_profile(
    agent_name: &str,
    project_root: &Path,
) -> std::result::Result<LoadedAgentProfile, anyhow::Error> {
    let global_agents_dir = safe_agents_dir(None).map_err(lattice_api::ApiError::BadRequest)?;
    lattice_api::resolve_named_agent_profile(agent_name, project_root, &global_agents_dir)
        .map_err(Into::into)
}

pub(crate) fn agents_dir(override_path: Option<&str>) -> PathBuf {
    if let Some(path) = override_path {
        PathBuf::from(path)
    } else if let Ok(dir) = std::env::var("LATTICE_AGENTS_DIR") {
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".lattice/agents")
    } else {
        PathBuf::from(".lattice/agents")
    }
}

pub(crate) fn safe_agents_dir(override_path: Option<&str>) -> Result<PathBuf, String> {
    let path = agents_dir(override_path);
    validate_agents_dir(&path)?;
    Ok(path)
}

pub(crate) use lattice_api::validate_agents_dir;

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
