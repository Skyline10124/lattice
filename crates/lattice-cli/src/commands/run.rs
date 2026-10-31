// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! CLI 消费提交回执与会话视图，执行与取消交接由 LocalTurn 管理。
use anyhow::Result;
use lattice_api::local::*;
use std::io::Write;

#[derive(Default)]
struct RunRenderState {
    target: Option<TurnId>,
    summary: Option<TurnSummary>,
    head: Option<SessionHead>,
    receipt: Option<SubmissionReceipt>,
    cancel: Option<(CancelStatus, Option<LocalError>)>,
    failed: bool,
    text: std::collections::BTreeMap<NodeId, (u32, String)>,
    retries: std::collections::BTreeSet<(NodeId, u32)>,
    results: std::collections::BTreeSet<InvocationId>,
    steps: std::collections::BTreeMap<NodeId, StepDetail>,
    newline: bool,
}
fn render_update(
    update: &LocalUpdate,
    state: &mut RunRenderState,
    out: &mut impl Write,
    err: &mut impl Write,
    verbose: bool,
    json: bool,
) -> Result<()> {
    match update {
        LocalUpdate::Submit {
            update: SubmissionUpdate::Receipt(receipt),
        } => state.receipt = Some(receipt.clone()),
        LocalUpdate::Head(head) => {
            if head.diagnostic.is_some() && state.target.is_none_or(|id| head.turn_id == Some(id)) {
                state.failed = true;
            }
            state.head = Some(head.clone());
        }
        LocalUpdate::Entry { entry } => {
            if let ConversationEntry::Step {
                node,
                step,
                summary,
                ..
            } = entry.as_ref()
            {
                state.steps.insert(*node, step.clone());
                if let Some(summary) = summary {
                    state.summary = Some(summary.clone());
                }
            }
        }
        LocalUpdate::Cancel { status, error, .. } => state.cancel = Some((*status, error.clone())),
        LocalUpdate::Connection { .. } | LocalUpdate::Error { .. } => state.failed = true,
        _ => {}
    }
    if json {
        writeln!(out, "{}", serde_json::to_string(update)?)?;
        out.flush()?;
        return Ok(());
    }
    match update {
        LocalUpdate::Submit {
            update:
                SubmissionUpdate::Sending {
                    turn_id,
                    session_id,
                    ..
                },
        } => writeln!(err, "session {session_id} turn {turn_id}: sending")?,
        LocalUpdate::Submit {
            update: SubmissionUpdate::Receipt(receipt),
        } => match &receipt.result {
            SubmissionResult::Accepted { .. } => writeln!(
                err,
                "turn {}: accepted{}",
                receipt.turn_id,
                if receipt.recovered {
                    " (recovered)"
                } else {
                    ""
                }
            )?,
            SubmissionResult::Rejected { error } => {
                writeln!(err, "turn {}: rejected", receipt.turn_id)?;
                render_human_error(&serde_json::to_value(error)?, err)?;
            }
            SubmissionResult::NotAccepted => {
                writeln!(err, "turn {}: not accepted", receipt.turn_id)?
            }
            SubmissionResult::Unknown { error } => writeln!(
                err,
                "turn {}: acceptance unknown{}",
                receipt.turn_id,
                error
                    .as_ref()
                    .map_or_else(String::new, |e| format!(": {}", e.message))
            )?,
        },
        LocalUpdate::Text(text) => {
            let value = state
                .text
                .entry(text.node)
                .or_insert((text.attempt, String::new()));
            if value.0 != text.attempt {
                *value = (text.attempt, String::new());
            }
            for segment in &text.segments {
                match segment {
                    TextSegment::Text { text } => {
                        write!(out, "{text}")?;
                        value.1.push_str(text);
                        state.newline = !text.ends_with('\n');
                    }
                    TextSegment::Reasoning { text } if verbose => {
                        writeln!(err, "reasoning: {text}")?
                    }
                    _ => {}
                }
            }
        }
        LocalUpdate::Entry { entry } => {
            if let ConversationEntry::Step {
                node,
                content,
                retries,
                ..
            } = entry.as_ref()
            {
                if verbose {
                    for retry in retries {
                        if state.retries.insert((*node, retry.attempt)) {
                            writeln!(
                                err,
                                "{}: {} (retry after {} ms)",
                                level(&retry.notice.level),
                                retry.notice.text,
                                retry.delay_ms
                            )?;
                        }
                    }
                }
                if let Node::Step {
                    turn_id,
                    progress,
                    slots,
                    ..
                } = content
                    && let Some(content) = content.step_content()
                {
                    let ended = progress.is_closed()
                        || state
                            .head
                            .as_ref()
                            .is_some_and(|head| !head.owner || head.turn_id != Some(*turn_id));
                    let final_text = content
                        .assistant
                        .parts
                        .iter()
                        .filter_map(|part| match &part.semantic {
                            MessageSemantic::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>();
                    let printed = state.text.entry(*node).or_default();
                    if let Some(rest) = final_text.strip_prefix(&printed.1) {
                        if !rest.is_empty() {
                            write!(out, "{rest}")?;
                            state.newline = !rest.ends_with('\n');
                        }
                    } else if printed.1 != final_text {
                        if state.newline {
                            writeln!(out)?;
                        }
                        write!(out, "{final_text}")?;
                        state.newline = !final_text.ends_with('\n');
                    }
                    printed.1 = final_text;
                    // 运行中的 Registered/Started 只是进度；推定结果只在执行结束后呈现。
                    for (result, slot) in content.results.into_iter().zip(slots) {
                        if (ended || matches!(slot.progress, SlotProgress::Terminal { .. }))
                            && state.results.insert(result.invocation_id)
                        {
                            writeln!(
                                err,
                                "tool {} {}",
                                result.name,
                                serde_json::to_string(&result.payload)?
                            )?;
                        }
                    }
                }
            }
        }
        LocalUpdate::Head(head) => {
            if let Some(error) = &head.diagnostic {
                writeln!(err, "execution diagnostic: {}", error.message)?;
            }
        }
        LocalUpdate::Cancel {
            turn_id,
            status,
            error,
            ..
        } => {
            writeln!(
                err,
                "turn {turn_id}: cancel {}",
                match status {
                    CancelStatus::Taken => "taken; the durable outcome is still to be queried",
                    CancelStatus::NotTaken => "not taken; background work may still exist",
                    CancelStatus::Unknown => "unknown",
                    CancelStatus::Pending => "pending",
                    CancelStatus::Moot => "moot",
                }
            )?;
            if let Some(error) = error {
                writeln!(err, "cancel diagnostic: turn {turn_id}: {}", error.message)?;
            }
        }
        LocalUpdate::Connection { status } => match status {
            ConnectionStatus::Eof => writeln!(err, "observation ended; query the original TurnId")?,
            ConnectionStatus::Idle => {
                writeln!(err, "observation timed out; query the original TurnId")?
            }
            ConnectionStatus::Error(error) => {
                writeln!(err, "observation error: {}", error.message)?
            }
        },
        LocalUpdate::Error { error } => writeln!(err, "error: {}", error.message)?,
    }
    out.flush()?;
    err.flush()?;
    Ok(())
}
pub async fn run_session_prompt(
    api: &LocalClient,
    args: super::session::SendArgs,
    verbose: bool,
    json: bool,
) -> Result<std::process::ExitCode> {
    let session = match &args.session_id {
        Some(prefix) => SubmitSession::Existing(api.resolve_session(prefix).await?.session_id),
        None => SubmitSession::New(lattice_api::local::new_session_id()),
    };
    let id = lattice_api::local::new_turn_id();
    let mut state = RunRenderState::default();
    render_update(
        &LocalUpdate::Submit {
            update: SubmissionUpdate::Sending {
                turn_id: id,
                session_id: session.id(),
                new: session.is_new(),
            },
        },
        &mut state,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
        verbose,
        json,
    )?;
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let prepare = async {
        let root = tokio::task::spawn_blocking(lattice_api::resolve_local_workspace_root)
            .await
            .map_err(|_| local_error("workspace worker failed"))?
            .map_err(LocalError::from)?;
        Ok::<_, LocalError>(root.canonical_path().to_path_buf())
    };
    let prepared = tokio::select! {biased;_=signal.recv()=>None,result=prepare=>Some(result)};
    let workspace = match prepared {
        Some(Ok(value)) => value,
        result => {
            let result = match result {
                Some(Err(error)) => SubmissionResult::Rejected { error },
                _ => SubmissionResult::NotAccepted,
            };
            let code = receipt_exit(&result);
            render_update(
                &LocalUpdate::Submit {
                    update: SubmissionUpdate::Receipt(SubmissionReceipt {
                        turn_id: id,
                        result,
                        recovered: false,
                    }),
                },
                &mut state,
                &mut std::io::stdout(),
                &mut std::io::stderr(),
                verbose,
                json,
            )?;
            return Ok(code);
        }
    };
    let request = TurnRequest {
        model: None,
        effort: None,
        turn_id: id,
        session,
        workspace,
        input: args.prompt,
        agent: args.agent,
    };
    let turn = api.start_turn(request);
    consume(
        turn,
        None,
        state,
        verbose,
        json,
        signal,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
    .await
}
pub(super) async fn follow_with_output(
    api: &LocalClient,
    detail: TurnDetail,
    verbose: bool,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<std::process::ExitCode> {
    let signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    consume(
        api.follow_turn(detail.clone()),
        Some((api.clone(), detail)),
        RunRenderState::default(),
        verbose,
        json,
        signal,
        out,
        err,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
async fn consume(
    mut turn: LocalTurn,
    fallback: Option<(LocalClient, TurnDetail)>,
    mut state: RunRenderState,
    verbose: bool,
    json: bool,
    mut signal: tokio::signal::unix::Signal,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<std::process::ExitCode> {
    state.target = turn.turn_id();
    let mut interrupted = false;
    let mut buffered = Vec::new();
    let mut body = fallback.is_none();
    let mut sending_printed = body;
    loop {
        tokio::select! {biased;
            _=signal.recv()=>{if interrupted{state.failed=true;break;}interrupted=true;turn.cancel();}
            update=turn.next()=>{
                let Some(update)=update else{break};
                if matches!(update,LocalUpdate::Submit{update:SubmissionUpdate::Sending{..}}) {if sending_printed{continue;}sending_printed=true;}
                if !body {
                    // 中途退回也会给出本轮的部分条目；收到带结局的最后一步后才能输出正文。
                    if matches!(&update,LocalUpdate::Entry{entry} if matches!(entry.as_ref(),ConversationEntry::Step{summary:Some(_),..})) {body=true;for update in buffered.drain(..){render_update(&update,&mut state,out,err,verbose,json)?;}}
                    else if matches!(&update,LocalUpdate::Connection{..}|LocalUpdate::Error{..}) {state.failed=true;}
                    if !body {buffered.push(update);continue;}
                }
                render_update(&update,&mut state,out,err,verbose,json)?;
            }
        }
    }
    let report = turn.detach().await;
    // 退出码依据已经输出的唯一回执；迟到结果仍可通过视图呈现。
    if state.receipt.is_none()
        && let Some(receipt) = report.submission
    {
        render_update(
            &LocalUpdate::Submit {
                update: SubmissionUpdate::Receipt(receipt),
            },
            &mut state,
            out,
            err,
            verbose,
            json,
        )?;
    }
    if let Some(status @ (CancelStatus::Taken | CancelStatus::NotTaken | CancelStatus::Unknown)) =
        report.cancel
        && state.cancel.as_ref() != Some(&(status, report.cancel_error.clone()))
    {
        render_update(
            &LocalUpdate::Cancel {
                turn_id: report.turn_id.expect("cancel identity"),
                status,
                owned: match status {
                    CancelStatus::Taken => Some(true),
                    CancelStatus::NotTaken => Some(false),
                    _ => None,
                },
                error: report.cancel_error,
            },
            &mut state,
            out,
            err,
            verbose,
            json,
        )?;
    }
    if !body {
        if state.failed {
            for update in buffered {
                render_update(&update, &mut state, out, err, verbose, json)?;
            }
        } else if let Some((api, initial)) = fallback {
            // 打开视图前的详情可能仍在途；缺少最后一步时只交付重新查询的持久结局。
            let detail = api.wait_for_turn(initial.turn_id).await?;
            if json {
                writeln!(out, "{}", serde_json::to_string(&detail)?)?;
            } else {
                write!(out, "{}", super::session::render_turn(&detail))?;
            }
            return Ok(summary_exit(Some(&detail.summary)));
        }
    }
    if state.summary.is_none() {
        state.summary = report.summary;
    }
    if !json {
        finish_human(&state, out, err, verbose)?;
    }
    if state.failed {
        return Ok(std::process::ExitCode::from(1));
    }
    if let Some(receipt) = state.receipt.as_ref()
        && !matches!(receipt.result, SubmissionResult::Accepted { .. })
    {
        return Ok(receipt_exit(&receipt.result));
    }
    Ok(summary_exit(state.summary.as_ref()))
}
fn local_error(message: &str) -> LocalError {
    LocalError {
        kind: LocalErrorKind::Internal,
        message: message.into(),
        conflict: None,
        recovery: None,
        store: None,
    }
}
fn summary_exit(summary: Option<&TurnSummary>) -> std::process::ExitCode {
    std::process::ExitCode::from(match summary.map(|s| &s.outcome) {
        Some(PublicTurnOutcome::Completed) => 0,
        Some(PublicTurnOutcome::Cancelled) => 2,
        _ => 1,
    })
}
fn receipt_exit(result: &SubmissionResult) -> std::process::ExitCode {
    match result {
        SubmissionResult::Rejected { error } => std::process::ExitCode::from(match error.kind {
            LocalErrorKind::BadRequest | LocalErrorKind::NotFound | LocalErrorKind::Conflict => 2,
            _ => 1,
        }),
        _ => std::process::ExitCode::from(1),
    }
}
fn level(level: &NoticeLevel) -> &'static str {
    match level {
        NoticeLevel::Info => "info",
        NoticeLevel::Warn => "warn",
        NoticeLevel::Error => "error",
    }
}
fn finish_human(
    state: &RunRenderState,
    out: &mut impl Write,
    err: &mut impl Write,
    verbose: bool,
) -> Result<()> {
    if state.newline {
        writeln!(out)?;
    }
    if let Some(summary) = &state.summary {
        write!(
            err,
            "{}",
            match summary.outcome {
                PublicTurnOutcome::Completed => "completed",
                PublicTurnOutcome::Failed => "failed",
                PublicTurnOutcome::Cancelled => "cancelled",
            }
        )?;
        if let Some(notice) = &summary.notice {
            write!(err, " {}: {}", level(&notice.level), notice.text)?;
        }
        writeln!(err)?;
    }
    if verbose {
        if let Some(head) = &state.head {
            writeln!(err, "session: {}", head.session_id)?;
        }
        let usage = lattice_api::UsageSummary::from_attempts(
            state
                .steps
                .values()
                .flat_map(|step| &step.attempts)
                .map(|attempt| (attempt.usage, attempt.usage_kind, attempt.cost)),
        );
        writeln!(err, "usage: {}", serde_json::to_string(&usage)?)?;
        if !state.results.is_empty() {
            writeln!(err, "tools: {} results", state.results.len())?;
        }
    }
    out.flush()?;
    err.flush()?;
    Ok(())
}
fn error_record(error: &anyhow::Error) -> (u8, serde_json::Value) {
    let local = error.downcast_ref::<LocalError>().cloned().or_else(|| {
        error
            .downcast_ref::<lattice_api::ApiError>()
            .cloned()
            .map(Into::into)
    });
    let code = match local.as_ref().map(|e| e.kind) {
        Some(LocalErrorKind::BadRequest | LocalErrorKind::NotFound | LocalErrorKind::Conflict) => 2,
        _ => 1,
    };
    let record = match local {
        Some(error) => {
            let mut value = serde_json::to_value(error).expect("local error");
            value["type"] = serde_json::json!("error");
            value
        }
        None => {
            serde_json::json!({"type":"error","kind":"internal","message":"command failed; inspect configuration and durable state"})
        }
    };
    (code, record)
}
pub(super) fn render_human_error(value: &serde_json::Value, err: &mut impl Write) -> Result<()> {
    writeln!(
        err,
        "error: {}",
        value["message"].as_str().unwrap_or("command failed")
    )?;
    if !value["store"].is_null() {
        writeln!(err, "{}", value["store"])?;
    }
    Ok(())
}
pub fn report_error(error: &anyhow::Error, json: bool) -> std::process::ExitCode {
    let (code, value) = error_record(error);
    if json {
        let _ = writeln!(std::io::stdout(), "{value}");
    } else {
        let _ = render_human_error(&value, &mut std::io::stderr());
    }
    std::process::ExitCode::from(code)
}
#[cfg(test)]
fn submission_error(error: LocalError) -> anyhow::Error {
    anyhow::Error::new(error).context("Turn submission")
}
#[cfg(test)]
#[path = "config_error_tests.rs"]
mod config_error_tests;
#[cfg(test)]
#[path = "run_ipc_tests.rs"]
mod ipc_tests;
#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
