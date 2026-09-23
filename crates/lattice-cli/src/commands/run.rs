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

#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio::time;

use lattice_api::local::{
    Acceptance, CancelStatus, ConnectionStatus, FrozenConfiguration, HistoryReadError,
    HistoryScope, LocalClient, LocalErrorKind, LocalHistory, LocalState, LocalTurn, LocalUpdate,
    NotSubmittedStage, ObservationPhase, ResetReason, SessionObservation, TextAction, TurnId,
    TurnObservation, TurnRequest,
};
use lattice_api::{LoadedAgentProfile, TurnEvent, TurnOutcome, TurnUsageSummary};

#[cfg(test)]
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
    let sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let session_id = session.session_id.clone();
    let turn = api.start_turn_with(request, HistoryScope::SelectedTurn, move || async move {
        tokio::task::spawn_blocking(move || {
            let name = session.active_agent_name;
            let loaded = load_named_profile(&name, root.canonical_path())
                .map_err(|_| lattice_api::ApiError::BadRequest("invalid profile".into()))?;
            if loaded.profile.agent.name != name {
                return Err(
                    lattice_api::ApiError::BadRequest("agent profile name differs".into()).into(),
                );
            }
            FrozenConfiguration::from_env(loaded, &root)
        })
        .await
        .map_err(|_| lattice_api::ApiError::Internal("configuration worker failed".into()))?
    });
    observe_with_signal(turn, &session_id, true, verbose, json, sigint).await
}

/// 提交结束于接受之外的状态：原身份必须可见，供用户用同一 TurnId 查询或重试，不自动重发。
fn submission_state(session_id: &str, turn_id: TurnId, state: &str, json: bool) {
    if json {
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({"type":"submission", "session_id":session_id, "turn_id":turn_id, "state":state})
        );
    } else {
        let message = match state {
            "not_sent" => "本客户端未发送后续配置：配置冻结时已中断；原 Turn 的后台状态请另行查询",
            "not_accepted" => "本客户端不再提交配置；不代表原 Turn 未被其他连接接受",
            _ => "提交结果未知：后台可能仍接受同一请求，请用同一 TurnId 查询",
        };
        let _ = writeln!(
            std::io::stderr(),
            "{message}：session {session_id} turn {turn_id}"
        );
    }
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
    let turn = api.observe(session_id.into(), Some(turn_id), HistoryScope::SelectedTurn);
    observe_with_signal(turn, session_id, false, verbose, json, sigint).await
}

/// CLI 只保留显示与退出选择；提交握手、取消交接和正文退场均由 LocalTurn 推进。
async fn observe_with_signal(
    mut turn: LocalTurn,
    session_id: &str,
    submitting: bool,
    verbose: bool,
    json: bool,
    mut sigint: tokio::signal::unix::Signal,
) -> Result<std::process::ExitCode> {
    let mut render = RunRenderState::default();
    let mut awaiting_submission = submitting;
    let mut submission_unknown = false;
    let mut interrupted = false;
    let mut closing = None;
    let mut history_ready = false;
    let mut completion_known = false;
    let mut last_state = None;
    let mut last_cancel = None;
    let mut last_cancel_error = None;
    let mut last_history_error = None;
    let mut last_submission_diagnostic = None;
    let mut not_submitted = false;
    let result = async {
        loop {
            if (!awaiting_submission || completion_known) && history_ready && let Some(exit) = closing {
                break Ok(exit);
            }
            tokio::select! {
                _ = sigint.recv() => {
                    if closing.is_some() && (!awaiting_submission || completion_known) {
                        connection_notice("transcript read interrupted; fixed result above stands; reconnect to read the transcript", json);
                        break Ok(std::process::ExitCode::from(1));
                    }
                    if interrupted { break Ok(std::process::ExitCode::from(1)); }
                    interrupted = true;
                    turn.cancel();
                }
                update = turn.next() => match update {
                    Some(LocalUpdate::Submission { result, .. }) => {
                        awaiting_submission = false;
                        submission_unknown = false;
                        let acceptance = result.map_err(submission_error)?;
                        // 接受前的显示不能结束新观察；库随后给出重固定更新。
                        closing = None;
                        history_ready = false;
                        render_acceptance(session_id, &acceptance, json)?;
                    }
                    Some(LocalUpdate::SubmissionUnknown { turn_id, error }) => {
                        awaiting_submission = false;
                        submission_unknown = true;
                        submission_state(session_id, turn_id, "unknown", json);
                        if let Some(error) = error {
                            report_error(&error.clone().into(), json);
                            last_submission_diagnostic = Some(error);
                        }
                    }
                    Some(LocalUpdate::NotSubmitted { .. }) => {
                        not_submitted = true;
                        // 只结束本客户端的后续配置；detach 仍保留另一连接的接受/接管事实。
                        break Ok(std::process::ExitCode::from(1));
                    }
                    Some(LocalUpdate::State { view, phase, confirmation, error, .. }) => {
                        let state = LocalState { view, phase, confirmation, error };
                        render_local_state(&state, json)?;
                        // 可读 pin 不结束原提交确认；只有独立完成回执可越过接受等待。
                        completion_known |= phase == ObservationPhase::Finished;
                        closing = (phase != ObservationPhase::Live).then(|| {
                            if state.error.is_some() { std::process::ExitCode::from(1) }
                            else { durable_exit(&state.view) }
                        });
                        history_ready = false;
                        last_state = Some(state);
                    }
                    Some(LocalUpdate::History { result, stale, .. }) => {
                        match result {
                            Ok(history) => render_history(*history, json)?,
                            Err(error) => {
                                render_history_error(&error, json);
                                last_history_error = Some(error);
                            }
                        }
                        if !stale { history_ready = true; }
                    }
                    Some(LocalUpdate::Reset { reason, cursor }) => match reason {
                        ResetReason::Gap => {
                            if json { writeln!(std::io::stdout(), "{}", serde_json::json!({"type":"gap", "cursor":cursor}))?; }
                            else { writeln!(std::io::stderr(), "\n观察存在缺口；以下固定结果替换临时输出，不是逐 token 重放。")?; }
                        }
                        ResetReason::Reconnect => { closing = None; history_ready = false; }
                        ResetReason::Snapshot => {}
                    },
                    Some(LocalUpdate::Event { event, text, .. }) => {
                        if text == TextAction::StartIncomplete {
                            connection_notice("当前模型段前文可能缺失；以下为新收到的文字后缀", json);
                        }
                        if json { render_json_event(&event, &mut std::io::stdout())?; }
                        else { render_human_event(&event, &mut render, &mut std::io::stdout(), &mut std::io::stderr(), verbose)?; }
                        apply_event_to_state(&event, &mut render);
                        std::io::stdout().flush()?;
                    }
                    Some(LocalUpdate::Cancel { turn_id, status, error }) => {
                        render_cancel(status, json);
                        if let Some(error) = &error { render_cancel_error(session_id, Some(turn_id), error, json); }
                        last_cancel_error = error;
                        last_cancel = Some(status);
                    }
                    Some(LocalUpdate::Connection(status)) => {
                        match status {
                            ConnectionStatus::Error(error) => { report_error(&error.into(), json); }
                            ConnectionStatus::Eof => connection_notice("observation ended; reconnect with the same TurnId", json),
                            ConnectionStatus::Idle => connection_notice("observation timed out; reconnect with the same TurnId", json),
                        }
                        // 初始观察可早于提交，接受衔接由库负责；不据此取消提交或另起订阅。
                        if !awaiting_submission { break Ok(std::process::ExitCode::from(1)); }
                    }
                    None => break Ok(std::process::ExitCode::from(1)),
                }
            }
        }
    }.await;
    let report = turn.detach().await;
    // 仅去重已显示的事实，不用 OID、游标或读取代次控制协议。
    if let Some(state) = &report.state
        && last_state.as_ref() != Some(state)
    {
        render_local_state(state, json)?;
    }
    if let Some(status) = report.cancel
        && last_cancel != Some(status)
    {
        render_cancel(status, json);
    }
    if let Some(error) = &report.cancel_error
        && last_cancel_error.as_ref() != Some(error)
    {
        render_cancel_error(&report.session_id, report.turn_id, error, json);
    }
    if let Some(stage) = report.not_submitted {
        if let Some(id) = report.turn_id {
            submission_state(
                &report.session_id,
                id,
                match stage {
                    NotSubmittedStage::NeedsConfiguration => "not_accepted",
                    NotSubmittedStage::FreezingConfiguration => "not_sent",
                },
                json,
            );
        }
    } else if (awaiting_submission || submission_unknown) && !not_submitted {
        match &report.submission {
            Some(Ok(acceptance)) => render_acceptance(&report.session_id, acceptance, json)?,
            Some(Err(error)) => {
                report_error(&submission_error(error.clone()), json);
            }
            None => {
                if !submission_unknown && let Some(id) = report.turn_id {
                    submission_state(&report.session_id, id, "unknown", json);
                }
            }
        }
    }
    if let Some(error) = report.submission_diagnostic
        && last_submission_diagnostic.as_ref() != Some(&error)
    {
        report_error(&error.into(), json);
    }
    if matches!(
        report.cancel,
        Some(CancelStatus::Pending | CancelStatus::NotTaken | CancelStatus::Unknown)
    ) && let Some(id) = report.turn_id
    {
        unconfirmed_cancel_notice(&report.session_id, id, json);
    }
    if let Some(error) = &report.history_error
        && last_history_error.as_ref() != Some(error)
    {
        render_history_error(error, json);
    }
    if verbose && !json {
        render_verbose_epilogue(&render, &mut std::io::stderr())?;
    }
    if report.history_error.is_some() || report.state.is_some_and(|state| state.error.is_some()) {
        Ok(std::process::ExitCode::from(1))
    } else {
        result
    }
}

fn render_acceptance(session_id: &str, acceptance: &Acceptance, json: bool) -> Result<()> {
    if json {
        writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({"type":"acceptance", "session_id":session_id, "result":acceptance})
        )?;
    } else {
        let message = match acceptance {
            Acceptance::Accepted { .. } => "已确认接受",
            Acceptance::Existing { .. } => "找回既有请求；未重复执行",
            Acceptance::Recovered { .. } => "旧 Turn 已恢复关闭；本次新 prompt 未接受",
            Acceptance::ConfirmationUnknown { .. } => "接受确认未知；保留同一 TurnId 查询或重试",
            Acceptance::NeedsConfiguration => "尚未接受",
        };
        writeln!(std::io::stderr(), "{message}")?;
    }
    Ok(())
}

fn render_local_state(state: &LocalState, json: bool) -> Result<()> {
    render_snapshot(&state.view, json)?;
    if let Some(error) = &state.error {
        report_error(&error.clone().into(), json);
    }
    Ok(())
}

fn render_history_error(error: &HistoryReadError, json: bool) {
    connection_notice(
        &format!(
            "fixed transcript unavailable ({:?}); fixed result stands; reconnect to read the transcript",
            error.context
        ),
        json,
    );
    report_error(
        &anyhow::Error::new(error.clone()).context(HistoryFailure),
        json,
    );
}

/// 退出时仍未确认的取消意图必须带原身份可见，供用户用同一 TurnId 查询或再次取消。
fn unconfirmed_cancel_notice(session_id: &str, turn_id: TurnId, json: bool) {
    if json {
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({"type":"cancel_unconfirmed", "session_id":session_id, "turn_id":turn_id})
        );
    } else {
        let _ = writeln!(
            std::io::stderr(),
            "取消未确认：session {session_id} turn {turn_id}；请用同一 TurnId 查询或再次取消"
        );
    }
}

fn render_cancel_error(
    session_id: &str,
    turn_id: Option<TurnId>,
    error: &lattice_api::local::LocalError,
    json: bool,
) {
    connection_notice(
        &format!(
            "取消诊断：session {session_id} turn {}",
            turn_id.map_or_else(|| "未知".into(), |id| id.to_string())
        ),
        json,
    );
    report_error(&error.clone().into(), json);
}

/// 返回后台是否已接管取消；false 与未知都不是终结，后续仍可能接受同一请求。
fn render_cancel(status: CancelStatus, json: bool) {
    match status {
        CancelStatus::Taken => {
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
        CancelStatus::NotTaken => connection_notice(
            "后台在判定点未接管本次取消；不代表没有后台工作，请查询原 Turn",
            json,
        ),
        CancelStatus::Unknown => connection_notice("cancel acknowledgement unknown", json),
        CancelStatus::Pending | CancelStatus::Moot => {}
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
    render_snapshot(view, json)?;
    let history = api
        .read_history(view, HistoryScope::SelectedTurn)
        .await
        .map_err(|error| anyhow::Error::new(error).context(HistoryFailure))?;
    render_history(history, json)
}

fn render_snapshot(view: &SessionObservation, json: bool) -> Result<()> {
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
    Ok(())
}

/// 对已交付历史选择渲染标签，不选择或读取新的固定对象。
fn render_history(history: LocalHistory, json: bool) -> Result<()> {
    for entry in history.entries {
        let (object_id, status) = match &entry.turn {
            TurnObservation::Committed(turn) => (&turn.commit_id, outcome_label(&turn.outcome)),
            TurnObservation::Pending(turn) => (&turn.journal_oid, "pending"),
        };
        render_messages(object_id, status, entry.messages, json)?;
    }
    Ok(())
}

type BodyMessages = Vec<lattice_api::local::NativeMessage<lattice_api::local::BoundToolCall>>;

#[derive(Debug)]
struct HistoryFailure;

impl std::fmt::Display for HistoryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fixed history read")
    }
}

fn render_messages(
    object_id: &str,
    status: &str,
    messages: BodyMessages,
    json: bool,
) -> Result<()> {
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
    Ok(())
}

// 只标记提交 IPC 的身份前提错误；同一分类器还服务后台启动和会话写操作。
#[derive(Debug)]
struct Submission;

impl std::fmt::Display for Submission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Turn submission")
    }
}

fn submission_error(error: lattice_api::local::LocalError) -> anyhow::Error {
    anyhow::Error::new(error).context(Submission)
}

fn error_record(error: &anyhow::Error) -> (u8, serde_json::Value) {
    let local = error
        .downcast_ref::<lattice_api::local::LocalError>()
        .cloned()
        .or_else(|| {
            error
                .downcast_ref::<HistoryReadError>()
                .map(|error| error.error.clone())
        })
        .or_else(|| {
            error
                .downcast_ref::<lattice_api::ApiError>()
                .cloned()
                .map(Into::into)
        });
    let code = match local.as_ref().map(|error| error.kind) {
        _ if error.is::<HistoryFailure>() => 1,
        Some(LocalErrorKind::BadRequest | LocalErrorKind::NotFound) => 2,
        Some(LocalErrorKind::Conflict) if error.is::<Submission>() => 2,
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

#[cfg(test)]
#[path = "run_ipc_tests.rs"]
mod ipc_tests;
