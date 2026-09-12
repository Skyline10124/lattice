// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::time::{Duration, Instant};

use crate::connection::{Connection, Update};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::StreamExt;
use lattice_api::TurnEvent;
use lattice_api::local::{LocalClient, LocalErrorKind, SessionObservation, TurnId, TurnRequest};

use crate::TuiOptions;
use crate::input::{CTRL_C_WINDOW, History, InputBuffer, KeyAction, resolve_key};
use crate::session::SessionContext;
use crate::view::{Dock, Sidebar, Viewport, frame_layout, transcript_lines};

const FRAME: Duration = Duration::from_millis(33);

/// Dock 上 Warn / Error 两个非 Prompt 状态的严重级。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DockSeverity {
    Warn,
    Error,
}

/// 状态事件的展示位置：直接进 transcript，或暂驻 Dock。
/// 不变量：`blocks` 中至多存在一个 `StatusPlacement::Dock(_)`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusPlacement {
    Transcript,
    Dock(DockSeverity),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    /// 渲染作用域，不单独占行；历史不能被重标为当前 Agent 的发言。
    AgentName(String),
    User(String),
    /// Accumulates `TokenDelta`. A new delta appends to the trailing
    /// `Assistant` block when one exists, otherwise pushes a new block.
    Assistant(String),
    Tool {
        id: String,
        name: String,
        state: ToolState,
    },
    Status {
        kind: StatusKind,
        placement: StatusPlacement,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[rustfmt::skip]
pub(crate) enum ToolState {
    Running,
    Unknown,
    Unavailable { outcome: String },
    Ok { output_len: usize },
    Failed { output_len: usize, error: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[rustfmt::skip]
pub(crate) enum StatusKind {
    Committed { commit_id: String },
    Visible { commit_id: String },
    CommitFailed { reason: String },
    TurnFailed { reason: String },
    Cancelled { reason: Option<String> },
    LlmRetry { attempt: u32, max_attempts: u32, delay_ms: u64, message: String },
    LlmFailed { attempt: u32, max_attempts: u32, message: String },
    /// `/help` output and startup banner.
    Notice(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnPhase {
    Idle,
    Streaming,
    Committing,
    /// 取消请求已发起，接管确认与持久终态分别观察。
    Cancelling,
}

pub(crate) struct App {
    pub(crate) ctx: SessionContext,
    pub(crate) blocks: Vec<Block>,
    pub(crate) input: InputBuffer,
    pub(crate) history: History,
    pub(crate) viewport: Viewport,
    pub(crate) phase: TurnPhase,
    pub(crate) current_turn: Option<TurnId>,
    observed_turn: Option<TurnId>,
    // 完成事实独立于正文重载；连接错误不能撤销已经取得的确认。
    completed_turn: Option<(Box<lattice_api::local::CommittedTurnObservation>, bool)>,
    pub(crate) connected: bool,
    cancel_requested: bool,
    reconnect_requested: bool,
    submitted: Option<(TurnId, String)>,
    durable_len: usize,
    gap_pending: bool,
    accept_live_events: bool,
    accept_deltas: bool,
    acceptance_notice: Option<String>,
    /// Set when `supports_keyboard_enhancement()` reported true at startup.
    pub(crate) kitty_keyboard: bool,
    /// Instant of the last Ctrl+C, for the double-press exit rule.
    pub(crate) last_ctrl_c: Option<Instant>,
    pub(crate) should_quit: bool,
    pub(crate) term_width: u16,
    pub(crate) term_height: u16,
}

impl App {
    pub(crate) fn new(ctx: SessionContext, kitty_keyboard: bool, session_dir: &Path) -> Self {
        Self {
            ctx,
            blocks: Vec::new(),
            input: InputBuffer::new(),
            history: History::load(session_dir),
            viewport: Viewport::new(),
            phase: TurnPhase::Idle,
            current_turn: None,
            observed_turn: None,
            completed_turn: None,
            connected: true,
            cancel_requested: false,
            reconnect_requested: false,
            submitted: None,
            durable_len: 0,
            gap_pending: false,
            accept_live_events: true,
            accept_deltas: true,
            acceptance_notice: None,
            kitty_keyboard,
            last_ctrl_c: None,
            should_quit: false,
            term_width: 80,
            term_height: 24,
        }
    }

    /// Pure: fold one `TurnEvent` into `blocks` / `phase`. Exhaustive `match`
    /// with **no `_ =>` arm** (§9).
    #[rustfmt::skip]
    pub(crate) fn apply_turn_event(&mut self, event: TurnEvent) {
        if matches!(event, TurnEvent::TokenDelta { .. }) && !self.accept_deltas { return; }
        match event {
            TurnEvent::TokenDelta { content } => match self.blocks.last_mut() {
                Some(Block::Assistant(buf)) => buf.push_str(&content),
                _ => self.blocks.push(Block::Assistant(content)),
            },
            TurnEvent::ToolCallStarted { id, name } => {
                self.blocks.push(Block::Tool {
                    id,
                    name,
                    state: ToolState::Running,
                });
            }
            TurnEvent::ToolCallFinished {
                id,
                name: _,
                ok,
                output_len,
                error_summary,
            } => {
                if let Some(Block::Tool { state, .. }) = self
                    .blocks
                    .iter_mut()
                    .rev()
                    .find(|b| matches!(b, Block::Tool { id: tid, .. } if *tid == id))
                {
                    *state = if ok {
                        ToolState::Ok { output_len }
                    } else {
                        ToolState::Failed {
                            output_len,
                            error: error_summary,
                        }
                    };
                }
            }
            TurnEvent::TurnCommitting => {
                if self.phase == TurnPhase::Streaming {
                    self.phase = TurnPhase::Committing;
                }
            }
            TurnEvent::TurnCommitted { commit_id, .. } => {
                self.fail_running_tools("turn ended before tool completion");
                self.push_status(StatusKind::Visible { commit_id });
            }
            TurnEvent::CommitFailed { reason } => {
                self.fail_running_tools(&reason);
                self.push_dock_status(StatusKind::CommitFailed { reason }, DockSeverity::Error);
            }
            TurnEvent::EvidenceDurabilityFailed { attempts } => {
                let reason = format!(
                    "terminal evidence durability failed after {attempts} attempt(s)"
                );
                self.fail_running_tools(&reason);
                self.push_dock_status(StatusKind::CommitFailed { reason }, DockSeverity::Error);
            }
            TurnEvent::RecoveryRequired { reason } => {
                let reason = format!("recovery required: {reason}");
                self.fail_running_tools(&reason);
                self.push_dock_status(StatusKind::CommitFailed { reason }, DockSeverity::Error);
            }
            TurnEvent::TurnFailed { reason } => {
                self.fail_running_tools(&reason);
                self.push_dock_status(StatusKind::TurnFailed { reason }, DockSeverity::Error);
            }
            TurnEvent::TurnCancelled { reason } => {
                self.fail_running_tools(reason.as_deref().unwrap_or("cancelled"));
                self.push_status(StatusKind::Cancelled { reason });
            }
            TurnEvent::LlmRequestError {
                attempt,
                max_attempts,
                will_retry,
                delay_ms,
                message,
                ..
            } => {
                if will_retry {
                    self.push_dock_status(
                        StatusKind::LlmRetry {
                            attempt,
                            max_attempts,
                            delay_ms: delay_ms.unwrap_or(0),
                            message,
                        },
                        DockSeverity::Warn,
                    );
                } else {
                    self.push_dock_status(
                        StatusKind::LlmFailed {
                            attempt,
                            max_attempts,
                            message,
                        },
                        DockSeverity::Error,
                    );
                }
            }
            TurnEvent::ModelStarted { .. } => {
                // 只有 cut 后完整观察到的新模型段才能拼接；不能把后缀接到已加载的旧消息。
                self.accept_deltas = true;
                self.blocks.push(Block::Assistant(String::new()));
            }
            TurnEvent::ModelFinished { .. } => self.accept_deltas = false,
            TurnEvent::TurnStarted { .. }
            | TurnEvent::ReasoningDelta { .. }
            | TurnEvent::CepCallStarted { .. }
            | TurnEvent::CepCallFinished { .. } => {}
        }
    }

    /// Pure: fold one resolved key action. Returns `true` when a turn should be
    /// submitted with the returned prompt.
    #[rustfmt::skip]
    pub(crate) fn apply_action(&mut self, action: KeyAction) -> Option<String> {
        if self.dock_status().is_some() {
            match action {
                // 无修饰 Space：释放当前 WARN / ERROR 回 transcript，
                // Space 被消费，不进入 InputBuffer。
                KeyAction::Insert(' ') => { self.release_dock_status(); return None; }
                // Dock 显示状态时，输入编辑与提交不得改动不可见的 InputBuffer。
                KeyAction::Insert(_)
                | KeyAction::Paste(_)
                | KeyAction::Newline
                | KeyAction::Submit
                | KeyAction::Backspace
                | KeyAction::Delete
                | KeyAction::Left
                | KeyAction::Right
                | KeyAction::Home
                | KeyAction::End
                | KeyAction::Up
                | KeyAction::Down => return None,
                // PageUp / PageDown、Ctrl+C、Ctrl+D 等全局动作沿用现有语义。
                _ => {}
            }
        }
        match action {
            KeyAction::Insert(c) => self.input.insert_char(c),
            KeyAction::Paste(text) => self.input.insert_paste(&text),
            KeyAction::Newline => self.input.newline(),
            KeyAction::Submit => return self.submit(),
            KeyAction::Backspace => self.input.backspace(),
            KeyAction::Delete => self.input.delete(),
            KeyAction::Left => self.input.move_left(),
            KeyAction::Right => self.input.move_right(),
            KeyAction::Home => self.input.home(),
            KeyAction::End => self.input.end(),
            KeyAction::Up => {
                if !self.input.move_up()
                    && let Some(entry) = self.history.prev()
                {
                    self.input.set_text(entry);
                }
            }
            KeyAction::Down => {
                if !self.input.move_down()
                    && let Some(entry) = self.history.next()
                {
                    self.input.set_text(entry);
                }
            }
            KeyAction::ScrollPageUp => self.viewport.page_up(self.transcript_height()),
            KeyAction::ScrollPageDown => {
                let layout = frame_layout(self.term_width, self.term_height);
                let rows = self.content_rows(layout.content_w);
                self.viewport.page_down(rows, self.transcript_height());
            }
            KeyAction::ScrollToBottom => {
                let layout = frame_layout(self.term_width, self.term_height);
                let rows = self.content_rows(layout.content_w);
                self.viewport.to_bottom(rows, self.transcript_height());
            }
            KeyAction::CancelOrClear => self.cancel_or_clear(),
            KeyAction::Quit => self.should_quit = true,
            KeyAction::Ignored => {}
        }
        None
    }

    /// 本地指令不提交 Turn；重连只重新观察，普通退出不触发取消。
    #[rustfmt::skip]
    pub(crate) fn apply_builtin(&mut self, input: &str) -> bool {
        match input {
            "/quit" | "/exit" | "/q" => { self.should_quit = true; true }
            "/reconnect" => { self.reconnect_requested = true; true }
            "/help" | "/?" => { self.push_status(StatusKind::Notice(help_lines(self.kitty_keyboard))); true }
            "/clear" => { self.blocks.clear(); self.viewport = Viewport::new(); true }
            _ => false,
        }
    }

    /// Number of transcript rows at the shared content width, used by `Viewport`.
    pub(crate) fn content_rows(&self, content_w: u16) -> usize {
        transcript_lines(self, content_w).len()
    }

    pub(crate) fn apply_stream_idle_timeout(&mut self) {
        self.connection_lost(format!(
            "观察 {}s 无增量；执行状态未知，/reconnect 重连",
            lattice_api::STREAM_IDLE_TIMEOUT_SECS
        ));
    }

    fn connection_lost(&mut self, reason: String) {
        self.connected = false;
        for block in &mut self.blocks {
            if let Block::Tool {
                state: state @ ToolState::Running,
                ..
            } = block
            {
                *state = ToolState::Unknown;
            }
        }
        self.push_status(StatusKind::Notice(vec![reason]));
    }

    fn apply_snapshot(
        &mut self,
        view: SessionObservation,
        blocks: Option<Vec<Block>>,
        owned: bool,
        confirmed: bool,
        error: Option<lattice_api::local::LocalError>,
    ) {
        let body_unavailable = blocks.is_none();
        // 快照本身表明请求的 Turn 已关闭且没有活动 Turn；正文是否可读不改变这一事实。
        let closed = matches!(
            view.requested_turn,
            Some(lattice_api::local::TurnObservation::Committed(_))
        ) && view.active_turn.is_none();
        self.accept_deltas = false;
        let observed = view
            .requested_turn
            .as_ref()
            .map(|turn| match turn {
                lattice_api::local::TurnObservation::Pending(turn) => turn.turn_id,
                lattice_api::local::TurnObservation::Committed(turn) => turn.turn_id,
            })
            .or_else(|| view.active_turn.as_ref().map(|turn| turn.turn_id));
        if observed.is_some() {
            self.observed_turn = observed;
        }
        self.accept_live_events = !matches!(
            view.requested_turn,
            Some(lattice_api::local::TurnObservation::Committed(_))
        );
        if let Some(lattice_api::local::TurnObservation::Committed(turn)) = &view.requested_turn {
            let confirmed = confirmed
                || self
                    .completed_turn
                    .as_ref()
                    .is_some_and(|(known, confirmed)| {
                        known.turn_id == turn.turn_id
                            && known.commit_id == turn.commit_id
                            && *confirmed
                    });
            self.completed_turn = Some((turn.clone(), confirmed));
        }
        self.ctx.active_agent = view.session.active_agent_name;
        if let Some(blocks) = blocks {
            self.blocks = blocks;
            self.durable_len = self.blocks.len();
        }
        if let Some((turn, confirmed)) = &self.completed_turn {
            let mut found = false;
            for block in &mut self.blocks {
                if let Block::Status { kind, .. } = block
                    && matches!(kind, StatusKind::Visible { commit_id } | StatusKind::Committed { commit_id } if *commit_id == turn.commit_id)
                {
                    found = true;
                    if *confirmed {
                        *kind = StatusKind::Committed {
                            commit_id: turn.commit_id.clone(),
                        };
                    }
                }
            }
            if body_unavailable {
                let turn_id = turn.turn_id;
                let outcome = turn.outcome.clone();
                let kind = if *confirmed {
                    StatusKind::Committed {
                        commit_id: turn.commit_id.clone(),
                    }
                } else {
                    StatusKind::Visible {
                        commit_id: turn.commit_id.clone(),
                    }
                };
                if !found {
                    self.push_status(kind);
                }
                let reason = format!("Turn {turn_id}：{outcome:?}");
                self.push_status(match outcome {
                    lattice_api::TurnOutcome::Completed { .. } => StatusKind::Notice(vec![reason]),
                    lattice_api::TurnOutcome::Cancelled { .. } => StatusKind::Cancelled {
                        reason: Some(reason),
                    },
                    lattice_api::TurnOutcome::Failed { .. }
                    | lattice_api::TurnOutcome::Interrupted { .. } => {
                        StatusKind::TurnFailed { reason }
                    }
                });
            }
        }
        self.connected = true;
        // 引用已发布但核心仍收尾时，取消目标仍属于原 Turn。
        self.current_turn = view
            .active_turn
            .as_ref()
            .map(|turn| turn.turn_id)
            .or_else(|| {
                owned
                    .then(|| {
                        view.requested_turn.as_ref().map(|turn| match turn {
                            lattice_api::local::TurnObservation::Pending(turn) => turn.turn_id,
                            lattice_api::local::TurnObservation::Committed(turn) => turn.turn_id,
                        })
                    })
                    .flatten()
            });
        self.phase = if owned {
            TurnPhase::Streaming
        } else {
            TurnPhase::Idle
        };
        if let Some(pending) = view.active_turn {
            let id = pending.turn_id;
            self.push_status(StatusKind::Notice(vec![format!(
                "pending Turn {id}{}",
                if owned {
                    ""
                } else {
                    "；本连接尚未确认 live 所有权；可重连查询或用 session recover 协调"
                }
            )]));
        } else {
            self.submitted = None;
        }
        if self.gap_pending && !body_unavailable {
            self.push_status(StatusKind::Notice(vec![
                "观察存在增量缺口；持久视图已替换，临时增量未重放".into(),
            ]));
        }
        if let Some(message) = self.acceptance_notice.take() {
            self.push_status(StatusKind::Notice(vec![message]));
        }
        if let Some(error) = error {
            self.push_status(StatusKind::Notice(vec![format!(
                "Turn {}：{}",
                self.observed_turn
                    .map_or_else(|| "未知".into(), |id| id.to_string()),
                error.message
            )]));
        }
        if body_unavailable {
            // 正文读取失败只说明持久正文不可用：连接仍在、Turn 是否完成由快照本身决定。
            // 已关闭 Turn 的固定快照没有替换 blocks 时，仍要按终态收口尚在 Running 的工具行。
            if closed {
                self.fail_running_tools("turn ended before tool completion");
            }
            self.push_status(StatusKind::Notice(vec![
                "持久正文不可用；当前显示未验证，/reconnect 重载".into(),
            ]));
        }
    }

    pub(crate) fn fail_running_tools(&mut self, reason: &str) {
        for block in &mut self.blocks {
            if let Block::Tool {
                state: state @ ToolState::Running,
                ..
            } = block
            {
                *state = ToolState::Failed {
                    output_len: 0,
                    error: Some(reason.to_owned()),
                };
            }
        }
    }

    fn finish_stream(&mut self) {
        self.connection_lost("观察连接结束；不代表 Turn 已关闭，/reconnect 重连".into());
    }

    fn expire_ctrl_c_notice(&mut self, now: Instant) -> bool {
        let expired = self
            .last_ctrl_c
            .is_some_and(|then| now.duration_since(then) > CTRL_C_WINDOW);
        if expired {
            self.last_ctrl_c = None;
        }
        expired
    }

    /// 当前 Dock 状态；`blocks` 中至多存在一个 `StatusPlacement::Dock(_)`，
    /// 推导时直接取唯一项，不做 `rev()` 兜底。
    pub(crate) fn dock_status(&self) -> Option<(&StatusKind, DockSeverity)> {
        self.blocks.iter().find_map(|block| match block {
            Block::Status {
                kind,
                placement: StatusPlacement::Dock(severity),
            } => Some((kind, *severity)),
            _ => None,
        })
    }

    /// 直接进 transcript。
    fn push_status(&mut self, kind: StatusKind) {
        self.blocks.push(Block::Status {
            kind,
            placement: StatusPlacement::Transcript,
        });
    }

    /// 先把当前 Dock status 的 placement 改回 `Transcript`，再以
    /// `Dock(severity)` 追加新 status。turnover 全部在内部完成。
    fn push_dock_status(&mut self, kind: StatusKind, severity: DockSeverity) {
        self.release_dock_status();
        self.blocks.push(Block::Status {
            kind,
            placement: StatusPlacement::Dock(severity),
        });
    }

    /// WARN / ERROR 状态下把当前 Dock status 释放回 transcript；它会在
    /// 原本的时间位置出现且只出现一次。无活动状态时是空操作。
    fn release_dock_status(&mut self) {
        for block in &mut self.blocks {
            if let Block::Status {
                placement: placement @ StatusPlacement::Dock(_),
                ..
            } = block
            {
                *placement = StatusPlacement::Transcript;
            }
        }
    }

    fn submit(&mut self) -> Option<String> {
        let raw = self.input.text();
        if raw.trim().is_empty() {
            return None;
        }
        if self.apply_builtin(raw.trim()) {
            self.history.push(raw.trim());
            self.input.clear();
            return None;
        }
        if self.phase != TurnPhase::Idle {
            return None;
        }
        self.history.push(raw.trim());
        self.input.clear();
        let id = lattice_api::local::new_turn_id();
        self.accept_live_events = true;
        self.gap_pending = false;
        self.current_turn = Some(id);
        self.observed_turn = Some(id);
        self.submitted = Some((id, raw.clone()));
        self.blocks
            .push(Block::AgentName(self.ctx.active_agent.clone()));
        self.blocks.push(Block::User(raw.clone()));
        self.push_status(StatusKind::Notice(vec![format!(
            "提交身份 {id}；确认丢失仍按此 ID 找回"
        )]));
        self.phase = TurnPhase::Streaming;
        Some(raw)
    }

    #[rustfmt::skip]
    fn cancel_or_clear(&mut self) {
        if self.current_turn.is_some() {
            self.cancel_requested = true;
            self.phase = TurnPhase::Cancelling;
        } else {
            self.input.clear();
        }
    }

    /// 请求被拒绝或未发出：输入交还用户自行决定，不自动重发，也没有可取消的 Turn。
    fn abandon_submission(&mut self) {
        if let Some((_, prompt)) = self.submitted.take() {
            self.input.set_text(&prompt);
        }
        self.phase = TurnPhase::Idle;
        self.current_turn = None;
    }

    /// 与绘制路径使用同一内容宽度和垂直开销。
    /// Dock::height() 是 Dock 占用的唯一来源；bottomline 只在侧栏隐藏时存在。
    fn transcript_height(&self) -> usize {
        let layout = frame_layout(self.term_width, self.term_height);
        let bottom_h = match layout.sidebar {
            Sidebar::Visible => 0,
            Sidebar::Hidden => 1,
        };
        let dock = Dock::new(self);
        self.term_height
            .saturating_sub(dock.height(layout.content_w) + bottom_h) as usize
    }
}

/// 启动先重载固定历史，再进入 raw 模式；不依赖实时流补回已提交结果。
pub(crate) async fn run_app(
    mut options: TuiOptions,
    ctx: SessionContext,
    client: LocalClient,
) -> anyhow::Result<()> {
    let root = options.project_root.clone();
    options.project_root =
        tokio::task::spawn_blocking(move || std::fs::canonicalize(root)).await??;
    let mut connection = Connection::start(client.clone(), ctx.session_id.clone(), None, None);
    let mut initial_gap = false;
    let (initial_view, initial_blocks, initial_owned, initial_confirmed, initial_error) = loop {
        match tokio::time::timeout(
            Duration::from_secs(lattice_api::STREAM_IDLE_TIMEOUT_SECS),
            connection.next(),
        )
        .await?
        {
            Some(Update::Snapshot {
                view,
                blocks,
                owned,
                confirmed,
                error,
            }) => {
                break (view, blocks, owned, confirmed, error);
            }
            Some(Update::Gap) => initial_gap = true,
            Some(Update::Error(error)) => return Err(error.into()),
            Some(Update::SelectedTurn { .. } | Update::NotSubmitted { .. } | Update::Event(_)) => {}
            None => anyhow::bail!("initial observation ended without a fixed snapshot"),
        }
    };
    let mut terminal = ratatui::try_init()?;
    let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    if kitty {
        crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        )?;
    }
    install_cleanup_panic_hook(kitty);
    let _cleanup = TerminalCleanup { kitty };

    let mut app = App::new(ctx, kitty, &options.session_dir);
    app.gap_pending = initial_gap;
    app.apply_snapshot(
        *initial_view,
        initial_blocks,
        initial_owned,
        initial_confirmed,
        initial_error,
    );
    app.push_status(StatusKind::Notice(startup_notice(&app.ctx)));
    let exit = run_loop(
        &options,
        &mut app,
        &client,
        initial_owned.then_some(connection),
        EventStream::new(),
        |app| {
            if let Ok(size) = terminal.size() {
                app.term_width = size.width;
                app.term_height = size.height;
                let layout = frame_layout(size.width, size.height);
                let th = app.transcript_height();
                let rows = app.content_rows(layout.content_w);
                app.viewport.on_content_changed(rows, th);
            }
            terminal.draw(|f| crate::view::draw(f, app))?;
            Ok(())
        },
    )
    .await;
    // 离开备用屏幕后再有界承接取消意图与在途提交回执；退出不能销毁已经发出的取消，也不能丢弃仍未知的回执。
    drop(_cleanup);
    for line in settle_exit(&client, &app.ctx.session_id, exit.control).await {
        eprintln!("{line}");
    }
    exit.result
}

type CancelTask = tokio::task::JoinHandle<lattice_api::local::Result<bool>>;

/// 退出后继续承接提交回执与取消回执的上限；到期仍未知则带原 ID 报告，不无界挂起。
const EXIT_GRACE_SECS: u64 = 5;

/// 取消意图绑定提交时的 TurnId。Pending 表示已请求但后台尚未确认接管；
/// Moot 表示请求被拒绝、未发出或选定了已关闭的旧 Turn，意图没有对象，不得扩散。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CancelIntent {
    Pending,
    Confirmed,
    Moot,
}

/// 事件循环持有的观察与取消状态；退出后交给 `settle_exit`，不随循环结束丢弃。
struct TurnControl {
    stream: Option<Connection>,
    deadline: Option<tokio::time::Instant>,
    cancel: Option<CancelTask>,
    /// 在途取消发出时提交回执尚未到达：其接管可能属于探测所有者，不能作最终确认。
    provisional: bool,
    intent: Option<(TurnId, CancelIntent)>,
    /// 当前 Connection 的提交回执（SelectedTurn / NotSubmitted / Error）仍在途。
    receipt_pending: bool,
}

impl TurnControl {
    /// 用户请求取消：意图绑定当前 TurnId；无在途取消时立即发一次，
    /// 提交回执未到时同时阻止尚未发出的配置提交。owned=false 不是终结，回执到达后再补发。
    fn request_cancel(&mut self, client: &LocalClient, session_id: &str, turn_id: TurnId) {
        if !matches!(self.intent, Some((id, _)) if id == turn_id) {
            self.intent = Some((turn_id, CancelIntent::Pending));
        }
        if self.cancel.is_none() {
            self.cancel = Some(spawn_cancel(client, session_id, turn_id));
            self.provisional = self.receipt_pending;
        }
        if self.receipt_pending
            && let Some(connection) = &self.stream
        {
            connection.abandon_unsent();
        }
    }

    /// 有 live 证据（接受回执或 owned 快照）时，对同一身份未确认的意图只补发一次。
    /// 返回意图是否仍针对该 Turn。
    fn resend_pending(&mut self, client: &LocalClient, session_id: &str, turn_id: TurnId) -> bool {
        match self.intent {
            Some((id, CancelIntent::Pending)) if id == turn_id => {
                if self.cancel.is_none() {
                    self.cancel = Some(spawn_cancel(client, session_id, turn_id));
                    self.provisional = false;
                }
                true
            }
            Some((id, CancelIntent::Confirmed)) if id == turn_id => true,
            _ => false,
        }
    }

    /// 取消回执：接管只在提交回执已到时算确认；提交回执前发出的取消若已知 live 则补发一次。
    fn settle_cancel(&mut self, client: &LocalClient, session_id: &str, taken: bool) {
        self.cancel = None;
        let provisional = std::mem::take(&mut self.provisional);
        let Some((turn_id, CancelIntent::Pending)) = self.intent else {
            return;
        };
        if provisional {
            if !self.receipt_pending {
                self.resend_pending(client, session_id, turn_id);
            }
        } else if taken {
            self.intent = Some((turn_id, CancelIntent::Confirmed));
        }
    }

    /// 请求被拒绝、未发送或选定了已关闭的旧 Turn：待办意图失去对象。返回其原 TurnId 供提示。
    fn moot(&mut self) -> Option<TurnId> {
        match &mut self.intent {
            Some((turn_id, intent @ CancelIntent::Pending)) => {
                *intent = CancelIntent::Moot;
                Some(*turn_id)
            }
            _ => None,
        }
    }

    /// 意图针对的 Turn 已观察到持久终态：不再有待确认的取消。
    fn resolve_terminal(&mut self, view: &SessionObservation) {
        if let Some(lattice_api::local::TurnObservation::Committed(turn)) = &view.requested_turn
            && matches!(self.intent, Some((id, _)) if id == turn.turn_id)
        {
            self.intent = None;
        }
    }

    fn close_stream(&mut self) {
        self.stream = None;
        self.deadline = None;
        self.receipt_pending = false;
    }
}

struct LoopExit {
    result: anyhow::Result<()>,
    control: TurnControl,
}

fn spawn_cancel(client: &LocalClient, session_id: &str, turn_id: TurnId) -> CancelTask {
    let client = client.clone();
    let session = session_id.to_owned();
    tokio::spawn(async move { client.cancel(&session, turn_id).await })
}

fn rejected(error: &lattice_api::local::LocalError) -> bool {
    matches!(
        error.kind,
        LocalErrorKind::BadRequest
            | LocalErrorKind::NotFound
            | LocalErrorKind::Busy
            | LocalErrorKind::Conflict
            | LocalErrorKind::Stopping
    )
}

fn moot_notice(turn_id: TurnId) -> String {
    format!("本次请求 {turn_id} 未接受或已关闭；取消没有对象，不作用于其他 Turn")
}

fn not_submitted_notice(turn_id: TurnId) -> String {
    format!("请求未接受：中断后不再提交配置，未执行：turn {turn_id}")
}

/// 键盘事件流与绘制回调由调用方注入：产品接 crossterm/ratatui，测试接脚本事件并检查状态。
#[rustfmt::skip]
async fn run_loop<E, D>(
    options: &TuiOptions,
    app: &mut App,
    client: &LocalClient,
    initial: Option<Connection>,
    mut events: E,
    mut draw: D,
) -> LoopExit
where
    E: futures::Stream<Item = std::io::Result<Event>> + Unpin,
    D: FnMut(&mut App) -> anyhow::Result<()>,
{
    let mut control = TurnControl {
        deadline: initial.as_ref().map(|_| idle_deadline()),
        stream: initial,
        cancel: None,
        provisional: false,
        intent: None,
        receipt_pending: false,
    };
    let session = app.ctx.session_id.clone();
    let mut ticker = tokio::time::interval(FRAME);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let result = loop {
        // 取消请求先于退出判定登记：同一轮到达的 Esc 与退出不能让意图丢失。
        if app.cancel_requested {
            app.cancel_requested = false;
            if let Some(id) = app.current_turn {
                control.request_cancel(client, &session, id);
            }
        }
        if app.should_quit {
            break Ok(());
        }
        if app.reconnect_requested {
            app.reconnect_requested = false;
            control.stream = Some(Connection::start(client.clone(), session.clone(), app.current_turn.or(app.observed_turn), None));
            control.deadline = Some(idle_deadline());
            control.receipt_pending = false;
        }
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(ev)) => {
                        if let Some(prompt) = handle_input_event(app, ev) {
                            control.intent = None;
                            begin_turn(options, app, &mut control.stream, client, prompt);
                            control.deadline = control.stream.as_ref().map(|_| idle_deadline());
                            control.receipt_pending = true;
                        }
                    }
                    Some(Err(_)) | None => app.should_quit = true,
                }
                dirty = true;
            }
            receipt = async { control.cancel.as_mut().expect("guarded cancellation").await }, if control.cancel.is_some() => {
                let taken = cancel_taken(&receipt);
                app.push_status(StatusKind::Notice(vec![cancel_message(receipt, control.provisional)]));
                control.settle_cancel(client, &session, taken);
                dirty = true;
            }
            wait = poll_turn(&mut control.stream, control.deadline) => {
                match wait {
                    TurnWait::Event(update) => {
                        control.deadline = Some(idle_deadline());
                        match update {
                            Update::SelectedTurn { turn_id, message, live } => {
                                control.receipt_pending = false;
                                app.current_turn = Some(turn_id);
                                app.acceptance_notice = Some(format!("{message}：{turn_id}"));
                                if live {
                                    if control.resend_pending(client, &session, turn_id) { app.phase = TurnPhase::Cancelling; }
                                } else if let Some(id) = control.moot() {
                                    app.push_status(StatusKind::Notice(vec![moot_notice(id)]));
                                }
                            }
                            Update::NotSubmitted { turn_id } => {
                                control.close_stream();
                                control.moot();
                                app.abandon_submission();
                                app.push_status(StatusKind::Notice(vec![not_submitted_notice(turn_id)]));
                            }
                            Update::Event(event) => { if app.accept_live_events { app.apply_turn_event(event); } }
                            Update::Gap => {
                                app.gap_pending = true;
                                app.accept_deltas = false;
                                app.blocks.truncate(app.durable_len);
                                app.push_status(StatusKind::Notice(vec!["增量缺口；将替换持久视图，不重复追加".into()]));
                            }
                            Update::Snapshot { view, blocks, owned, confirmed, error } => {
                                if !owned { control.resolve_terminal(&view); }
                                app.apply_snapshot(*view, blocks, owned, confirmed, error);
                                if owned
                                    && let Some(id) = app.current_turn
                                    && control.resend_pending(client, &session, id) {
                                        app.phase = TurnPhase::Cancelling;
                                    }
                                if !owned { control.close_stream(); }
                            }
                            Update::Error(error) => {
                                if rejected(&error) {
                                    app.abandon_submission();
                                    if control.receipt_pending
                                        && let Some(id) = control.moot() {
                                            app.push_status(StatusKind::Notice(vec![moot_notice(id)]));
                                        }
                                }
                                control.receipt_pending = false;
                                app.connection_lost(error.message);
                            }
                        }
                    }
                    TurnWait::Ended => {
                        control.close_stream();
                        app.finish_stream();
                    }
                    TurnWait::TimedOut => {
                        control.close_stream();
                        app.apply_stream_idle_timeout();
                    }
                }
                dirty = true;
            }
            _ = ticker.tick() => {
                dirty |= app.expire_ctrl_c_notice(Instant::now());
                if !dirty {
                    continue;
                }
                if let Err(error) = draw(app) {
                    break Err(error);
                }
                dirty = false;
            }
        }
    };
    LoopExit { result, control }
}

/// 界面退出后按原 SessionId/TurnId 有界承接取消意图：在途取消等回执；提交回执未到则继续
/// 轮询同一 Connection，接受后再取消、拒绝或选定旧 Turn 则作废、到期仍未知则如实报告。
/// 无取消意图的退出只断开，不新增取消。返回终端复原后要打印的行。
async fn settle_exit(
    client: &LocalClient,
    session_id: &str,
    mut control: TurnControl,
) -> Vec<String> {
    let mut lines = Vec::new();
    if control.intent.is_none() {
        return lines;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(EXIT_GRACE_SECS);
    while matches!(control.intent, Some((_, CancelIntent::Pending))) {
        if let Some(task) = control.cancel.as_mut() {
            let Ok(receipt) = tokio::time::timeout_at(deadline, task).await else {
                break;
            };
            let taken = cancel_taken(&receipt);
            lines.push(cancel_message(receipt, control.provisional));
            control.settle_cancel(client, session_id, taken);
            continue;
        }
        if !control.receipt_pending {
            break;
        }
        let Some(stream) = control.stream.as_mut() else {
            control.receipt_pending = false;
            continue;
        };
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(Update::SelectedTurn {
                turn_id: selected,
                message,
                live,
            })) => {
                control.receipt_pending = false;
                lines.push(format!("{message}：{selected}"));
                if live {
                    control.resend_pending(client, session_id, selected);
                } else if let Some(id) = control.moot() {
                    lines.push(moot_notice(id));
                }
            }
            Ok(Some(Update::NotSubmitted { turn_id })) => {
                control.close_stream();
                control.moot();
                lines.push(not_submitted_notice(turn_id));
            }
            Ok(Some(Update::Error(error))) => {
                control.receipt_pending = false;
                if rejected(&error)
                    && let Some(id) = control.moot()
                {
                    lines.push(format!("请求 {id} 被拒绝，未执行：{}", error.message));
                } else {
                    lines.push(format!("提交回执失败：{}", error.message));
                }
            }
            Ok(Some(Update::Event(_) | Update::Gap | Update::Snapshot { .. })) => {}
            Ok(None) => control.receipt_pending = false,
            Err(_) => break,
        }
    }
    drop(control.stream);
    match control.intent {
        Some((id, CancelIntent::Pending)) => lines.push(format!(
            "取消未确认：session {session_id} turn {id}；后台可能仍接受或执行该请求，请用同一 TurnId 查询或再次取消"
        )),
        Some((id, CancelIntent::Confirmed)) => lines.push(format!(
            "后台已接管取消：session {session_id} turn {id}；持久终态仍待确认"
        )),
        Some((_, CancelIntent::Moot)) | None => {}
    }
    lines
}

fn cancel_taken(
    receipt: &Result<lattice_api::local::Result<bool>, tokio::task::JoinError>,
) -> bool {
    matches!(receipt, Ok(Ok(true)))
}

/// owned=false 与未知都不是终结：提交回执未到时所有者可能尚未登记，稍后仍会接受同一请求。
fn cancel_message(
    receipt: Result<lattice_api::local::Result<bool>, tokio::task::JoinError>,
    provisional: bool,
) -> String {
    match receipt {
        Ok(Ok(true)) => "后台已接管取消；持久终态仍待确认".into(),
        Ok(Ok(false)) if provisional => {
            "取消早于所有者登记（无 live 所有者）；提交回执到达后将对同一 TurnId 再取消".into()
        }
        Ok(Ok(false)) => "该 Turn 无 live 所有者；没有确认持久 Cancelled，请查询或显式恢复".into(),
        _ => "取消确认未知；请按 TurnId 查询或再次明确取消".into(),
    }
}

enum TurnWait {
    Event(Update),
    Ended,
    TimedOut,
}

/// 观察 idle 使用绝对时刻；重建 select future 不能不断重置倒计时。
fn idle_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(lattice_api::STREAM_IDLE_TIMEOUT_SECS)
}

/// Connection 的 Stream 来自 mpsc recv，键盘/重绘赢得 select 时不会丢半帧。
async fn poll_turn<S: futures::Stream<Item = Update> + Unpin>(
    stream: &mut Option<S>,
    deadline: Option<tokio::time::Instant>,
) -> TurnWait {
    match (stream.as_mut(), deadline) {
        (Some(s), Some(deadline)) => match tokio::time::timeout_at(deadline, s.next()).await {
            Ok(Some(ev)) => TurnWait::Event(ev),
            Ok(None) => TurnWait::Ended,
            Err(_) => TurnWait::TimedOut,
        },
        // A live stream with no deadline cannot happen (both are set together);
        // wait for the stream rather than silently disabling the bound.
        (Some(s), None) => match s.next().await {
            Some(ev) => TurnWait::Event(ev),
            None => TurnWait::Ended,
        },
        (None, _) => std::future::pending().await,
    }
}

#[rustfmt::skip]
fn handle_input_event(app: &mut App, event: Event) -> Option<String> {
    match event {
        Event::Paste(text) => {
            app.apply_action(KeyAction::Paste(text));
            None
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let action = if is_ctrl_c(&key) {
                ctrl_c_action(app)
            } else {
                resolve_key(key, app.kitty_keyboard, app.input.is_empty())
            };
            app.apply_action(action)
        }
        Event::Resize(w, h) => {
            app.term_width = w;
            app.term_height = h;
            None
        }
        _ => None,
    }
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C' | '\u{3}'))
        && (key.modifiers.contains(KeyModifiers::CONTROL) || key.code == KeyCode::Char('\u{3}'))
}

#[rustfmt::skip]
fn ctrl_c_action(app: &mut App) -> KeyAction {
    let now = Instant::now();
    let double = app
        .last_ctrl_c
        .is_some_and(|t| now.duration_since(t) <= CTRL_C_WINDOW);
    app.last_ctrl_c = Some(now);
    if double {
        KeyAction::Quit
    } else {
        KeyAction::CancelOrClear
    }
}

fn begin_turn(
    options: &TuiOptions,
    app: &mut App,
    turn_stream: &mut Option<Connection>,
    client: &LocalClient,
    prompt: String,
) {
    let request = TurnRequest {
        session_id: app.ctx.session_id.clone(),
        turn_id: app.current_turn.expect("submission allocated identity"),
        workspace: options.project_root.clone(),
        input: prompt,
    };
    *turn_stream = Some(Connection::start(
        client.clone(),
        app.ctx.session_id.clone(),
        app.current_turn,
        Some((options.clone(), request)),
    ));
}

#[rustfmt::skip]
fn help_lines(kitty: bool) -> Vec<String> {
    let nl = if kitty { "Shift+Enter  newline" } else { "Alt+Enter  newline" };
    vec![
        "/help  this message".into(),
        "/reconnect  只重连观察，不恢复或重新提交".into(),
        "/clear  clear transcript".into(),
        "/quit  exit (also Ctrl+D on empty input)".into(),
        "Ctrl+C  cancel turn; twice within 2s to quit".into(),
        "Enter  submit".into(),
        nl.into(),
    ]
}

#[rustfmt::skip]
fn startup_notice(ctx: &SessionContext) -> Vec<String> {
    vec![
        "LATTICE".into(),
        format!("session {}", ctx.session_id),
        format!("agent {}", ctx.active_agent),
        format!("project {}", ctx.project_branch),
        "/help  show commands".into(),
        "/clear  clear transcript".into(),
        "/quit  exit".into(),
        "/reconnect  重新读取状态并观察".into(),
    ]
}

#[rustfmt::skip]
fn install_cleanup_panic_hook(kitty: bool) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        if kitty {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        prev(info);
    }));
}

struct TerminalCleanup {
    kitty: bool,
}

#[rustfmt::skip]
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if self.kitty {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

#[cfg(test)]
#[path = "app_loop_tests.rs"]
mod loop_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::resolve_key;
    use lattice_api::TurnEvent;
    use std::time::Duration;

    fn test_app() -> App {
        App::new(
            SessionContext {
                session_id: "sess".into(),
                active_agent: "agent".into(),
                project_branch: "main".into(),
            },
            false,
            // History is not under test here; an empty dir keeps it inert.
            Path::new("/nonexistent-tui-test-history"),
        )
    }

    #[test]
    fn snapshot_replaces_deltas_and_preserves_gap_without_terminal_event() {
        let mut app = test_app();
        let id = lattice_api::local::new_turn_id();
        let view = SessionObservation {
            state_oid: "state".into(),
            session: lattice_api::GraphSession {
                session_id: "session".into(),
                label: None,
                active_agent_name: "agent".into(),
                created_at: String::new(),
                updated_at: String::new(),
                project_target_branch: Some("main".into()),
                project_store_key: "key".into(),
            },
            head: lattice_api::GraphHead::Branch {
                name: "main".into(),
            },
            head_commit_id: Some("commit".into()),
            branches: Vec::new(),
            commit_count: 1,
            active_turn: None,
            requested_turn: Some(lattice_api::local::TurnObservation::Committed(Box::new(
                lattice_api::local::CommittedTurnObservation {
                    commit_id: "commit".into(),
                    turn_id: id,
                    parent_ids: Vec::new(),
                    agent_name: "agent".into(),
                    model: "model".into(),
                    provider: "provider".into(),
                    outcome: lattice_api::TurnOutcome::Completed {
                        finish_reason: lattice_api::TurnFinishReason::Stop,
                    },
                    origin: lattice_api::ClosureOrigin::Live,
                    usage: None,
                    workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
                    created_at: String::new(),
                },
            ))),
        };
        let history = vec![
            Block::User("prompt".into()),
            Block::Assistant("durable".into()),
        ];
        app.blocks.push(Block::Assistant("lost delta".into()));
        app.gap_pending = true;
        app.apply_snapshot(view.clone(), Some(history.clone()), true, false, None);
        assert_eq!(app.phase, TurnPhase::Streaming);
        assert_eq!(app.current_turn, Some(id));
        assert!(!app.accept_live_events);
        assert_eq!(&app.blocks[..2], &history);
        assert!(
            matches!(&app.blocks[2], Block::Status { kind: StatusKind::Notice(lines), .. } if lines[0].contains("缺口"))
        );
        app.apply_snapshot(view.clone(), Some(history.clone()), false, true, None);
        assert_eq!(&app.blocks[..2], &history);
        assert_eq!(app.blocks.len(), 3);
        assert!(
            matches!(&app.blocks[2], Block::Status { kind: StatusKind::Notice(lines), .. } if lines[0].contains("缺口"))
        );
        assert_eq!(app.phase, TurnPhase::Idle);
        assert!(app.current_turn.is_none());
        app.apply_snapshot(view.clone(), None, false, true, None);
        assert_eq!(&app.blocks[..2], &history);
        // 正文不可用不是断开：连接状态与正文可用性分开呈现。
        assert!(app.connected);
        assert!(!app.accept_live_events);
        assert_eq!(app.phase, TurnPhase::Idle);
        assert!(app.current_turn.is_none());
        assert!(
            matches!(app.blocks.last(), Some(Block::Status { kind: StatusKind::Notice(lines), .. }) if lines[0].contains("正文不可用"))
        );
        let visible = Block::Status {
            kind: StatusKind::Visible {
                commit_id: "commit".into(),
            },
            placement: StatusPlacement::Transcript,
        };
        for message in [
            "original durability failure",
            "durability confirmation unknown",
        ] {
            app = test_app();
            let error = lattice_api::local::LocalError {
                kind: LocalErrorKind::StoreUnavailable,
                message: message.into(),
                recovery: None,
            };
            app.apply_snapshot(
                view.clone(),
                Some(vec![visible.clone()]),
                false,
                false,
                Some(error),
            );
            assert_eq!(app.observed_turn, Some(id));
            let text = transcript_text(&app, 120);
            assert!(
                text.contains(message)
                    && text.contains("结果可见")
                    && text.contains("完成确认未知"),
                "{text}"
            );
            assert!(!text.contains("committed"), "{text}");
            assert!(app.apply_builtin("/reconnect"));
            assert_eq!(app.observed_turn, Some(id));
            app.apply_snapshot(
                view.clone(),
                None,
                false,
                true,
                Some(lattice_api::local::LocalError {
                    kind: LocalErrorKind::StoreUnavailable,
                    message: "persistent history read failed".into(),
                    recovery: None,
                }),
            );
            let text = transcript_text(&app, 200);
            assert!(text.contains("committed commit"), "{text}");
            assert!(!text.contains("完成确认未知"), "{text}");
            assert!(
                text.contains("Completed") && text.contains(&id.to_string()),
                "{text}"
            );
            assert!(
                text.contains("persistent history read failed") && text.contains("正文不可用"),
                "{text}"
            );
            app.connection_lost("connection reset".into());
            assert!(transcript_text(&app, 200).contains("committed commit"));
        }
        for value in [
            serde_json::json!({"failed": {"reason": "model_request_failed"}}),
            serde_json::json!({"cancelled": {"reason": "user_cancelled"}}),
            serde_json::json!({"interrupted": {"reason": "process_interrupted_before_model_attempt"}}),
        ] {
            let outcome: lattice_api::TurnOutcome = serde_json::from_value(value).unwrap();
            for confirmed in [false, true] {
                let mut app = test_app();
                let mut terminal = view.clone();
                if let Some(lattice_api::local::TurnObservation::Committed(turn)) =
                    &mut terminal.requested_turn
                {
                    turn.outcome = outcome.clone();
                }
                app.apply_snapshot(
                    terminal,
                    None,
                    false,
                    confirmed,
                    Some(lattice_api::local::LocalError {
                        kind: LocalErrorKind::StoreUnavailable,
                        message: "persistent history read failed".into(),
                        recovery: None,
                    }),
                );
                app.connection_lost("connection reset".into());
                let text = transcript_text(&app, 200);
                assert!(text.contains(&format!("{outcome:?}")), "{text}");
                assert!(
                    text.contains(&id.to_string()) && text.contains("commit"),
                    "{text}"
                );
                assert_eq!(text.contains("committed commit"), confirmed, "{text}");
                assert_eq!(text.contains("完成确认未知"), !confirmed, "{text}");
                assert!(
                    text.contains("persistent history read failed")
                        && text.contains("正文不可用")
                        && text.contains("connection reset"),
                    "{text}"
                );
                assert!(app.blocks.iter().any(|block| matches!(block,
                    Block::Status { kind: StatusKind::TurnFailed { .. }, .. }
                        if matches!(outcome, lattice_api::TurnOutcome::Failed { .. } | lattice_api::TurnOutcome::Interrupted { .. })
                ) || matches!(block,
                    Block::Status { kind: StatusKind::Cancelled { .. }, .. }
                        if matches!(outcome, lattice_api::TurnOutcome::Cancelled { .. })
                )), "terminal outcome must retain its display category");
                assert_eq!(app.observed_turn, Some(id));
                assert_eq!(app.phase, TurnPhase::Idle);
            }
        }
        let mut pending = view.clone();
        pending.active_turn = Some(lattice_api::local::PendingObservation {
            turn_id: id,
            journal_oid: "journal".into(),
        });
        pending.requested_turn = pending
            .active_turn
            .clone()
            .map(lattice_api::local::TurnObservation::Pending);
        app.apply_snapshot(pending, Some(history.clone()), true, false, None);
        app.apply_turn_event(TurnEvent::TokenDelta {
            content: "uncertain suffix".into(),
        });
        assert_eq!(
            &app.blocks[..2],
            &history,
            "reload must not append an unproven segment"
        );
        assert!(!transcript_text(&app, 120).contains("uncertain suffix"));
        app.apply_turn_event(TurnEvent::ModelStarted {
            provider: None,
            model: "next".into(),
        });
        app.apply_turn_event(TurnEvent::TokenDelta {
            content: "next segment".into(),
        });
        assert_eq!(&app.blocks[..2], &history);
        assert!(
            matches!(app.blocks.last(), Some(Block::Assistant(text)) if text == "next segment")
        );
        app.apply_snapshot(view, Some(vec![visible]), false, true, None);
        assert!(transcript_text(&app, 120).contains("committed commit"));
    }

    fn assert_failed_tool(app: &App, index: usize, reason: &str) {
        match &app.blocks[index] {
            Block::Tool {
                state: ToolState::Failed { output_len, error },
                ..
            } => {
                assert_eq!(*output_len, 0);
                assert!(error.as_deref().is_some_and(|error| error.contains(reason)));
            }
            other => panic!("expected failed tool, got {other:?}"),
        }
    }

    fn start_tool(app: &mut App, name: &str) {
        app.apply_turn_event(TurnEvent::ToolCallStarted {
            id: "call-1".into(),
            name: name.into(),
        });
    }
    #[test]
    fn token_deltas_coalesce_into_one_assistant_block() {
        let mut app = test_app();
        app.apply_turn_event(TurnEvent::TokenDelta {
            content: "a".into(),
        });
        app.apply_turn_event(TurnEvent::TokenDelta {
            content: "b".into(),
        });
        assert_eq!(app.blocks, vec![Block::Assistant("ab".into())]);
    }

    #[test]
    fn tool_finished_updates_the_block_with_matching_id() {
        let mut app = test_app();
        // Three concurrent tools: with only one in flight, "updates the block
        // with the matching id" would pass even if the code updated any block.
        for (id, name) in [
            ("call-1", "observe.workspace"),
            ("call-2", "mutate.edit"),
            ("call-3", "fetch.web"),
        ] {
            app.apply_turn_event(TurnEvent::ToolCallStarted {
                id: id.into(),
                name: name.into(),
            });
        }
        app.apply_turn_event(TurnEvent::ToolCallFinished {
            id: "call-2".into(),
            name: "mutate.edit".into(),
            ok: false,
            output_len: 0,
            error_summary: Some("precondition failed".into()),
        });

        let states: Vec<(&str, &ToolState)> = app
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Tool { id, state, .. } => Some((id.as_str(), state)),
                _ => None,
            })
            .collect();
        assert_eq!(states.len(), 3, "every started tool keeps its own block");
        for (id, state) in states {
            match (id, state) {
                ("call-2", ToolState::Failed { output_len, error }) => {
                    assert_eq!(*output_len, 0);
                    assert_eq!(error.as_deref(), Some("precondition failed"));
                }
                ("call-2", other) => panic!("call-2 should be Failed, got {other:?}"),
                (other_id, ToolState::Running) => {
                    assert!(matches!(other_id, "call-1" | "call-3"));
                }
                (other_id, other) => {
                    panic!("{other_id} must stay Running, got {other:?}")
                }
            }
        }
    }

    #[test]
    fn terminal_event_closes_running_tools() {
        let mut app = test_app();
        start_tool(&mut app, "mutate.edit");
        app.apply_turn_event(TurnEvent::TurnFailed {
            reason: "provider failed".into(),
        });

        assert_failed_tool(&app, 0, "provider failed");
    }

    #[test]
    fn stream_end_keeps_execution_state_and_marks_observation_unknown() {
        let mut app = test_app();
        app.phase = TurnPhase::Streaming;
        let id = lattice_api::local::new_turn_id();
        app.current_turn = Some(id);
        start_tool(&mut app, "mutate.edit");
        app.finish_stream();
        assert_eq!(app.phase, TurnPhase::Streaming);
        assert_eq!(app.current_turn, Some(id));
        assert!(!app.connected);
        assert!(matches!(
            &app.blocks[0],
            Block::Tool {
                state: ToolState::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn esc_targets_the_current_turn_without_claiming_durable_cancellation() {
        let mut app = test_app();
        let id = lattice_api::local::new_turn_id();
        app.current_turn = Some(id);
        app.phase = TurnPhase::Streaming;
        app.apply_action(KeyAction::CancelOrClear);
        assert!(app.cancel_requested);
        assert_eq!(app.current_turn, Some(id));
        assert_eq!(app.phase, TurnPhase::Cancelling);
        app.apply_action(KeyAction::Quit);
        assert!(app.cancel_requested);
        assert!(!app.blocks.iter().any(|block| matches!(
            block,
            Block::Status {
                kind: StatusKind::Cancelled { .. },
                ..
            }
        )));
    }

    #[test]
    fn esc_when_idle_clears_input() {
        let mut app = test_app();
        app.input.set_text("hello");
        app.apply_action(KeyAction::CancelOrClear);
        assert!(app.input.is_empty());
        assert_eq!(app.phase, TurnPhase::Idle);
    }

    #[test]
    fn paste_grows_input_without_submitting() {
        let mut app = test_app();
        let submitted = app.apply_action(KeyAction::Paste("a\nb\nc".into()));
        assert!(submitted.is_none());
        assert_eq!(app.input.text(), "a\nb\nc");
        // 3 行内容 + rail：Dock::height 是视口高度的唯一来源。
        assert_eq!(Dock::new(&app).height(frame_layout(80, 24).content_w), 4);
        assert_eq!(app.phase, TurnPhase::Idle);
    }

    #[test]
    fn double_ctrl_c_within_window_quits_single_does_not() {
        let mut app = test_app();
        assert!(!matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::CancelOrClear);
        assert!(!app.should_quit);
        assert!(matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::Quit);
        assert!(app.should_quit);

        let mut app = test_app();
        ctrl_c_action(&mut app);
        app.last_ctrl_c = Some(Instant::now() - Duration::from_secs(3));
        assert!(!matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::CancelOrClear);
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_notice_expiry_requests_redraw_once() {
        let mut app = test_app();
        let now = Instant::now();
        app.last_ctrl_c = Some(now - CTRL_C_WINDOW - Duration::from_millis(1));
        assert!(app.expire_ctrl_c_notice(now));
        assert!(app.last_ctrl_c.is_none());
        assert!(!app.expire_ctrl_c_notice(now));
    }

    #[test]
    fn ctrl_d_quits_only_on_empty_buffer() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let empty = resolve_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            false,
            true,
        );
        let full = resolve_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            false,
            false,
        );
        assert!(matches!(empty, KeyAction::Quit));
        assert!(matches!(full, KeyAction::Ignored));
    }

    #[test]
    fn builtin_clear_empties_blocks_and_resets_viewport() {
        let mut app = test_app();
        app.blocks.push(Block::User("x".into()));
        app.viewport.on_content_changed(20, 5);
        app.viewport.page_up(5);
        assert!(!app.viewport.following());
        assert!(app.apply_builtin("/clear"));
        assert!(app.blocks.is_empty());
        assert!(app.viewport.following());
        assert_eq!(app.viewport.scroll(), 0);
    }

    #[test]
    fn idle_stream_timeout_keeps_execution_unconfirmed() {
        let mut app = test_app();
        app.phase = TurnPhase::Streaming;
        start_tool(&mut app, "fetch.web");
        app.apply_stream_idle_timeout();
        assert_eq!(app.phase, TurnPhase::Streaming);
        assert!(!app.blocks.iter().any(|block| matches!(
            block,
            Block::Status {
                kind: StatusKind::TurnFailed { .. },
                ..
            }
        )));
    }

    /// §19.2：Warn / Error 的 Dock 生命周期与 transcript 时间顺序。
    /// 释放后的状态必须回到它原本的时间位置，且只出现一次。
    #[test]
    fn dock_lifecycle_keeps_transcript_order_and_draft() {
        let mut app = test_app();
        app.input.set_text("draft");
        // 初始为 Prompt。
        assert!(app.dock_status().is_none());

        // LlmRetry A → 只在 WARN Dock，transcript 暂不显示。
        app.apply_turn_event(TurnEvent::LlmRequestError {
            provider: None,
            model: "mock".into(),
            attempt: 1,
            max_attempts: 3,
            error_kind: "network".into(),
            retryable: true,
            will_retry: true,
            delay_ms: Some(100),
            status_code: None,
            message: "retry-a".into(),
        });
        let (kind, severity) = app.dock_status().expect("retry 应暂驻 Dock");
        assert!(matches!(kind, StatusKind::LlmRetry { message, .. } if message == "retry-a"));
        assert_eq!(severity, DockSeverity::Warn);
        assert!(!transcript_text(&app, 78).contains("retry-a"));

        // INFO 类状态直接进 transcript，Dock 不受影响。
        app.apply_turn_event(TurnEvent::TurnCancelled {
            reason: Some("info-i".into()),
        });
        assert!(transcript_text(&app, 78).contains("info-i"));
        assert!(matches!(
            app.dock_status(),
            Some((StatusKind::LlmRetry { .. }, _))
        ));

        // CommitFailed B → turnover：A 释放到 transcript，B 留在 ERROR Dock。
        app.apply_turn_event(TurnEvent::CommitFailed {
            reason: "commit-b".into(),
        });
        let (kind, severity) = app.dock_status().expect("失败应暂驻 Dock");
        assert!(matches!(kind, StatusKind::CommitFailed { reason } if reason == "commit-b"));
        assert_eq!(severity, DockSeverity::Error);
        let text = transcript_text(&app, 78);
        assert!(text.contains("retry-a"), "释放的 warn 保持原时间位置");
        assert!(!text.contains("commit-b"), "新错误留在 Dock");

        // Space：B 释放，Dock 回 Prompt，Space 不进入 InputBuffer，草稿不变。
        let submitted = app.apply_action(KeyAction::Insert(' '));
        assert!(submitted.is_none());
        assert!(app.dock_status().is_none());
        assert_eq!(app.input.text(), "draft");
        assert_eq!(app.phase, TurnPhase::Idle);

        // 最终 transcript 顺序仍为 A, I, B。
        let text = transcript_text(&app, 78);
        assert!(text.contains("retry-a"));
        assert!(text.contains("commit-b"));
        let order: Vec<usize> = ["retry-a", "info-i", "commit-b"]
            .into_iter()
            .map(|marker| {
                transcript_lines(&app, 78)
                    .iter()
                    .position(|line| line.to_string().contains(marker))
                    .unwrap_or_else(|| panic!("缺少 {marker}"))
            })
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "transcript 顺序必须保持 A, I, B");

        // 不变量：blocks 中至多一个 Dock placement。
        assert_eq!(dock_placement_count(&app), 0);
    }

    /// 同级替换 WARN A -> WARN B：旧状态在 turnover 时进入 transcript。
    #[test]
    fn dock_turnover_replaces_same_severity_without_losing_the_old() {
        let mut app = test_app();
        for message in ["retry-a", "retry-b"] {
            app.apply_turn_event(TurnEvent::LlmRequestError {
                provider: None,
                model: "mock".into(),
                attempt: 1,
                max_attempts: 3,
                error_kind: "network".into(),
                retryable: true,
                will_retry: true,
                delay_ms: Some(0),
                status_code: None,
                message: message.into(),
            });
        }
        let (kind, severity) = app.dock_status().expect("最新 warn 暂驻 Dock");
        assert!(matches!(kind, StatusKind::LlmRetry { message, .. } if message == "retry-b"));
        assert_eq!(severity, DockSeverity::Warn);
        let text = transcript_text(&app, 78);
        assert!(text.contains("retry-a"), "旧 warn 已原位释放");
        assert!(!text.contains("retry-b"));
        assert_eq!(
            dock_placement_count(&app),
            1,
            "不变量：至多一个 Dock placement"
        );

        // ERROR A -> WARN B 同理。
        app.apply_turn_event(TurnEvent::TurnFailed {
            reason: "turn-failed-a".into(),
        });
        app.apply_turn_event(TurnEvent::LlmRequestError {
            provider: None,
            model: "mock".into(),
            attempt: 2,
            max_attempts: 3,
            error_kind: "network".into(),
            retryable: true,
            will_retry: true,
            delay_ms: Some(0),
            status_code: None,
            message: "retry-c".into(),
        });
        assert!(matches!(
            app.dock_status(),
            Some((StatusKind::LlmRetry { message, .. }, DockSeverity::Warn)) if message == "retry-c"
        ));
        let text = transcript_text(&app, 78);
        assert!(text.contains("turn-failed-a"));
        assert!(text.contains("retry-a") && text.contains("retry-b"));
        assert_eq!(dock_placement_count(&app), 1);
    }

    /// §14.3：WARN / ERROR 下屏蔽不可见输入编辑，全局动作照常。
    #[test]
    fn dock_status_blocks_invisible_input_but_keeps_globals() {
        let mut app = test_app();
        app.input.set_text("draft");
        app.apply_turn_event(TurnEvent::TurnFailed {
            reason: "blocked".into(),
        });

        // 普通字符、退格、提交都不改动不可见 InputBuffer。
        assert!(app.apply_action(KeyAction::Insert('x')).is_none());
        assert!(app.apply_action(KeyAction::Backspace).is_none());
        assert!(app.apply_action(KeyAction::Submit).is_none());
        assert_eq!(app.input.text(), "draft");

        // 无修饰 Space 释放后，输入恢复正常。
        app.apply_action(KeyAction::Insert(' '));
        assert!(app.dock_status().is_none());
        assert!(app.apply_action(KeyAction::Insert('x')).is_none());
        assert_eq!(app.input.text(), "draftx");

        // Prompt 态的 Space 保持普通输入行为。
        app.apply_action(KeyAction::Insert(' '));
        assert_eq!(app.input.text(), "draftx ");
    }

    fn transcript_text(app: &App, width: u16) -> String {
        transcript_lines(app, width)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn dock_placement_count(app: &App) -> usize {
        app.blocks
            .iter()
            .filter(|block| {
                matches!(
                    block,
                    Block::Status {
                        placement: StatusPlacement::Dock(_),
                        ..
                    }
                )
            })
            .count()
    }

    /// The regression guard for the idle bound itself: `poll_turn` is rebuilt
    /// on every `select!` iteration (the 33 ms frame ticker wins constantly),
    /// so a *relative* `timeout(...)` restarts its countdown ~30x a second and
    /// can never elapse. Rebuild the future repeatedly against a stream that
    /// never yields and require the absolute deadline to still fire.
    #[tokio::test(start_paused = true)]
    async fn idle_deadline_survives_repeated_poll_rebuilds() {
        let mut stream = Some(futures::stream::pending::<Update>());
        let deadline = idle_deadline();

        // Two thirds of the window, consumed as many short frame-tick races.
        let ticks = (lattice_api::STREAM_IDLE_TIMEOUT_SECS * 2 / 3).max(1);
        for _ in 0..ticks {
            tokio::select! {
                wait = poll_turn(&mut stream, Some(deadline)) => {
                    assert!(
                        !matches!(wait, TurnWait::TimedOut),
                        "idle bound fired before the deadline"
                    );
                }
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }

        // Still inside the window: rebuilding must not have reset it.
        assert!(
            tokio::time::Instant::now() < deadline,
            "test consumed the whole window before asserting"
        );
        assert!(
            matches!(
                poll_turn(&mut stream, Some(deadline)).await,
                TurnWait::TimedOut
            ),
            "absolute deadline must elapse despite repeated rebuilds"
        );
        // The load-bearing assertion: it fired *at* the original deadline. A
        // relative `timeout(...)` also eventually fires, but only after a
        // fresh full window from the last rebuild — which is exactly the bug.
        let fired_at = tokio::time::Instant::now();
        assert!(
            fired_at.duration_since(deadline) < Duration::from_secs(1),
            "deadline was reset by rebuilding: fired {:?} late",
            fired_at.duration_since(deadline)
        );
    }
}
