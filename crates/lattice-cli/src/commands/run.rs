// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! CLI 只提交意图和呈现事实；完整 History 后的 Finished 才结束观察交付。
use anyhow::Result;
use lattice_api::local::{
    Acceptance, CancelStatus, ConnectionStatus, FrozenConfiguration, HistoryReadError, LocalClient,
    LocalErrorKind, LocalHistory, LocalState, LocalTurn, LocalUpdate, MaterialSummary,
    MessageSemantic, Node, NotSubmittedStage, NoticeLevel, ObservationPhase, ObservationSnapshot,
    PublicTurnOutcome, SessionId, TurnId, TurnRequest, TurnSummary, UsageSummary,
};
use lattice_api::{LoadedAgentProfile, TurnEvent};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio::time;

#[derive(Default)]
struct RunRenderState {
    last_summary: Option<TurnSummary>,
    printed_summaries: Vec<TurnId>,
    session_id: Option<SessionId>,
    usage: Option<UsageSummary>,
    tokens_emitted_without_newline: bool,
    mid_token_stderr_newline_applied: bool,
    tool_finished_count: usize,
}
fn apply_event_to_state(event: &TurnEvent, state: &mut RunRenderState) {
    match event {
        TurnEvent::TurnStarted { turn, .. } => state.session_id = Some(turn.session_id),
        TurnEvent::TurnCompleted { turn, summary, .. }
        | TurnEvent::TurnFailed { turn, summary, .. }
        | TurnEvent::TurnCancelled { turn, summary, .. } => {
            state.session_id = Some(turn.session_id);
            state.last_summary = Some(summary.clone());
        }
        TurnEvent::TokenDelta { content, .. } if !content.is_empty() => {
            state.tokens_emitted_without_newline = !content.ends_with('\n');
            if !state.tokens_emitted_without_newline {
                state.mid_token_stderr_newline_applied = false;
            }
        }
        TurnEvent::ToolCallFinished { .. } => state.tool_finished_count += 1,
        _ => {}
    }
}
fn outcome_label(outcome: &PublicTurnOutcome) -> &'static str {
    match outcome {
        PublicTurnOutcome::Completed => "completed",
        PublicTurnOutcome::Failed => "failed",
        PublicTurnOutcome::Cancelled => "cancelled",
    }
}
fn render_json_event(event: &TurnEvent, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(
        out,
        "{}",
        serde_json::to_string(event).map_err(std::io::Error::other)?
    )
}
fn flush_before_status(
    out: &mut impl Write,
    err: &mut impl Write,
    state: &mut RunRenderState,
) -> std::io::Result<()> {
    if state.tokens_emitted_without_newline && !state.mid_token_stderr_newline_applied {
        out.flush()?;
        writeln!(err)?;
        state.mid_token_stderr_newline_applied = true;
    }
    Ok(())
}
fn finish_text(state: &mut RunRenderState, out: &mut impl Write) -> std::io::Result<()> {
    if state.tokens_emitted_without_newline {
        writeln!(out)?;
        state.tokens_emitted_without_newline = false;
    }
    Ok(())
}
fn render_human_event(
    event: &TurnEvent,
    state: &mut RunRenderState,
    out: &mut impl Write,
    err: &mut impl Write,
    verbose: bool,
) -> std::io::Result<()> {
    if let TurnEvent::TokenDelta { content, .. } = event {
        return write!(out, "{content}");
    }
    let line = match event {
        TurnEvent::TurnStarted { turn, .. } => {
            verbose.then(|| format!("session {} turn {}", turn.session_id, turn.turn_id))
        }
        TurnEvent::ModelStarted {
            provider, model, ..
        } => verbose.then(|| format!("model {provider}/{model}")),
        TurnEvent::LlmRetry {
            key,
            delay_ms,
            notice,
        } => verbose.then(|| {
            format!(
                "{}: llm attempt {} failed, retrying in {delay_ms}ms: {}",
                notice_level_label(&notice.level),
                key.attempt_ordinal,
                notice.text
            )
        }),
        TurnEvent::ReasoningDelta { content, .. } => {
            verbose.then(|| format!("reasoning: {content}"))
        }
        TurnEvent::ModelFinished { finish_reason, .. } => verbose.then(|| {
            format!(
                "model finished {}",
                finish_reason.as_deref().unwrap_or("unknown")
            )
        }),
        TurnEvent::CallsRegistered { calls, .. } => {
            verbose.then(|| format!("registered {} calls", calls.len()))
        }
        TurnEvent::ToolCallStarted { name, .. } => Some(format!("tool {name}")),
        TurnEvent::ToolCallFinished {
            name,
            outcome,
            material,
            error_summary,
            ..
        } => {
            let content = match material {
                MaterialSummary::Available { byte_length, .. } => format!("{byte_length} B"),
                MaterialSummary::OutputTooLarge { byte_length, .. } => {
                    format!("{byte_length} B 内容不可用")
                }
                MaterialSummary::NoContent => "无正文".into(),
            };
            Some(format!(
                "tool {name} {} {content} {}",
                outcome.as_str(),
                error_summary.as_deref().unwrap_or("")
            ))
        }
        TurnEvent::CepCallStarted { method, .. } => verbose.then(|| format!("cep {method}")),
        TurnEvent::CepCallFinished { method, ok, .. } => {
            verbose.then(|| format!("cep {method} ok={ok}"))
        }
        TurnEvent::StepClosed { step } => {
            verbose.then(|| format!("step {} closed", step.key.ordinal))
        }
        TurnEvent::TurnCompleted { turn, summary, .. }
        | TurnEvent::TurnFailed { turn, summary, .. }
        | TurnEvent::TurnCancelled { turn, summary, .. } => {
            summary_once(turn.turn_id, summary, state)
        }
        TurnEvent::TokenDelta { .. } => unreachable!(),
    };
    if let Some(line) = line {
        flush_before_status(out, err, state)?;
        writeln!(err, "{line}")?;
    }
    Ok(())
}
fn summary_once(
    turn_id: TurnId,
    summary: &TurnSummary,
    state: &mut RunRenderState,
) -> Option<String> {
    if state.printed_summaries.contains(&turn_id) {
        return None;
    }
    state.printed_summaries.push(turn_id);
    Some(summary_text(summary))
}
fn notice_level_label(level: &NoticeLevel) -> &'static str {
    match level {
        NoticeLevel::Info => "info",
        NoticeLevel::Warn => "warn",
        NoticeLevel::Error => "error",
    }
}
fn summary_text(summary: &TurnSummary) -> String {
    let mut text = outcome_label(&summary.outcome).to_string();
    if let Some(notice) = &summary.notice {
        text.push_str(&format!(
            " {}: {}",
            notice_level_label(&notice.level),
            notice.text
        ));
    }
    text
}
fn render_verbose_epilogue(state: &RunRenderState, err: &mut impl Write) -> std::io::Result<()> {
    if let Some(id) = state.session_id {
        writeln!(err, "session: {id}")?;
    }
    if let Some(summary) = &state.last_summary {
        writeln!(err, "outcome: {}", summary_text(summary))?;
    }
    if let Some(usage) = &state.usage {
        writeln!(
            err,
            "usage: {}",
            serde_json::to_string(usage).map_err(std::io::Error::other)?
        )?;
    }
    if state.tool_finished_count > 0 {
        writeln!(err, "tools: {} executed", state.tool_finished_count)?;
    }
    Ok(())
}

pub async fn run_session_prompt(
    api: &LocalClient,
    args: super::session::SendArgs,
    verbose: bool,
    json: bool,
) -> Result<std::process::ExitCode> {
    let session_id = match args.session_id {
        Some(prefix) => Some(api.resolve_session(&prefix).await?.session_id),
        None => None,
    };
    let turn_id = args.turn_id.unwrap_or_else(lattice_api::local::new_turn_id);
    submission_state(session_id, turn_id, "submitting", json);
    std::io::stdout().flush()?;
    std::io::stderr().flush()?;
    let root = tokio::task::spawn_blocking(lattice_api::resolve_local_workspace_root).await??;
    let request = TurnRequest {
        session_id,
        turn_id,
        workspace: root.canonical_path().into(),
        input: args.prompt,
        agent: args.agent,
    };
    let sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let turn = api.start_turn_with(request, move |name| async move {
        tokio::task::spawn_blocking(move || {
            let loaded = load_named_profile(&name, root.canonical_path())?;
            FrozenConfiguration::from_env(loaded, &root)
        })
        .await
        .map_err(|_| lattice_api::ApiError::Internal("configuration worker failed".into()))?
    });
    observe_with_signal(turn, session_id, true, verbose, json, sigint).await
}
pub async fn observe_turn(
    api: &LocalClient,
    session_id: SessionId,
    turn_id: TurnId,
    verbose: bool,
    json: bool,
) -> Result<std::process::ExitCode> {
    let sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    observe_with_signal(
        api.observe(Some(session_id), Some(turn_id)),
        Some(session_id),
        false,
        verbose,
        json,
        sigint,
    )
    .await
}
async fn observe_with_signal(
    mut turn: LocalTurn,
    mut session_id: Option<SessionId>,
    submitting: bool,
    verbose: bool,
    json: bool,
    mut sigint: tokio::signal::unix::Signal,
) -> Result<std::process::ExitCode> {
    let mut render = RunRenderState::default();
    let mut awaiting = submitting;
    let mut interrupted = false;
    let mut last_state: Option<LocalState> = None;
    let mut last_cancel = None;
    let mut last_cancel_error = None;
    let mut last_history_error = None;
    let mut last_submission_diagnostic = None;
    let mut generation = 0;
    let mut history_ready = false;
    let result=async {
        loop {
            tokio::select! {
                _=sigint.recv()=>{
                    if render.last_summary.is_some() {
                        connection_notice("历史读取被中断；已知结局保留，请重连读取",json);
                        break Ok(std::process::ExitCode::from(1));
                    }
                    if interrupted {break Ok(std::process::ExitCode::from(1));}
                    interrupted=true;turn.cancel();
                }
                update=turn.next()=>match update {
                    Some(LocalUpdate::Submission{result,..})=>{
                        awaiting=false;
                        match result {
                            Ok(acceptance)=>{
                                if let Acceptance::Accepted{turn}|Acceptance::Existing{turn}=&acceptance {session_id=Some(turn.session_id);}
                                render_acceptance(&acceptance,json)?;
                            }
                            Err(error)=>{
                                if render.last_summary.is_some() {report_error(&submission_error(error),json);}
                                else {break Err(submission_error(error));}
                            }
                        }
                    }
                    Some(LocalUpdate::SubmissionUnknown{turn_id,error})=>{
                        awaiting=false;submission_state(session_id,turn_id,"unknown",json);
                        if let Some(error)=error {report_error(&error.clone().into(),json);last_submission_diagnostic=Some(error);}
                    }
                    Some(LocalUpdate::NotSubmitted{turn_id,stage})=>{
                        awaiting=false;
                        submission_state(session_id,turn_id,not_submitted_label(stage),json);
                        if render.last_summary.is_none() {break Ok(std::process::ExitCode::from(1));}
                    }
                    Some(LocalUpdate::State{view,phase,error})=>{
                        session_id=Some(view.session.session_id);
                        render.session_id=session_id;
                        if let Some(detail)=&view.selected_turn {
                            if detail.summary.is_some() {render.last_summary=detail.summary.clone();}
                            render.usage=Some(detail.usage.clone());
                        }
                        let state = LocalState { view, phase, error };
                        render_local_state(&state, &mut render, json)?;
                        last_state=Some(state);
                        history_ready=false;
                    }
                    Some(LocalUpdate::Reset{view,generation:new_generation})=>{
                        generation=new_generation;history_ready=false;
                        if json {println!("{}",serde_json::json!({"type":"snapshot","generation":generation,"view":view}));}
                        else if let Some(attempt)=&view.attempt {
                            finish_text(&mut render,&mut std::io::stdout())?;
                            println!(
                                "--- 当前尝试 turn {} step {} attempt {} 快照 ---",
                                attempt.key.turn_id,
                                attempt.key.model_step_ordinal,
                                attempt.key.attempt_ordinal
                            );
                            for segment in &attempt.segments {if let lattice_api::local::TextSegment::Text{text}=segment {print!("{text}");}}
                            println!();
                        }
                    }
                    Some(LocalUpdate::History{generation:read_generation,result,stale,..})=>{
                        match result {
                            Ok(history) if !stale && read_generation==generation => {
                                if !json { finish_text(&mut render,&mut std::io::stdout())?; }
                                render_history(*history,json)?;history_ready=true;last_history_error=None;
                            }
                            Ok(_)=>{},
                            Err(error)=>{
                                render_history_error(&error,json);
                                if !stale {
                                    last_history_error=Some(error);history_ready=true;
                                    if render.last_summary.is_some() {break Ok(std::process::ExitCode::from(1));}
                                }
                            }
                        }
                        // 无 Owner 的 Running 不会收到 Finished；正文交付后允许离开，不伪造完成。
                        if !awaiting && history_ready && render.last_summary.is_none() && last_state.as_ref().is_some_and(|s|s.phase==ObservationPhase::NoOwner) {
                            break Ok(std::process::ExitCode::from(1));
                        }
                    }
                    Some(LocalUpdate::Finished{generation:finished_generation,..})=>{
                        if history_ready && finished_generation==generation {
                            break Ok(summary_exit(render.last_summary.as_ref()));
                        }
                    }
                    Some(LocalUpdate::Event{event,..})=>{
                        if json {render_json_event(&event,&mut std::io::stdout())?;}
                        else {render_human_event(&event,&mut render,&mut std::io::stdout(),&mut std::io::stderr(),verbose)?;}
                        apply_event_to_state(&event,&mut render);std::io::stdout().flush()?;
                    }
                    Some(LocalUpdate::Cancel{turn_id,status,error})=>{
                        render_cancel(status,json);
                        if let Some(error)=&error {render_cancel_error(session_id,Some(turn_id),error,json);}
                        last_cancel=Some(status);last_cancel_error=error;
                    }
                    Some(LocalUpdate::Connection(status))=>{
                        match status {ConnectionStatus::Error(error)=>{report_error(&error.into(),json);},ConnectionStatus::Eof=>connection_notice("observation ended; reconnect with the same TurnId",json),ConnectionStatus::Idle=>connection_notice("observation timed out; reconnect with the same TurnId",json)}
                        if !awaiting {break Ok(std::process::ExitCode::from(1));}
                    }
                    None=>break Ok(std::process::ExitCode::from(1)),
                }
            }
        }
    }.await;
    let report = turn.detach().await;
    if let Some(state) = &report.state
        && last_state.as_ref() != Some(state)
    {
        render_local_state(state, &mut render, json)?;
    }
    if let Some(status) = report.cancel
        && last_cancel != Some(status)
    {
        render_cancel(status, json);
    }
    if let Some(error) = &report.cancel_error
        && last_cancel_error.as_ref() != Some(error)
    {
        render_cancel_error(report.session_id, report.turn_id, error, json);
    }
    if let Some(stage) = report.not_submitted
        && let Some(id) = report.turn_id
    {
        submission_state(report.session_id, id, not_submitted_label(stage), json);
    }
    if awaiting {
        match &report.submission {
            Some(Ok(acceptance)) => render_acceptance(acceptance, json)?,
            Some(Err(error)) => {
                report_error(&submission_error(error.clone()), json);
            }
            None => {
                if let Some(id) = report.turn_id {
                    submission_state(report.session_id, id, "unknown", json);
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
        if json {
            println!(
                "{}",
                serde_json::json!({"type":"cancel_unconfirmed","session_id":report.session_id,"turn_id":id})
            );
        } else {
            eprintln!(
                "取消未确认：session {} turn {id}",
                session_label(report.session_id)
            );
        }
    }
    if let Some(error) = &report.history_error
        && last_history_error.as_ref() != Some(error)
    {
        render_history_error(error, json);
    }
    if !json {
        finish_text(&mut render, &mut std::io::stdout())?;
        if verbose {
            render_verbose_epilogue(&render, &mut std::io::stderr())?;
        }
    }
    if report.history_error.is_some()
        || last_history_error.is_some()
        || report.state.is_some_and(|s| s.error.is_some())
    {
        Ok(std::process::ExitCode::from(1))
    } else {
        result
    }
}
fn summary_exit(summary: Option<&TurnSummary>) -> std::process::ExitCode {
    std::process::ExitCode::from(match summary.map(|s| &s.outcome) {
        Some(PublicTurnOutcome::Completed) => 0,
        Some(PublicTurnOutcome::Cancelled) => 2,
        _ => 1,
    })
}
fn session_label(id: Option<SessionId>) -> String {
    id.map_or_else(|| "尚未接受".into(), |id| id.to_string())
}
fn not_submitted_label(stage: NotSubmittedStage) -> &'static str {
    match stage {
        NotSubmittedStage::NeedsConfiguration => "not_accepted",
        NotSubmittedStage::FreezingConfiguration => "not_sent",
    }
}
fn submission_state(session_id: Option<SessionId>, turn_id: TurnId, state: &str, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({"type":"submission","session_id":session_id,"turn_id":turn_id,"state":state})
        );
    } else {
        eprintln!(
            "session {} turn {turn_id}: {state}；原身份用于查询/重发",
            session_label(session_id)
        );
    }
}
fn render_acceptance(acceptance: &Acceptance, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::json!({"type":"acceptance","result":acceptance})
        );
    } else {
        match acceptance {
            Acceptance::Accepted { turn } | Acceptance::Existing { turn } => eprintln!(
                "session {} turn {}: {}",
                turn.session_id,
                turn.turn_id,
                if matches!(acceptance, Acceptance::Existing { .. }) {
                    "找回既有请求；未重复执行"
                } else {
                    "已确认接受"
                }
            ),
            Acceptance::ConfirmationUnknown {
                turn_id,
                session_id,
            } => submission_state(*session_id, *turn_id, "unknown", false),
            Acceptance::NeedsConfiguration { .. } => eprintln!("尚需配置；未接受"),
        }
    }
    Ok(())
}
fn render_local_state(state: &LocalState, render: &mut RunRenderState, json: bool) -> Result<()> {
    render_snapshot(&state.view, render, json, &mut std::io::stderr())?;
    if let Some(error) = &state.error {
        report_error(&error.clone().into(), json);
    }
    Ok(())
}
fn render_snapshot(
    view: &ObservationSnapshot,
    render: &mut RunRenderState,
    json: bool,
    err: &mut impl Write,
) -> Result<()> {
    if json {
        println!("{}", serde_json::json!({"type":"snapshot","view":view}));
    } else {
        writeln!(
            err,
            "session {} current={} active={}",
            view.session.session_id,
            view.session.current.0,
            view.session
                .active_turn
                .map_or_else(|| "none".into(), |id| id.to_string())
        )?;
        if let Some(turn) = &view.selected_turn
            && let Some(summary) = &turn.summary
            && let Some(line) = summary_once(turn.turn_id, summary, render)
        {
            writeln!(err, "{line}")?;
        }
    }
    Ok(())
}
fn render_history(history: LocalHistory, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::json!({"type":"history","history":history})
        );
        return Ok(());
    }
    println!("--- 固定根路径正文，current={} ---", history.anchor.head.0);
    for entry in history.entries {
        let (message, results) = match entry.content {
            Node::Root => continue,
            Node::UserMessage { message, .. } => (message, vec![]),
            Node::Step { content, .. } => (content.assistant, content.results),
        };
        println!("node {} agent={}", entry.node.0, entry.agent_name);
        for part in message.parts {
            if let MessageSemantic::Text { text } = part.semantic {
                println!("{text}");
            }
        }
        for result in results {
            println!(
                "{} · {}",
                result.name,
                serde_json::to_string(&result.payload)?
            );
        }
    }
    Ok(())
}
fn render_history_error(error: &HistoryReadError, json: bool) {
    connection_notice("历史正文未读取；已知结局保留，请重连", json);
    report_error(
        &anyhow::Error::new(error.clone()).context(HistoryFailure),
        json,
    );
}
fn render_cancel_error(
    session_id: Option<SessionId>,
    turn_id: Option<TurnId>,
    error: &lattice_api::local::LocalError,
    json: bool,
) {
    connection_notice(
        &format!(
            "取消诊断：session {} turn {}",
            session_label(session_id),
            turn_id.map_or_else(|| "未知".into(), |id| id.to_string())
        ),
        json,
    );
    report_error(&error.clone().into(), json);
}
fn render_cancel(status: CancelStatus, json: bool) {
    match status {
        CancelStatus::Taken => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"type":"cancel_acknowledged","owned":true})
                );
            } else {
                eprintln!("后台已接管取消；持久终态仍待查询");
            }
        }
        CancelStatus::NotTaken => {
            connection_notice("后台在判定点未接管本次取消；不表示没有后台工作", json)
        }
        CancelStatus::Unknown => connection_notice("cancel acknowledgement unknown", json),
        CancelStatus::Pending | CancelStatus::Moot => {}
    }
}
fn connection_notice(message: &str, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({"type":"connection","message":message})
        );
    } else {
        eprintln!("{message}");
    }
}
#[derive(Debug)]
struct HistoryFailure;
impl std::fmt::Display for HistoryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fixed history read")
    }
}
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
                .map(|e| e.error.clone())
        })
        .or_else(|| {
            error
                .downcast_ref::<lattice_api::ApiError>()
                .cloned()
                .map(Into::into)
        });
    let code = match local.as_ref().map(|e| e.kind) {
        _ if error.is::<HistoryFailure>() => 1,
        Some(LocalErrorKind::BadRequest | LocalErrorKind::NotFound) => 2,
        Some(LocalErrorKind::Conflict) if error.is::<Submission>() => 2,
        _ => 1,
    };
    let record = match local {
        Some(e) => {
            serde_json::json!({"type":"error","kind":e.kind,"message":e.message,"recovery":e.recovery,"store":e.store})
        }
        None => {
            serde_json::json!({"type":"error","kind":"internal","message":"command failed; inspect configuration and durable state"})
        }
    };
    (code, record)
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
        if !value["store"].is_null() {
            let _ = writeln!(std::io::stderr(), "{}", value["store"]);
        }
    }
    std::process::ExitCode::from(code)
}
pub(crate) fn load_named_profile(
    agent_name: &str,
    project_root: &Path,
) -> Result<LoadedAgentProfile, lattice_api::ApiError> {
    let global_agents_dir = safe_agents_dir(None)?;
    lattice_api::resolve_named_agent_profile(agent_name, project_root, &global_agents_dir)
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
pub(crate) fn safe_agents_dir(
    override_path: Option<&str>,
) -> Result<PathBuf, lattice_api::ApiError> {
    let path = agents_dir(override_path);
    validate_agents_dir(&path)?;
    Ok(path)
}
pub(crate) use lattice_api::validate_agents_dir;
#[cfg(test)]
#[path = "run_ipc_tests.rs"]
mod ipc_tests;
#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "config_error_tests.rs"]
mod config_error_tests;
