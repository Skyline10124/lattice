// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use crate::TuiOptions;
use crate::blocks::{BlockId, BlockList, IdentifiedBlock};
use crate::connection::{
    KeptInput, Viewed, cancellation_status, detach_lines, discarded_comments_status,
    execution_diagnostic_status, handoff_status, not_accepted_status, not_submitted_status,
    observation_lost_status, older_turns_unavailable_status, orphaned_turn_status,
    recovered_status, rejected_status, submission_unknown_status,
};
use crate::input::{CTRL_C_WINDOW, History, InputBuffer, KeyAction, resolve_key};
use crate::session::{Generating, SessionContext};
use crate::view::{
    Dock, DockIntent, DockState, Focus, Panel, Transcript, Viewport, frame_layout,
    render_transcript,
};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use lattice_api::UsageSummary;
use lattice_api::local::{
    CancelStatus, ConnectionStatus, ConversationEntry, DetachReport, LocalClient, LocalError,
    LocalErrorKind, LocalHistory, LocalTurn, LocalUpdate, Node, NodeId, NoticeLevel,
    PublicTurnOutcome, SessionHead, SessionId, StepKey, SubmissionReceipt, SubmissionResult,
    SubmissionUpdate, SubmitSession, TextSegment, TextUpdate, TurnId, TurnRequest, ViewUpdate,
};
use tokio::time::Instant;

const FRAME: Duration = Duration::from_millis(33);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    /// 一轮的开头，不占行：UserMessage 条目，或本客户端尚未被条目取代的提交。
    /// `node` 与 `ordinal` 随条目到达；`agent` 是这一轮回答的 agent。
    UserTurn {
        turn_id: TurnId,
        node: Option<NodeId>,
        ordinal: Option<std::num::NonZeroUsize>,
        agent: String,
    },
    /// Step 条目的开头，不占行。
    Step {
        node: NodeId,
        key: StepKey,
    },
    User(String),
    CommentedUser(crate::comment::Message),
    /// 保留被拒提交的标记，但不重复挂载已归还的评论。
    RejectedUser(crate::comment::Message),
    HistoryAssistant(crate::comment::HistoryText),
    /// 本客户端的提交已被接受、条目尚未到达：先显示助手标题线，不伪造正文。
    AssistantStart,
    /// 生成中的文字，或由相邻多段拼成、不能评论的回复正文。
    Assistant(String),
    Tool {
        id: String,
        name: String,
        state: ToolState,
    },
    Status(Status),
}

/// 状态消息只描述自己；是否暂驻 Dock 由 Dock 按等级决定，消息上没有位置字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) level: NoticeLevel,
    /// 可多行，以换行分隔。
    pub(crate) text: String,
    /// 仅 Turn 结局携带所属 Turn 与结局：它属于这一轮最后一个 Step 条目，并决定符号与配色。
    pub(crate) closing: Option<(TurnId, PublicTurnOutcome)>,
    /// 仅重试提示携带它所属的尝试：它属于这个 Step 条目。
    pub(crate) retry: Option<lattice_api::local::ModelAttemptKey>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnPhase {
    Idle,
    Streaming,
    /// 取消请求已发起，接管确认与持久终态分别观察。
    Cancelling,
}

/// 结果未知的本客户端提交；确定未被接受时据此决定原输入的去向。
enum UnknownSubmission {
    /// 手动提交的原文：与直接被拒一样，输入区为空时放回。
    Manual(crate::queue::Outgoing),
    /// 自动发送的一条已让出排队名额，不再放回队首，免得队列超过上限；原文只在输入历史中。
    Automatic,
}

/// 首次 Ctrl+C 取消的那一轮。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CancelTarget {
    /// 本客户端的提交，经它的 LocalTurn 登记取消意图；尚未发出时不再发出。
    Own,
    /// 其他客户端提交、会话头所示正在执行的一轮，按 TurnId 直接取消。
    Other(TurnId),
}

/// 往上翻到已装入的最早一轮时取更早的轮；`epoch` 是发出时的视图代次。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OlderRequest {
    pub(crate) session_id: SessionId,
    pub(crate) before: NodeId,
    pub(crate) epoch: u64,
}

pub(crate) struct App {
    pub(crate) ctx: SessionContext,
    pub(crate) blocks: BlockList,
    /// 启动横幅固定在对话记录最上方。它不是状态消息，也不在 `blocks` 中，
    /// 条目替换与 Dock 都碰不到它；身份只供浏览光标锚定。
    pub(crate) banner: Option<(BlockId, Vec<String>)>,
    pub(crate) markdown_cache: std::cell::RefCell<crate::markdown::RenderCache>,
    pub(crate) focus: Focus,
    pub(crate) dock: DockState,
    pub(crate) input: InputBuffer,
    pub(crate) queue: crate::queue::Queue,
    auto_sent_after: HashSet<TurnId>,
    pub(crate) history: History,
    pub(crate) viewport: Viewport,
    pub(crate) phase: TurnPhase,
    pub(crate) connected: bool,
    pub(crate) model: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) turn_usage: Option<UsageSummary>,
    pub(crate) context_tokens: Option<u64>,
    /// 由应用循环取走并执行的请求。
    pub(crate) cancel_requested: Option<CancelTarget>,
    pub(crate) view_wanted: bool,
    pub(crate) older_request: Option<OlderRequest>,
    pub(crate) recover_request: Option<TurnId>,
    cancelling: bool,
    /// 已确认的会话头：它当前位置的条目已经到达。各轮是否结束与正在执行的一轮只按它判定；
    /// 视图开始前为空。
    pub(crate) head: Option<SessionHead>,
    /// 刚到达、当前位置的条目尚未到达的会话头。后台在同一次推送里紧接着给出那个条目，
    /// 收到之前不拿新会话头配旧条目。
    pending_head: Option<SessionHead>,
    /// 正在显示的条目，按 NodeId 维护；块序列中的段由它们生成。
    entries: BTreeMap<NodeId, ConversationEntry>,
    /// 生成中的文字，按所属 Step 节点。
    texts: HashMap<NodeId, Generating>,
    /// 已经结束的轮；由未结束变为结束的那一刻，结局才是新到的消息。
    ended: HashSet<TurnId>,
    /// 新视图开始后、首条会话头到达前为真：那条会话头之后是初始条目。
    view_starting: bool,
    /// 新视图的首条会话头已到、首个初始条目未到：重连失败时已显示的内容保留，首个条目到达才替换。
    replace_pending: bool,
    /// 新视图或路径替换的首条会话头到它当前位置的条目为止：其间的条目是初始条目，
    /// 带出的提示与结局不是新消息。
    initial: bool,
    /// 视图代次：新视图开始或路径替换时递增，迟到的历史查询结果据此丢弃。
    epoch: u64,
    older_pending: bool,
    orphan_warned: Option<TurnId>,
    diagnostic: Option<LocalError>,
    /// 本客户端尚未得到结果的提交。
    submitted: Option<(TurnId, crate::queue::Outgoing)>,
    /// 本客户端最近一次提交；会话头所示正在执行的是它时，取消经它的 LocalTurn。
    own_turn: Option<TurnId>,
    /// 已被接受、会话头尚未指向它的那一轮：其间仍按正在运行处理。
    awaiting_head: Option<TurnId>,
    /// 新视图开始时已在等会话头的那一轮。它的接受先于这次 `watch`，新视图的初始内容读于接受之后；
    /// 初始条目给完仍没有到达它，它已不在执行，不再等。
    awaiting_before_view: Option<TurnId>,
    /// 结果未知的那次提交，按 TurnId 留到收到它的确定结果为止。
    unknown_submission: Option<(TurnId, UnknownSubmission)>,
    /// 新会话首次提交时生成的 SessionId；确认被接受前之后的提交沿用它与 `New`。
    new_session: Option<SessionId>,
    /// 新会话下结果未知的提交；后台答复会话已存在时按它找回。
    unconfirmed: Option<TurnId>,
    /// 已实时显示的提交回执：请求与结果种类。交接报告只省略相同的事实。
    shown_receipts: HashSet<(TurnId, std::mem::Discriminant<SubmissionResult>)>,
    pub(crate) cancel_status: Option<CancelStatus>,
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
            view_wanted: ctx.session_id.is_some(),
            ctx,
            blocks: BlockList::default(),
            banner: None,
            markdown_cache: Default::default(),
            focus: Focus::Dock,
            dock: DockState::default(),
            input: InputBuffer::new(),
            queue: Default::default(),
            auto_sent_after: Default::default(),
            history: History::load(session_dir),
            viewport: Viewport::new(),
            phase: TurnPhase::Idle,
            connected: true,
            model: None,
            provider: None,
            turn_usage: None,
            context_tokens: None,
            cancel_requested: None,
            older_request: None,
            recover_request: None,
            cancelling: false,
            head: None,
            pending_head: None,
            entries: BTreeMap::new(),
            texts: HashMap::new(),
            ended: HashSet::new(),
            view_starting: false,
            replace_pending: false,
            initial: false,
            epoch: 0,
            older_pending: false,
            orphan_warned: None,
            diagnostic: None,
            submitted: None,
            own_turn: None,
            awaiting_head: None,
            awaiting_before_view: None,
            unknown_submission: None,
            new_session: None,
            unconfirmed: None,
            shown_receipts: Default::default(),
            cancel_status: None,
            kitty_keyboard,
            last_ctrl_c: None,
            should_quit: false,
            term_width: 80,
            term_height: 24,
        }
    }

    /// 保留键先执行；其余动作交给 Dock 栈顶解释，模块不接触 LocalTurn。
    pub(crate) fn apply_action(&mut self, action: KeyAction) -> Option<String> {
        let width = frame_layout(self.term_width, self.term_height).content_w;
        let transcript = self.prepare_transcript();
        let lines = &transcript.lines;
        match action {
            KeyAction::ScrollPageUp | KeyAction::ScrollPageDown | KeyAction::ScrollToBottom => {
                let height = self.transcript_height(lines.len());
                if width == 0 || height == 0 {
                    if !self.queue.is_editing() {
                        self.focus_input(lines.len());
                    }
                    return None;
                }
                match action {
                    KeyAction::ScrollPageUp => {
                        self.viewport.page_up(height);
                        self.request_older_at_top();
                    }
                    KeyAction::ScrollPageDown => self.viewport.page_down(lines.len(), height),
                    _ => self.viewport.to_bottom(lines.len(), height),
                }
                if let Focus::Transcript(cursor) = &mut self.focus {
                    if !matches!(action, KeyAction::ScrollToBottom) {
                        self.viewport.pause();
                    }
                    if !self.queue.is_editing() {
                        self.viewport.clamp_cursor(cursor);
                        cursor.reanchor(&transcript);
                    }
                }
            }
            KeyAction::CancelOrClear => self.cancel_or_clear(),
            KeyAction::Quit => self.should_quit = true,
            action if self.queue.is_editing() => {
                match action {
                    KeyAction::Submit | KeyAction::Clear => {
                        if let Some(anchor) =
                            self.queue.finish_edit(matches!(action, KeyAction::Submit))
                        {
                            self.focus_anchor(&anchor);
                        }
                        return self.auto_submit();
                    }
                    action => self.queue.edit(&action, transcript.editor_width(width)),
                }
                let transcript = self.prepare_transcript();
                if let Some((row, _)) = transcript.editor_cursor {
                    self.viewport.reveal(row);
                }
            }
            // 焦点在对话记录时暂驻消息不阻断其余按键，但无修饰 Space 仍释放它。
            KeyAction::Insert(' ') if self.dock.message_id().is_some() => self.dock.release(),
            action if matches!(self.focus, Focus::Transcript(_)) => match action {
                KeyAction::Submit => {
                    if let Focus::Transcript(cursor) = self.focus {
                        match transcript.comment_navigation(cursor, width) {
                            Some(crate::view::CommentNavigation::Jump(target)) => {
                                self.focus = Focus::Transcript(target);
                                self.viewport.reveal(target.row);
                            }
                            Some(crate::view::CommentNavigation::Compose(anchor)) => {
                                let id = self.blocks.allocate_id();
                                self.queue.start_comment(id, anchor, &self.blocks);
                            }
                            None => {
                                self.queue.start_edit(cursor.block.id);
                            }
                        }
                    }
                    let transcript = self.prepare_transcript();
                    if let Some((row, _)) = transcript.editor_cursor {
                        self.viewport.reveal(row);
                    }
                }
                // 不在编辑时 Esc 只回到输入区；删除草稿或排队消息用 Delete，避免误删。
                KeyAction::Clear => self.focus_input(lines.len()),
                KeyAction::Delete => {
                    if let Focus::Transcript(cursor) = self.focus {
                        if let Some(anchor) = self.queue.remove_draft(cursor.block.id) {
                            self.focus_anchor(&anchor);
                        } else if self.queue.remove(cursor.block.id) {
                            self.prepare_transcript();
                        }
                    }
                }
                // 对话记录中的可打印字符与粘贴被忽略，焦点不动，避免误输入。
                KeyAction::Insert(_) | KeyAction::ModifiedSpace | KeyAction::Paste(_) => {}
                action @ (KeyAction::Up
                | KeyAction::Down
                | KeyAction::Left
                | KeyAction::Right
                | KeyAction::Home
                | KeyAction::End) => {
                    if let Focus::Transcript(cursor) = &mut self.focus {
                        if matches!(action, KeyAction::Down)
                            && cursor.row >= lines.len().saturating_sub(1)
                        {
                            self.focus_input(lines.len());
                        } else {
                            cursor.move_in(&action, lines, width);
                            cursor.reanchor(&transcript);
                            self.viewport.reveal(cursor.row);
                            if matches!(action, KeyAction::Up) {
                                self.request_older_at_top();
                            }
                        }
                    }
                }
                _ => {}
            },
            action => match Dock::new(self).route(action) {
                DockIntent::Submit => return self.submit(),
                DockIntent::ReleaseStatus => self.dock.release(),
                DockIntent::ClosePanel => self.dock.close_panel(),
                DockIntent::None => {}
                DockIntent::Edit(action) => match action {
                    KeyAction::Clear => self.input.clear(),
                    KeyAction::Up | KeyAction::Down => {
                        let (row, col) = crate::view::input_cursor(&self.input, width);
                        if matches!(action, KeyAction::Up)
                            && row == 0
                            && width > 0
                            && self.transcript_height(lines.len()) > 0
                            && let Some(cursor) =
                                transcript.cursor(transcript.lines.len().saturating_sub(1), col)
                        {
                            self.viewport.pause();
                            self.viewport.reveal(cursor.row);
                            self.focus = Focus::Transcript(cursor);
                        } else {
                            crate::view::move_input_vertical(
                                &mut self.input,
                                width,
                                matches!(action, KeyAction::Down),
                            );
                        }
                    }
                    KeyAction::HistoryPrev | KeyAction::HistoryNext => {
                        let entry = if matches!(action, KeyAction::HistoryPrev) {
                            self.history.prev()
                        } else {
                            self.history.next()
                        };
                        if let Some(entry) = entry {
                            self.input.set_text(entry);
                        }
                    }
                    action => self.input.edit(&action, width),
                },
            },
        }
        None
    }

    /// 已结束一轮的回复才可评论。
    pub(crate) fn turn_closed(&self, turn: TurnId) -> bool {
        self.ended.contains(&turn)
    }

    fn focus_anchor(&mut self, location: &crate::comment::Location) {
        let width = frame_layout(self.term_width, self.term_height).content_w;
        let transcript = render_transcript(self, width);
        if let Some(cursor) = transcript.anchor_cursor(location) {
            self.focus = Focus::Transcript(cursor);
            self.viewport.on_content_changed(
                transcript.lines.len(),
                self.transcript_height(transcript.lines.len()),
            );
            self.viewport.pause();
            self.viewport.reveal(cursor.row);
        }
    }

    fn focus_input(&mut self, rows: usize) {
        self.focus = Focus::Dock;
        self.viewport.to_bottom(rows, self.transcript_height(rows));
    }

    /// 本地指令不提交 Turn；重连只重新观察，普通退出不触发取消。
    #[rustfmt::skip]
    pub(crate) fn apply_builtin(&mut self, input: &str) -> bool {
        match input {
            "/quit" | "/exit" | "/q" => { self.should_quit = true; true }
            "/reconnect" => { self.view_wanted = self.ctx.session_id.is_some(); true }
            "/help" | "/?" => { self.dock.open_panel(Panel { title: "HELP", lines: help_lines(self.kitty_keyboard) }); true }
            _ => false,
        }
    }

    pub(crate) fn prepare_transcript(&mut self) -> Transcript {
        let width = frame_layout(self.term_width, self.term_height).content_w;
        let transcript = render_transcript(self, width);
        let previous_scroll = self.viewport.scroll();
        let screen_row = match self.focus {
            Focus::Transcript(cursor) => cursor.row.saturating_sub(self.viewport.scroll()),
            Focus::Dock => 0,
        };
        // 合同只要求浏览时 Dock 变化不动视图；焦点在 Dock 且跟随时仍贴底，最新内容不被遮住。
        if self.dock.take_changed() && matches!(self.focus, Focus::Transcript(_)) {
            self.viewport.on_dock_changed(
                transcript.lines.len(),
                self.transcript_height(transcript.lines.len()),
            );
        } else {
            self.viewport.on_content_changed(
                transcript.lines.len(),
                self.transcript_height(transcript.lines.len()),
            );
        }
        if (width == 0
            || self.transcript_height(transcript.lines.len()) == 0
            || transcript.lines.is_empty())
            && !self.queue.is_editing()
        {
            self.focus_input(transcript.lines.len());
        }
        if let Focus::Transcript(mut cursor) = self.focus {
            let previous_row = cursor.row;
            let retained = if let Some((row, col)) = transcript.editor_cursor {
                cursor.row = row;
                cursor.desired_col = col;
                true
            } else {
                transcript.relocate(&mut cursor)
            };
            if !retained {
                self.viewport.keep_screen_row(cursor.row, screen_row);
            } else if self.viewport.following() {
                if self.viewport.scroll() != previous_scroll {
                    self.viewport.clamp_cursor(&mut cursor);
                }
            } else if cursor.row != previous_row {
                self.viewport.keep_screen_row(cursor.row, screen_row);
            }
            cursor.reanchor(&transcript);
            self.focus = Focus::Transcript(cursor);
        }
        transcript
    }

    fn connection_lost(&mut self, cause: &ConnectionStatus) {
        self.queue.pause();
        self.connected = false;
        for block in self.blocks.iter_mut() {
            if let Block::Tool {
                state: state @ ToolState::Running,
                ..
            } = block
            {
                *state = ToolState::Unknown;
            }
        }
        self.push_status(observation_lost_status(cause));
    }

    fn apply_handoff(&mut self, report: DetachReport) {
        // 旧句柄的回执只结清自己的名额，不覆盖新 Turn 的状态或输入。
        if let Some(id) = report.turn_id {
            match report.submission.as_ref().map(|receipt| &receipt.result) {
                Some(SubmissionResult::Accepted { turn }) => {
                    self.queue.release(id);
                    self.resolve_unknown(id);
                    self.accept_session(turn.session_id);
                }
                // 交接报告只生成状态消息，不归还输入。
                Some(SubmissionResult::Rejected { .. } | SubmissionResult::NotAccepted) => {
                    self.resolve_unknown(id);
                    if self.queue.reject(id) {
                        self.mark_rejected_comments(id);
                        self.discard_missing_comments();
                    }
                }
                Some(SubmissionResult::Unknown { .. }) | None => {
                    if self.queue.release(id) {
                        self.queue.pause();
                    }
                }
            }
        }
        // 已由本客户端实时显示的同一回执与结局不再重述；未知之后的确定结果、取消与诊断照常显示。
        let mut report = report;
        if report.submission.as_ref().is_some_and(|receipt| {
            self.shown_receipts
                .contains(&(receipt.turn_id, std::mem::discriminant(&receipt.result)))
        }) {
            report.submission = None;
        }
        let viewed = if report.turn_id.is_some_and(|id| self.summary_shown(id)) {
            Viewed::ClosingShown
        } else {
            Viewed::Unknown
        };
        for line in detach_lines(report, viewed) {
            self.notice_once(handoff_status(&line));
        }
        self.refresh_phase();
    }

    /// 会话视图的推送。
    pub(crate) fn apply_view(&mut self, update: LocalUpdate) {
        match update {
            LocalUpdate::Head(head) => self.apply_head(head),
            LocalUpdate::Entry { entry } => self.apply_entry(*entry),
            LocalUpdate::Text(text) => self.apply_text(text),
            LocalUpdate::Connection { status } => self.connection_lost(&status),
            // 视图出错后不再推送，与连接出错一样是观察中断。
            LocalUpdate::Error { error } => self.connection_lost(&ConnectionStatus::Error(error)),
            // 只读视图既不提交也不取消。
            LocalUpdate::Submit { .. } | LocalUpdate::Cancel { .. } => {}
        }
        self.settle(true);
        if self.initial && self.pending_head.is_none() {
            // 起始会话头当前位置的条目已经到达，初始条目给完；草稿与排队评论的锚点按新路径核对。
            self.initial = false;
            self.discard_missing_comments();
            if let Some(turn) = self.awaiting_before_view.take()
                && self.awaiting_head == Some(turn)
                && !self.head_reached(turn)
            {
                self.awaiting_head = None;
            }
        }
        self.refresh_status_fields();
        self.refresh_phase();
    }

    /// 本客户端提交句柄的推送。
    pub(crate) fn apply_submission(&mut self, update: LocalUpdate) {
        match update {
            LocalUpdate::Submit {
                update: SubmissionUpdate::Receipt(receipt),
            } => self.apply_receipt(receipt),
            LocalUpdate::Submit {
                update: SubmissionUpdate::Sending { .. },
            } => {}
            LocalUpdate::Cancel {
                turn_id,
                status,
                error,
                ..
            } => {
                self.cancel_status = Some(status);
                self.push_status(cancellation_status(turn_id, status, error.as_ref()));
            }
            // 提交句柄不开视图，不推会话内容；会话内容只取自会话视图。
            LocalUpdate::Head(_)
            | LocalUpdate::Entry { .. }
            | LocalUpdate::Text(_)
            | LocalUpdate::Connection { .. }
            | LocalUpdate::Error { .. } => {}
        }
        self.refresh_phase();
    }

    fn apply_receipt(&mut self, receipt: SubmissionReceipt) {
        let turn_id = receipt.turn_id;
        self.shown_receipts
            .insert((turn_id, std::mem::discriminant(&receipt.result)));
        match receipt.result {
            SubmissionResult::Accepted { turn } => {
                self.queue.release(turn_id);
                self.resolve_unknown(turn_id);
                if self
                    .submitted
                    .as_ref()
                    .is_some_and(|(id, _)| *id == turn_id)
                {
                    self.submitted = None;
                }
                self.accept_session(turn.session_id);
                self.ctx.active_agent = turn.agent_name;
                // 已确认的会话头到达这一轮之前，仍按正在运行处理。
                if !self.head_reached(turn_id) {
                    self.awaiting_head = Some(turn_id);
                }
                self.mark_accepted(turn_id);
                // 直接收到的接受由助手标题线表达，不另写接受行。
                if receipt.recovered {
                    self.push_status(recovered_status(turn_id));
                }
            }
            SubmissionResult::Rejected { error } => {
                self.session_exists(&error);
                let kept = self.reject_submission(turn_id);
                self.push_status(rejected_status(turn_id, &error, kept));
            }
            // 不来自查询的未被接受是取消发生在请求写出之前，这次提交没有发出。
            SubmissionResult::NotAccepted => {
                let kept = self.reject_submission(turn_id);
                self.push_status(if receipt.recovered {
                    not_accepted_status(turn_id, kept)
                } else {
                    not_submitted_status(turn_id, kept)
                });
            }
            SubmissionResult::Unknown { error } => {
                // 不还原已提交的输入，避免重复提交；队列暂停到用户决定。
                self.queue.pause();
                let automatic = self.queue.release(turn_id);
                if let Some((_, message)) = self.submitted.take_if(|(id, _)| *id == turn_id) {
                    let held = if automatic {
                        UnknownSubmission::Automatic
                    } else {
                        UnknownSubmission::Manual(message)
                    };
                    self.unknown_submission = Some((turn_id, held));
                }
                if self.ctx.session_id.is_none() {
                    self.unconfirmed = Some(turn_id);
                }
                self.push_status(submission_unknown_status(turn_id, error.as_ref()));
            }
        }
        self.refresh_status_fields();
    }

    /// 确认会话已经建立：此后的提交写 `Existing`，并开始观察它。
    fn accept_session(&mut self, session_id: SessionId) {
        if self.ctx.session_id.is_none() {
            self.ctx.session_id = Some(session_id);
            self.view_wanted = true;
        }
        self.new_session = None;
        self.unconfirmed = None;
    }

    /// 接受回执先于条目到达时，在提交的 USER 块后显示助手标题线。
    fn mark_accepted(&mut self, turn_id: TurnId) {
        if let Some(start) = self.pending_start(turn_id) {
            let range = self.blocks.group(start);
            if !self.blocks.entries[range.clone()]
                .iter()
                .any(|entry| matches!(entry.block, Block::AssistantStart))
            {
                self.blocks
                    .replace(range.end..range.end, vec![Block::AssistantStart]);
            }
        }
    }

    /// 本客户端提交的 USER 块，尚未被同一 TurnId 的 UserMessage 条目取代。
    fn pending_start(&self, turn_id: TurnId) -> Option<usize> {
        self.blocks.entries.iter().position(|entry| {
            matches!(entry.block, Block::UserTurn { turn_id: id, node: None, .. } if id == turn_id)
        })
    }

    /// 其他客户端提交的一轮的取消回执。
    pub(crate) fn apply_cancel_result(
        &mut self,
        turn_id: TurnId,
        result: lattice_api::local::Result<bool>,
    ) {
        let (status, error) = match result {
            Ok(true) => (CancelStatus::Taken, None),
            Ok(false) => (CancelStatus::NotTaken, None),
            Err(error) => (CancelStatus::Unknown, Some(error)),
        };
        self.cancel_status = Some(status);
        self.push_status(cancellation_status(turn_id, status, error.as_ref()));
        self.refresh_phase();
    }

    /// 应用循环开始了新的视图：它的首条会话头之后是初始条目。
    pub(crate) fn begin_view(&mut self) {
        self.view_starting = true;
        self.awaiting_before_view = self.awaiting_head;
    }

    fn last_summary(&self, turn: TurnId) -> Option<&lattice_api::local::TurnSummary> {
        self.entries.values().rev().find_map(|entry| match entry {
            ConversationEntry::Step {
                content,
                summary: Some(summary),
                ..
            } if content.turn_id() == Some(turn) => Some(summary),
            _ => None,
        })
    }

    /// 会话头所示正在执行的那一轮：当前位置那一轮有 Owner 且尚未结束。
    fn running_turn(&self) -> Option<TurnId> {
        let head = self.head.as_ref()?;
        head.turn_id
            .filter(|turn| head.owner && !self.turn_ended(*turn))
    }

    /// 正在运行：本客户端的提交尚未得到结果，或会话头说明当前位置那一轮有 Owner 且尚未结束。
    fn refresh_phase(&mut self) {
        let running = self.submitted.is_some()
            || self.awaiting_head.is_some()
            || self.running_turn().is_some();
        if !running {
            self.cancelling = false;
        }
        self.phase = match (running, self.cancelling) {
            (false, _) => TurnPhase::Idle,
            (true, false) => TurnPhase::Streaming,
            (true, true) => TurnPhase::Cancelling,
        };
    }

    fn cancel_target(&self) -> Option<CancelTarget> {
        if self.submitted.is_some() {
            return Some(CancelTarget::Own);
        }
        let running = self.running_turn().or(self.awaiting_head)?;
        Some(if Some(running) == self.own_turn {
            CancelTarget::Own
        } else {
            CancelTarget::Other(running)
        })
    }

    /// 新视图的初始条目到达或路径替换时丢弃此前的条目及其生成中的文字；
    /// 状态消息与未被接受的 USER 块保留。
    fn discard_entries(&mut self) {
        self.entries.clear();
        self.texts.clear();
        self.ended.clear();
        let mut remove = vec![false; self.blocks.len()];
        let mut index = 0;
        while index < self.blocks.len() {
            let range = self.blocks.group(index);
            let discard = match &self.blocks.entries[index].block {
                Block::UserTurn { node: Some(_), .. } | Block::Step { .. } => true,
                Block::UserTurn { node: None, .. } => self.blocks.entries[range.clone()]
                    .iter()
                    .any(|entry| matches!(entry.block, Block::AssistantStart)),
                _ => false,
            };
            if discard {
                remove[range.clone()].fill(true);
                index = range.end;
            } else {
                index += 1;
            }
        }
        let mut remove = remove.into_iter();
        self.blocks
            .entries
            .retain(|_| !remove.next().unwrap_or(false));
    }

    /// 本轮从替换后的条目重算；会话头当前位置未到齐时不把已装入的小计当作完整值。
    fn refresh_status_fields(&mut self) {
        let latest = self.entries.values().rev().find_map(|entry| match entry {
            ConversationEntry::UserMessage {
                content:
                    Node::UserMessage {
                        turn_id,
                        model,
                        provider,
                        ..
                    },
                ..
            } => Some((*turn_id, model.clone(), provider.clone())),
            _ => None,
        });
        (self.model, self.provider) = latest
            .as_ref()
            .map(|(_, model, provider)| (model.clone(), provider.clone()))
            .unzip();
        self.turn_usage = latest
            .filter(|(turn, _, _)| {
                self.pending_head.is_none()
                    && !self.replace_pending
                    && self.awaiting_head.is_none_or(|accepted| accepted == *turn)
            })
            .map(|(turn, _, _)| {
                UsageSummary::from_attempts(
                    self.entries
                        .values()
                        .filter_map(|entry| match entry {
                            ConversationEntry::Step {
                                content:
                                    Node::Step {
                                        turn_id, attempts, ..
                                    },
                                ..
                            } if *turn_id == turn => Some(attempts),
                            _ => None,
                        })
                        .flatten()
                        .map(|attempt| (attempt.usage, attempt.estimated_cost_usd)),
                )
            });
        self.context_tokens = self
            .entries
            .values()
            .rev()
            .find_map(|entry| match entry {
                ConversationEntry::Step {
                    content: Node::Step { attempts, .. },
                    ..
                } => attempts
                    .iter()
                    .rev()
                    .find_map(|attempt| attempt.usage.and_then(|usage| usage.normalized())),
                _ => None,
            })
            .filter(|_| self.pending_head.is_none())
            .and_then(|usage| match (usage.input, usage.output) {
                (Some(input), Some(output)) => input.checked_add(output),
                (input, output) => input.or(output),
            });
    }

    fn notice_once(&mut self, status: Status) {
        if !self
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Status(known) if *known == status))
        {
            self.push_status(status);
        }
    }

    fn summary_shown(&self, turn_id: TurnId) -> bool {
        self.blocks.iter().any(|block| {
            matches!(block, Block::Status(Status { closing: Some((id, _)), .. }) if *id == turn_id)
        })
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

    /// 新到状态消息的唯一入口：消息按时间位置进入对话记录，是否暂驻由 Dock 自己决定。
    fn push_status(&mut self, status: Status) {
        let id = self.blocks.allocate_id();
        self.dock.offer(id, &status);
        self.blocks.entries.push(IdentifiedBlock {
            id,
            block: Block::Status(status),
        });
    }

    /// 条目中新到的重试提示或结局交给 Dock；它仍留在所属条目的位置上。
    fn offer_in(&mut self, range: std::ops::Range<usize>, wanted: impl Fn(&Status) -> bool) {
        if let Some(entry) = self.blocks.entries[range]
            .iter()
            .find(|entry| matches!(&entry.block, Block::Status(status) if wanted(status)))
            && let Block::Status(status) = &entry.block
        {
            self.dock.offer(entry.id, status);
        }
    }

    pub(crate) fn show_banner(&mut self) {
        self.banner = Some((self.blocks.allocate_id(), startup_banner()));
    }

    fn submit(&mut self) -> Option<String> {
        let raw = self.input.text();
        if raw.trim().is_empty() && self.queue.draft_count() == 0 {
            if self.phase == TurnPhase::Idle && self.input.is_empty() {
                self.queue.resume();
                return self.send_queue_head();
            }
            return None;
        }
        if self.apply_builtin(raw.trim()) {
            self.history.push(raw.trim());
            self.input.clear();
            return None;
        }
        if self.phase != TurnPhase::Idle {
            let id = self.blocks.allocate_id();
            if self.queue.enqueue(id, raw) {
                self.input.clear();
            }
            return None;
        }
        self.input.clear();
        self.check_comments_before_send();
        let message = self.queue.take_message(raw);
        Some(self.start_submission(message, lattice_api::local::new_turn_id()))
    }

    fn auto_submit(&mut self) -> Option<String> {
        let turn = self.head.as_ref()?.turn_id?;
        let completed = self
            .last_summary(turn)
            .is_some_and(|summary| summary.outcome == PublicTurnOutcome::Completed);
        if !(completed && self.turn_ended(turn)) || self.auto_sent_after.contains(&turn) {
            return None;
        }
        let raw = self.send_queue_head()?;
        self.auto_sent_after.insert(turn);
        Some(raw)
    }

    fn send_queue_head(&mut self) -> Option<String> {
        if self.should_quit || self.phase != TurnPhase::Idle {
            return None;
        }
        self.check_comments_before_send();
        let id = lattice_api::local::new_turn_id();
        let raw = self.queue.start_send(id)?;
        Some(self.start_submission(raw, id))
    }

    // 手动与排队共用提交路径，不借用或覆盖正在编辑的输入区。
    fn start_submission(&mut self, message: crate::queue::Outgoing, id: TurnId) -> String {
        self.history.push(message.text.trim());
        let raw = message.encode();
        self.cancel_status = None;
        self.own_turn = Some(id);
        self.submitted = Some((id, message));
        self.blocks.push(Block::UserTurn {
            turn_id: id,
            node: None,
            ordinal: None,
            agent: self.ctx.active_agent.clone(),
        });
        self.blocks.push(crate::session::user_block(raw.clone()));
        self.refresh_phase();
        raw
    }

    fn cancel_or_clear(&mut self) {
        if let Some(target) = self.cancel_target() {
            self.cancel_requested = Some(target);
            self.cancelling = true;
            self.phase = TurnPhase::Cancelling;
        } else if self.dock.shows_input() {
            self.input.clear();
        }
    }

    fn discard_missing_comments(&mut self) {
        let steps = self
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Step { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        let dropped = self.queue.retain_steps(&steps);
        if !dropped.is_empty() {
            self.push_status(discarded_comments_status(&dropped));
        }
    }

    /// 自动发送的一条放回队首且不进输入区；手动提交的文字在输入区为空时放回，
    /// 输入区已有用户的草稿时不覆盖，原文留在提交时记下的输入历史中。两者都回到空闲。
    /// 结果未知之后才确定未被接受的提交同样处理。只归还本客户端的草稿，
    /// 不撤销已知的执行、接管或完成显示。
    fn reject_submission(&mut self, turn_id: TurnId) -> KeptInput {
        let automatic = self.queue.reject(turn_id);
        let mut kept = KeptInput::Restored;
        let manual = match self.unknown_submission.take_if(|(id, _)| *id == turn_id) {
            Some((_, UnknownSubmission::Manual(message))) => Some(message),
            Some((_, UnknownSubmission::Automatic)) => {
                kept = KeptInput::History;
                None
            }
            // 自动发送的一条也记在 submitted 中，原文以排队中的为准。
            None => self
                .submitted
                .take_if(|(id, _)| *id == turn_id)
                .map(|(_, message)| message)
                .filter(|_| !automatic),
        };
        if !automatic && manual.is_none() && kept == KeptInput::Restored {
            return KeptInput::Nothing;
        }
        self.mark_rejected_comments(turn_id);
        // 评论不进输入历史，总是恢复为草稿。
        if let Some(message) = manual {
            let text = self.queue.restore(message, &mut self.blocks);
            if self.input.is_empty() {
                self.input.set_text(&text);
            } else if !text.trim().is_empty() {
                kept = KeptInput::History;
            }
        }
        self.discard_missing_comments();
        kept
    }

    /// 收到这次提交的确定结果：不再保留结果未知时留下的原文。
    fn resolve_unknown(&mut self, turn_id: TurnId) {
        if self
            .unknown_submission
            .as_ref()
            .is_some_and(|(id, _)| *id == turn_id)
        {
            self.unknown_submission = None;
        }
    }

    fn mark_rejected_comments(&mut self, turn_id: TurnId) {
        let Some(start) = self.pending_start(turn_id) else {
            return;
        };
        let range = self.blocks.group(start);
        for entry in &mut self.blocks.entries[range] {
            if let Block::CommentedUser(message) = &entry.block {
                entry.block = Block::RejectedUser(message.clone());
            }
        }
    }

    /// 与绘制路径使用同一内容宽度及 Dock 高度，statusline 始终占一行；
    /// `rows` 是同一宽度下的对话记录行数，Dock 高度随它变化。
    pub(crate) fn transcript_height(&self, rows: usize) -> usize {
        let layout = frame_layout(self.term_width, self.term_height);
        self.term_height
            .saturating_sub(1)
            .saturating_sub(Dock::new(self).height(layout.content_w, rows)) as usize
    }

    fn apply_head(&mut self, head: SessionHead) {
        // 新视图的首条会话头或路径已替换：之后到它当前位置的条目为止是初始条目。
        // 路径替换立即丢弃此前的条目；新视图等首个初始条目到达再替换，重连失败时内容保留。
        let starts = std::mem::take(&mut self.view_starting) || head.path_replaced;
        if head.path_replaced {
            self.replace_entries();
            self.awaiting_head = None;
        } else if starts {
            self.replace_pending = true;
        }
        if starts {
            self.initial = true;
        }
        self.ctx.session_id = Some(head.session_id);
        if let Some(agent) = &head.agent {
            self.ctx.active_agent = agent.clone();
        }
        self.connected = true;
        if head.diagnostic != self.diagnostic {
            if let Some(error) = &head.diagnostic {
                self.push_status(execution_diagnostic_status(error));
            }
            self.diagnostic = head.diagnostic.clone();
        }
        // 当前位置是根节点时没有对应的条目，会话头立即确认。
        let root = head.position == NodeId(0);
        self.pending_head = Some(head);
        if root {
            self.confirm_head();
        }
    }

    /// 收到待确认会话头当前位置的条目：之后按它判定各轮。
    fn confirm_head(&mut self) {
        if std::mem::take(&mut self.replace_pending) {
            self.replace_entries();
        }
        self.head = self.pending_head.take();
        if self
            .awaiting_head
            .is_some_and(|turn| self.head_reached(turn))
        {
            self.awaiting_head = None;
        }
    }

    /// 已确认的会话头到达了这一轮：指向它，或当前路径上已有它的 UserMessage（位置已经越过它）。
    /// 每批推送先给会话头，再按路径顺序给到当前位置为止的条目，所以确认时这一轮的
    /// UserMessage 若在本批中，已经到达。
    fn head_reached(&self, turn: TurnId) -> bool {
        self.pending_head.is_none()
            && (self
                .head
                .as_ref()
                .is_some_and(|head| head.turn_id == Some(turn))
                || self.entries.values().any(|entry| {
                    matches!(entry, ConversationEntry::UserMessage { .. })
                        && entry_turn(entry) == Some(turn)
                }))
    }

    /// 新视图的初始条目开始到达或路径已替换：丢弃此前的条目，迟到的历史查询结果作废。
    fn replace_entries(&mut self) {
        self.replace_pending = false;
        self.discard_entries();
        self.epoch = self.epoch.wrapping_add(1);
        self.older_pending = false;
        self.orphan_warned = None;
    }

    /// 同一 NodeId 原位替换；本客户端提交的 USER 块由同一 TurnId 的 UserMessage 条目取代；
    /// 其余新条目接在末尾。
    fn apply_entry(&mut self, entry: ConversationEntry) {
        let node = entry.node();
        let Some(turn) = entry_turn(&entry) else {
            return;
        };
        if self.replace_pending {
            self.replace_entries();
        }
        let user_message = matches!(entry, ConversationEntry::UserMessage { .. });
        // 视图中出现这次提交的 UserMessage，同样说明已被接受；回执可能更晚到或者丢失。
        // 它只证明已接受，忙状态仍保留到会话头确认。
        if user_message {
            self.resolve_unknown(turn);
        }
        if user_message && self.submitted.as_ref().is_some_and(|(id, _)| *id == turn) {
            self.submitted = None;
            self.queue.release(turn);
            if !self.head_reached(turn) {
                self.awaiting_head = Some(turn);
            }
        }
        let known_retries: HashSet<u32> = match self.entries.insert(node, entry) {
            Some(ConversationEntry::Step { retries, .. }) => {
                retries.iter().map(|retry| retry.attempt).collect()
            }
            _ => HashSet::new(),
        };
        if self
            .texts
            .get(&node)
            .is_some_and(|generating| crate::session::superseded(&self.entries[&node], generating))
        {
            self.texts.remove(&node);
        }
        let blocks = crate::session::entry_blocks(
            &self.entries[&node],
            self.ended.contains(&turn),
            self.texts.get(&node),
        );
        let start = self
            .blocks
            .node_start(node)
            .or_else(|| self.pending_start(turn).filter(|_| user_message));
        let range = match start {
            Some(start) => self.blocks.replace(self.blocks.group(start), blocks),
            None => {
                let end = self.blocks.len();
                self.blocks.replace(end..end, blocks)
            }
        };
        // 推送使条目多出的重试提示是新到的消息；初始条目中已有的不是。
        if !self.initial {
            let fresh: Vec<_> = self.blocks.entries[range.clone()]
                .iter()
                .filter_map(|entry| match &entry.block {
                    Block::Status(Status {
                        retry: Some(key), ..
                    }) if !known_retries.contains(&key.attempt_ordinal) => Some(*key),
                    _ => None,
                })
                .collect();
            for key in fresh {
                self.offer_in(range.clone(), |status| status.retry == Some(key));
            }
        }
        if self
            .pending_head
            .as_ref()
            .is_some_and(|head| head.position == node)
        {
            self.confirm_head();
        }
    }

    /// 生成中的文字按所属节点与尝试累积；新的尝试从头开始。
    fn apply_text(&mut self, update: TextUpdate) {
        let generating = self.texts.entry(update.node).or_default();
        if generating.attempt != update.attempt || update.offset == 0 {
            *generating = Generating {
                attempt: update.attempt,
                ..Generating::default()
            };
        }
        for segment in update.segments {
            match segment {
                TextSegment::Text { text } => {
                    generating.received = generating.received.saturating_add(text.len());
                    generating.text.push_str(&text);
                }
                TextSegment::Reasoning { text } => {
                    generating.received = generating.received.saturating_add(text.len());
                }
            }
        }
        if self
            .entries
            .get(&update.node)
            .is_some_and(|entry| crate::session::superseded(entry, &self.texts[&update.node]))
        {
            self.texts.remove(&update.node);
        }
        self.render_node(update.node);
    }

    fn render_node(&mut self, node: NodeId) {
        let Some(entry) = self.entries.get(&node) else {
            return;
        };
        let ended = entry_turn(entry).is_some_and(|turn| self.ended.contains(&turn));
        let blocks = crate::session::entry_blocks(entry, ended, self.texts.get(&node));
        if let Some(start) = self.blocks.node_start(node) {
            self.blocks.replace(self.blocks.group(start), blocks);
        }
    }

    /// 按当前会话头与条目重新判定各轮是否结束。由未结束变为结束的一轮重画它的各段：
    /// 槽位改按会话库给出的结论显示，最后一步带出结局；`fresh` 且不在初始条目中时结局是新到的消息。
    fn settle(&mut self, fresh: bool) {
        let turns: std::collections::BTreeSet<TurnId> =
            self.entries.values().filter_map(entry_turn).collect();
        for turn in turns {
            let ended = self.turn_ended(turn);
            if ended == self.ended.contains(&turn) {
                continue;
            }
            if ended {
                self.ended.insert(turn);
            } else {
                self.ended.remove(&turn);
            }
            let nodes: Vec<NodeId> = self
                .entries
                .iter()
                .filter(|(_, entry)| entry_turn(entry) == Some(turn))
                .map(|(node, _)| *node)
                .collect();
            for node in nodes {
                self.render_node(node);
            }
            if ended && fresh && !self.initial {
                if let Some(start) = self.last_step_start(turn) {
                    let range = self.blocks.group(start);
                    self.offer_in(range, |status| {
                        status.closing.as_ref().is_some_and(|(id, _)| *id == turn)
                    });
                }
                if self
                    .last_summary(turn)
                    .is_some_and(|summary| summary.outcome != PublicTurnOutcome::Completed)
                {
                    self.queue.pause();
                }
            }
        }
        // 当前位置那一轮停在未关闭的 Step 上而没有 Owner：执行者已消失，每个视图提示一次。
        if let Some(head) = &self.head
            && let Some(turn) = head.turn_id
            && !head.owner
            && self.orphan_warned != Some(turn)
            && self
                .last_summary(turn)
                .is_some_and(|summary| !summary.progress.is_closed())
        {
            self.orphan_warned = Some(turn);
            self.push_status(orphaned_turn_status(turn));
        }
    }

    fn last_step_start(&self, turn: TurnId) -> Option<usize> {
        let node = self
            .entries
            .iter()
            .rev()
            .find_map(|(node, entry)| match entry {
                ConversationEntry::Step {
                    content,
                    summary: Some(_),
                    ..
                } if content.turn_id() == Some(turn) => Some(*node),
                _ => None,
            })?;
        self.blocks.node_start(node)
    }

    /// 视图翻到对话记录顶部而第 1 轮尚未装入时，以已装入的最早一轮的 UserMessage 为 `before`
    /// 取更早的轮。失败后不自动重查，等下一次翻到顶部。
    fn request_older_at_top(&mut self) {
        if self.viewport.scroll() != 0 || self.older_pending {
            return;
        }
        let Some(session_id) = self.ctx.session_id else {
            return;
        };
        let Some((before, ordinal)) = self.entries.iter().find_map(|(node, entry)| match entry {
            ConversationEntry::UserMessage { ordinal, .. } => Some((*node, *ordinal)),
            ConversationEntry::Step { .. } => None,
        }) else {
            return;
        };
        if ordinal <= 1 {
            return;
        }
        self.older_pending = true;
        self.older_request = Some(OlderRequest {
            session_id,
            before,
            epoch: self.epoch,
        });
    }

    /// 取到的轮插在已装入的最早一轮之前，视图停在原来看到的内容上。请求发出后路径被替换时，
    /// 结果属于旧路径，丢弃。
    pub(crate) fn apply_older(
        &mut self,
        request: OlderRequest,
        result: lattice_api::local::Result<LocalHistory>,
    ) {
        if request.epoch != self.epoch {
            return;
        }
        self.older_pending = false;
        let history = match result {
            Ok(history) => history,
            Err(error) => {
                self.push_status(older_turns_unavailable_status(&error));
                return;
            }
        };
        let Some(mut at) = self.blocks.node_start(request.before) else {
            return;
        };
        let width = frame_layout(self.term_width, self.term_height).content_w;
        // 横幅与插入点之前的状态消息不随插入移动；翻到顶部时第 0 行总是横幅，所以以屏幕上
        // 第一行插入点之后的内容为准。记下这一行、它所属的块，及它在这个块自己的各行中的倒数序号：
        // 块前面有了内容时开头会多出分隔空行，从末行数起不受影响；块内穿插的内嵌评论属于别的块，
        // 不计入。插入后按它恢复视图。
        let scroll = self.viewport.scroll();
        let transcript = render_transcript(self, width);
        let visible = scroll.saturating_add(self.transcript_height(transcript.lines.len()));
        let fixed: HashSet<BlockId> = self
            .banner
            .iter()
            .map(|(id, _)| *id)
            .chain(self.blocks.entries[..at].iter().map(|entry| entry.id))
            .collect();
        let top = (scroll..visible.min(transcript.owners.len()))
            .find(|row| !fixed.contains(&transcript.owners[*row].id))
            .and_then(|row| Some((row, block_rows_from_end(&transcript, row)?)));
        for entry in history.entries {
            let node = entry.node();
            let Some(turn) = entry_turn(&entry) else {
                continue;
            };
            if self.entries.contains_key(&node) {
                continue;
            }
            self.entries.insert(node, entry);
            // 更早的轮都已结束：当前位置所属的那一轮总在已装入的部分里。不按会话头判定，
            // 新会话头待确认时也一样；它们的提示与结局不是新消息，直接进对话记录。
            let ended = self.last_summary(turn).is_some();
            if ended {
                self.ended.insert(turn);
            }
            let blocks = crate::session::entry_blocks(&self.entries[&node], ended, None);
            at = self.blocks.replace(at..at, blocks).end;
        }
        // 浏览光标由 prepare_transcript 按所在块重新定位并保持屏幕行；焦点在 Dock 时移动视图。
        // 屏幕上只有横幅与前置状态消息时，插入的行都在屏幕之下，视图不动。
        if matches!(self.focus, Focus::Dock)
            && !self.viewport.following()
            && let Some((old_row, (id, rest))) = top
        {
            let rows = block_rows(&render_transcript(self, width), id);
            if let Some(&row) = rows
                .len()
                .checked_sub(rest)
                .and_then(|index| rows.get(index))
            {
                self.viewport.shift(row.saturating_sub(old_row));
            }
        }
        self.settle(false);
        self.refresh_status_fields();
    }

    /// 按 TurnId 找回新会话的上一次提交。已被接受时会话已经建立，改为 `Existing` 并开始观察；
    /// 查无此请求时它确定未被接受。
    pub(crate) fn apply_recovery(
        &mut self,
        turn_id: TurnId,
        result: lattice_api::local::Result<lattice_api::local::TurnDetail>,
    ) {
        match result {
            Ok(turn) => {
                self.resolve_unknown(turn_id);
                self.accept_session(turn.session_id);
                self.push_status(recovered_status(turn_id));
            }
            // 按 TurnId 查询为 NotFound：提交确定未被接受，归还输入。它没有建立会话，
            // 之后的提交再遇到会话已存在时不再找回它。
            Err(error) if error.kind == LocalErrorKind::NotFound => {
                if self.unconfirmed == Some(turn_id) {
                    self.unconfirmed = None;
                }
                let kept = self.reject_submission(turn_id);
                self.push_status(not_accepted_status(turn_id, kept));
            }
            Err(error) => self.push_status(submission_unknown_status(turn_id, Some(&error))),
        }
        self.refresh_phase();
    }

    /// 新会话的提交得到“会话已存在”：上一次结果未知的提交实际已建立会话，按它的 TurnId 找回。
    fn session_exists(&mut self, error: &LocalError) {
        if error.conflict == Some(lattice_api::local::ConflictReason::SessionExists)
            && self.ctx.session_id.is_none()
        {
            self.recover_request = self.unconfirmed;
        }
    }

    /// 这次提交的身份：已有会话写 `Existing`；新会话在首次提交时生成 SessionId，
    /// 确认被接受前之后的提交沿用它与 `New`。
    pub(crate) fn submission_identity(&mut self) -> Option<(TurnId, SubmitSession)> {
        let (turn_id, _) = self.submitted.as_ref()?;
        let session = match self.ctx.session_id {
            Some(id) => SubmitSession::Existing(id),
            None => SubmitSession::New(
                *self
                    .new_session
                    .get_or_insert_with(lattice_api::local::new_session_id),
            ),
        };
        Some((*turn_id, session))
    }

    /// 这一轮已经结束：最后一步已关闭，或者会话头说明它不在执行（当前位置在别处，或没有 Owner）。
    /// 新会话头待确认时，已确认的那条早于本批条目，不据它作新的判定：本批中刚到的一轮会被它
    /// 当成已在别处结束。其间沿用确认时的结论，确认后再按新会话头判定，已结束的轮也不因此反复。
    /// 还没有步骤的一轮没有可显示的结局，留到步骤到达再判定。
    fn turn_ended(&self, turn: TurnId) -> bool {
        self.last_summary(turn).is_some_and(|summary| {
            summary.progress.is_closed()
                || match (&self.pending_head, &self.head) {
                    (Some(_), _) => self.ended.contains(&turn),
                    (None, Some(head)) => head.turn_id != Some(turn) || !head.owner,
                    (None, None) => false,
                }
        })
    }

    /// 退出报告所述那一轮已知的情况。对话记录已不在屏幕上，结局照常重述。
    fn exit_viewed(&self, turn: Option<TurnId>) -> Viewed {
        if turn.is_some_and(|id| self.turn_ended(id)) {
            Viewed::Ended
        } else {
            Viewed::Unknown
        }
    }

    /// 路径替换后、新的初始条目尚未全部确认时提交，先按已装入的条目核对评论锚点。
    fn check_comments_before_send(&mut self) {
        if self.initial {
            self.discard_missing_comments();
        }
    }
}

/// 块自己的各行在对话记录中的位置，按行序；不含穿插其间、属于别的块的行。
fn block_rows(transcript: &Transcript, id: BlockId) -> Vec<usize> {
    transcript
        .owners
        .iter()
        .enumerate()
        .filter(|(_, owner)| owner.id == id)
        .map(|(row, _)| row)
        .collect()
}

/// `row` 所属的块，及这一行在该块自己的各行中的倒数序号（末行为 1）。
fn block_rows_from_end(transcript: &Transcript, row: usize) -> Option<(BlockId, usize)> {
    let id = transcript.owners.get(row)?.id;
    let rows = block_rows(transcript, id);
    let index = rows.iter().position(|known| *known == row)?;
    Some((id, rows.len().saturating_sub(index)))
}

fn entry_turn(entry: &ConversationEntry) -> Option<TurnId> {
    match entry {
        ConversationEntry::UserMessage { content, .. }
        | ConversationEntry::Step { content, .. } => content.turn_id(),
    }
}

/// 进入 raw 模式后并行接收视图推送与按键；启动不等待条目读取。
pub(crate) async fn run_app(
    mut options: TuiOptions,
    ctx: SessionContext,
    client: LocalClient,
) -> anyhow::Result<()> {
    let root = options.project_root.clone();
    options.project_root =
        tokio::task::spawn_blocking(move || std::fs::canonicalize(root)).await??;
    // try_init 可能在 raw/备用屏幕已改变后失败，恢复 guard 必须先于初始化。
    let mut cleanup = TerminalCleanup { kitty: false };
    let mut terminal = ratatui::try_init()?;
    let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    install_cleanup_panic_hook(kitty);
    cleanup.kitty = kitty;
    enable_terminal_modes(&mut std::io::stdout(), kitty)?;

    let mut app = App::new(ctx, kitty, &options.session_dir);
    app.show_banner();
    let mut exit = run_loop(&options, &mut app, &client, EventStream::new(), |app| {
        terminal.draw(|f| crate::view::draw(f, app))?;
        Ok(())
    })
    .await;
    // 先复原终端，再由共同句柄有界承接既有意图；普通退出不新增取消。
    drop(cleanup);
    if let Some(turn) = exit.turn {
        exit.handoffs.push(turn.detach().boxed());
    }
    while let Some(report) = exit.handoffs.next().await {
        let viewed = app.exit_viewed(report.turn_id);
        for line in detach_lines(report, viewed) {
            eprintln!("{line}");
        }
    }
    exit.result
}

struct LoopExit {
    result: anyhow::Result<()>,
    turn: Option<LocalTurn>,
    handoffs: FuturesUnordered<BoxFuture<'static, DetachReport>>,
}

/// 产品接 crossterm/ratatui，测试注入按键并检查同一绘制回调。
/// 会话视图与本客户端的提交句柄分开持有：内容只取自视图，提交、取消与退出交接经句柄。
async fn run_loop<E, D>(
    options: &TuiOptions,
    app: &mut App,
    client: &LocalClient,
    mut events: E,
    mut draw: D,
) -> LoopExit
where
    E: futures::Stream<Item = std::io::Result<Event>> + Unpin,
    D: FnMut(&mut App) -> anyhow::Result<()>,
{
    let mut ticker = tokio::time::interval(FRAME);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut handoffs = FuturesUnordered::new();
    let mut view: Option<LocalTurn> = None;
    let mut turn: Option<LocalTurn> = None;
    // 句柄交付完自己的更新后仍保留，退出或换用新句柄时交给 detach。
    let mut turn_open = false;
    let mut cancels = FuturesUnordered::new();
    let mut older = FuturesUnordered::new();
    let mut recoveries = FuturesUnordered::new();
    let mut dirty = true;
    let result = loop {
        if std::mem::take(&mut app.view_wanted)
            && !app.should_quit
            && let Some(session_id) = app.ctx.session_id
        {
            // 重新开始 watch 用新的句柄，旧连接上尚未读出的推送随旧句柄丢弃。
            view = Some(client.observe(session_id));
            app.begin_view();
        }
        if let Some(target) = app.cancel_requested.take() {
            match target {
                CancelTarget::Own => {
                    if let Some(turn) = &turn {
                        turn.cancel();
                    }
                }
                CancelTarget::Other(turn_id) => {
                    let client = client.clone();
                    cancels.push(async move { (turn_id, client.cancel(turn_id).await) }.boxed());
                }
            }
            dirty = true;
        }
        if let Some(request) = app.older_request.take() {
            let client = client.clone();
            older.push(
                async move {
                    let result = client
                        .history_before(request.session_id, request.before)
                        .await;
                    (request, result)
                }
                .boxed(),
            );
        }
        if let Some(turn_id) = app.recover_request.take() {
            let client = client.clone();
            recoveries.push(async move { (turn_id, client.wait_for_turn(turn_id).await) }.boxed());
        }
        if app.should_quit {
            break Ok(());
        }
        let mut prompt = None;
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(event)) => prompt = handle_input_event(app, event),
                    Some(Err(_)) | None => app.should_quit = true,
                }
            }
            Some(report) = handoffs.next(), if !handoffs.is_empty() => app.apply_handoff(report),
            update = async { turn.as_mut().expect("已有句柄").next().await }, if turn_open => {
                match update {
                    Some(update) => app.apply_submission(update),
                    None => turn_open = false,
                }
            }
            update = async { view.as_mut().expect("已有视图").next().await }, if view.is_some() => {
                match update {
                    Some(update) => {
                        // 提交句柄不开视图，取消补发与结束判定用转入的同一份视图消息。
                        let forwarded = match &update {
                            LocalUpdate::Head(head) => Some(ViewUpdate::Head(head.clone())),
                            LocalUpdate::Entry { entry } => Some(ViewUpdate::Entry { entry: entry.clone() }),
                            LocalUpdate::Text(text) => Some(ViewUpdate::Text(text.clone())),
                            _ => None,
                        };
                        app.apply_view(update);
                        if let (Some(turn), Some(forwarded)) = (&turn, forwarded) {
                            turn.apply_view(forwarded).await;
                        }
                    }
                    None => {
                        view = None;
                        app.connection_lost(&ConnectionStatus::Eof);
                    }
                }
            }
            Some((turn_id, result)) = cancels.next(), if !cancels.is_empty() => app.apply_cancel_result(turn_id, result),
            Some((request, result)) = older.next(), if !older.is_empty() => app.apply_older(request, result),
            Some((turn_id, result)) = recoveries.next(), if !recoveries.is_empty() => app.apply_recovery(turn_id, result),
            _ = ticker.tick() => {
                dirty |= app.expire_ctrl_c_notice(Instant::now());
                if !dirty { continue; }
                if let Err(error) = draw(app) { break Err(error); }
                dirty = false;
                continue;
            }
        }
        dirty = true;
        if let Some(prompt) = prompt.or_else(|| app.auto_submit()) {
            if let Some(old) = turn.take() {
                handoffs.push(old.detach().boxed());
            }
            turn = begin_turn(options, app, prompt).map(|request| client.start_submission(request));
            turn_open = turn.is_some();
        }
    };
    // 输入结束、普通退出和绘制失败都保存尚未发送的原文；不保存编辑器草稿。
    for text in app.queue.drain() {
        app.history.push(&text);
    }
    LoopExit {
        result,
        turn,
        handoffs,
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
                resolve_key(key, app.kitty_keyboard, app.input.is_empty() && matches!(app.focus, Focus::Dock) && app.dock.shows_input())
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

fn begin_turn(options: &TuiOptions, app: &mut App, prompt: String) -> Option<TurnRequest> {
    let (turn_id, session) = app.submission_identity()?;
    Some(TurnRequest {
        turn_id,
        session,
        workspace: options.project_root.clone(),
        input: prompt,
        agent: (!options.agent.is_empty()).then(|| options.agent.clone()),
    })
}

#[rustfmt::skip]
fn help_lines(kitty: bool) -> Vec<String> {
    let nl = if kitty { "Shift+Enter  换行" } else { "Alt+Enter  换行" };
    vec![
        "/help  显示本帮助".into(),
        "/reconnect  只重连观察，不恢复或重新提交".into(),
        "/quit  退出（输入为空时也可按 Ctrl+D）".into(),
        "Esc  清空输入；关闭本面板".into(),
        "Space  关闭警告或错误，移入对话记录".into(),
        "Ctrl+C  取消运行中 Turn；空闲清空输入；两秒内再按退出".into(),
        "Ctrl+↑/↓  输入历史".into(),
        "Enter  提交".into(),
        nl.into(),
    ]
}

#[rustfmt::skip]
/// session 与 agent 由状态行（或侧栏）显示并随接受更新，这里不重复。
fn startup_banner() -> Vec<String> {
    vec![
        "LATTICE".into(),
        "/help  显示命令".into(),
        "/quit  退出".into(),
        "/reconnect  重新读取状态并观察".into(),
    ]
}

fn enable_terminal_modes(writer: &mut impl std::io::Write, kitty: bool) -> std::io::Result<()> {
    crossterm::execute!(
        writer,
        crossterm::cursor::SetCursorStyle::BlinkingBlock,
        EnableBracketedPaste
    )?;
    if kitty {
        crossterm::execute!(
            writer,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    Ok(())
}

fn restore_terminal_modes(writer: &mut impl std::io::Write, kitty: bool) -> std::io::Result<()> {
    // 每项独立尝试；形状恢复失败也不能跳过键盘/粘贴清理。
    let shape = crossterm::execute!(writer, crossterm::cursor::SetCursorStyle::DefaultUserShape);
    let paste = crossterm::execute!(writer, DisableBracketedPaste);
    let keyboard = if kitty {
        crossterm::execute!(writer, PopKeyboardEnhancementFlags)
    } else {
        Ok(())
    };
    shape.and(paste).and(keyboard)
}

#[rustfmt::skip]
fn install_cleanup_panic_hook(kitty: bool) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal_modes(&mut std::io::stdout(), kitty);
        prev(info);
    }));
}

struct TerminalCleanup {
    kitty: bool,
}

#[rustfmt::skip]
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = restore_terminal_modes(&mut std::io::stdout(), self.kitty);
        ratatui::restore();
    }
}

#[cfg(test)]
#[path = "app_loop_tests.rs"]
mod loop_tests;

#[cfg(test)]
#[path = "connection_tests.rs"]
mod connection_tests;

#[cfg(test)]
#[path = "test_fixtures.rs"]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    #[test]
    fn terminal_cleanup_restores_on_normal_error_and_panic_paths() {
        const PROBE: &str = "LATTICE_TUI_CLEANUP_PROBE";
        if let Ok(mode) = std::env::var(PROBE) {
            let run = || -> std::io::Result<()> {
                let _cleanup = TerminalCleanup { kitty: false };
                install_cleanup_panic_hook(false);
                enable_terminal_modes(&mut std::io::stdout(), false)?;
                if mode == "panic" {
                    panic!("cleanup probe panic");
                }
                if mode == "error" {
                    return Err(std::io::Error::other("cleanup probe error"));
                }
                Ok(())
            };
            assert_eq!(run().is_ok(), mode == "normal");
            return;
        }
        for mode in ["normal", "error", "panic"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::tests::terminal_cleanup_restores_on_normal_error_and_panic_paths",
                    "--nocapture",
                ])
                .env(PROBE, mode)
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                mode != "panic",
                "mode {mode}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let restored = output.stdout.windows(5).position(|s| s == b"\x1b[0 q");
            assert!(restored.is_some(), "{mode} must restore default shape");
            let enabled = output.stdout.windows(5).position(|s| s == b"\x1b[1 q");
            assert!(enabled.unwrap() < restored.unwrap());
        }
    }

    #[test]
    fn ctrl_c_targets_running_turn_but_esc_only_clears_input() {
        let mut app = test_app(true);
        let id = lattice_api::local::new_turn_id();
        open_view(
            &mut app,
            head(2, Some(id), true),
            vec![user(1, id, 1, "prompt"), step(2, id, 0).entry()],
        );
        assert_eq!(app.phase, TurnPhase::Streaming);
        app.input.set_text("draft");
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(app.input.is_empty());
        assert_eq!(app.cancel_requested, None);
        assert_eq!(app.phase, TurnPhase::Streaming);
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        // 其他客户端提交的一轮按会话头所示的 TurnId 直接取消。
        assert_eq!(app.cancel_requested, Some(CancelTarget::Other(id)));
        assert_eq!(app.phase, TurnPhase::Cancelling);
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(app.should_quit);
    }
    #[test]
    fn double_ctrl_c_within_window_quits_single_does_not() {
        let mut app = test_app(false);
        assert!(!matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::CancelOrClear);
        assert!(!app.should_quit);
        assert!(matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::Quit);
        assert!(app.should_quit);

        let mut app = test_app(false);
        app.input.set_text("clear on first Ctrl+C");
        ctrl_c_action(&mut app);
        app.last_ctrl_c = Some(Instant::now() - Duration::from_secs(3));
        assert!(!matches!(ctrl_c_action(&mut app), KeyAction::Quit));
        app.apply_action(KeyAction::CancelOrClear);
        assert!(app.input.is_empty());
        assert!(!app.should_quit);
    }

    /// 消息之间不叠：替换（Warn→Warn、Warn→Error、Error→Warn）把旧消息落回对话记录的
    /// 原时间位置，释放后每条恰好出现一次；Info 不进 Dock；草稿不受影响。
    #[test]
    fn dock_replacement_and_release_keep_transcript_order_and_draft() {
        let mut app = test_app(false);
        app.input.set_text("draft");
        let docked_is = |app: &App, level: NoticeLevel, marker: &str| {
            app.dock
                .message()
                .is_some_and(|status| status.level == level && status.text.contains(marker))
        };
        app.push_status(status(NoticeLevel::Warn, "warn-0"));
        app.push_status(status(NoticeLevel::Warn, "warn-a"));
        assert!(docked_is(&app, NoticeLevel::Warn, "warn-a"));
        let text = transcript(&app);
        assert!(text.contains("warn-0") && !text.contains("warn-a"));
        app.push_status(status(NoticeLevel::Info, "info-i"));
        assert!(
            docked_is(&app, NoticeLevel::Warn, "warn-a"),
            "Info 不进 Dock"
        );
        assert!(transcript(&app).contains("info-i"));
        app.push_status(status(NoticeLevel::Error, "commit-b"));
        assert!(docked_is(&app, NoticeLevel::Error, "commit-b"));
        assert!(transcript(&app).contains("warn-a"));
        app.push_status(status(NoticeLevel::Warn, "warn-c"));
        assert!(docked_is(&app, NoticeLevel::Warn, "warn-c"));
        // Space：释放当前消息，Space 不进入输入，草稿不变。
        assert!(app.apply_action(KeyAction::Insert(' ')).is_none());
        assert!(app.dock.shows_input());
        assert_eq!(app.input.text(), "draft");
        let order: Vec<usize> = ["warn-0", "warn-a", "info-i", "commit-b", "warn-c"]
            .into_iter()
            .map(|marker| row(&app, marker))
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "保持到达顺序");
    }
    fn dock_title(app: &mut App) -> &'static str {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::view::draw(frame, app))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let shown = ["USER", "HELP", "WARN", "ERROR"]
            .into_iter()
            .filter(|title| screen.contains(&format!("── {title} ─")))
            .collect::<Vec<_>>();
        assert_eq!(shown.len(), 1, "Dock 只显示栈顶：{shown:?}");
        shown[0]
    }

    /// Dock 模块栈的四种状态与两种按键：栈顶是面板时只有 Esc 关闭，栈顶是消息时只有
    /// 无修饰 Space 释放；另一种按键不动栈，也不改不可见的输入。
    #[test]
    fn dock_stack_shows_the_top_module_and_routes_space_and_esc() {
        #[derive(Debug)]
        enum Step {
            Help,
            Notice(NoticeLevel),
            Space,
            Esc,
        }
        use Step::*;
        let cases: [(&str, Vec<(Step, &str)>); 4] = [
            ("输入区", vec![(Notice(NoticeLevel::Info), "USER")]),
            (
                "输入区、面板",
                vec![(Help, "HELP"), (Space, "HELP"), (Esc, "USER")],
            ),
            (
                "输入区、消息",
                vec![
                    (Notice(NoticeLevel::Warn), "WARN"),
                    (Esc, "WARN"),
                    (Notice(NoticeLevel::Error), "ERROR"),
                    (Esc, "ERROR"),
                    (Space, "USER"),
                ],
            ),
            (
                "输入区、面板、消息",
                vec![
                    (Help, "HELP"),
                    (Notice(NoticeLevel::Warn), "WARN"),
                    (Esc, "WARN"),
                    (Space, "HELP"),
                    (Space, "HELP"),
                    (Esc, "USER"),
                ],
            ),
        ];
        let press = |app: &mut App, code: KeyCode| {
            handle_input_event(app, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        };
        for (name, steps) in cases {
            let mut app = test_app(false);
            app.input.set_text("draft");
            assert_eq!(dock_title(&mut app), "USER", "{name}");
            for (index, (step, top)) in steps.into_iter().enumerate() {
                let label = format!("{name} 第 {index} 步 {step:?}");
                let draft = app.input.text();
                // `/help` 作为本地指令清空输入；其余步骤都不得改动输入缓冲。
                let input = match step {
                    Help => {
                        app.input.set_text("/help");
                        assert!(app.apply_action(KeyAction::Submit).is_none());
                        String::new()
                    }
                    Notice(level) => {
                        app.push_status(status(level, "status-marker"));
                        draft
                    }
                    Space => {
                        press(&mut app, KeyCode::Char(' '));
                        draft
                    }
                    Esc => {
                        press(&mut app, KeyCode::Esc);
                        draft
                    }
                };
                assert_eq!(app.input.text(), input, "{label}");
                assert_eq!(dock_title(&mut app), top, "{label}");
                assert_eq!(app.dock.shows_input(), top == "USER", "{label}");
                assert_eq!(app.focus, Focus::Dock, "{label}");
            }
            // 面板不是状态消息：关闭后对话记录里没有帮助文字。
            assert!(!transcript(&app).contains("/reconnect"), "{name}");
            // 回到输入区后恢复普通编辑。
            let draft = app.input.text();
            press(&mut app, KeyCode::Char('x'));
            assert_eq!(app.input.text(), format!("{draft}x"), "{name}");
        }
    }

    /// 启动横幅固定在对话记录最上方：新视图丢弃条目与插入更早的轮都不把它当作状态消息重新排位。
    #[test]
    fn startup_banner_stays_on_top_across_new_views_and_older_turns() {
        let mut app = test_app(true);
        app.show_banner();
        app.push_status(status(NoticeLevel::Info, "before-view"));
        let turn = |n: u64| TurnId::parse(&format!("33333333-3333-4333-8333-{n:012}")).unwrap();
        let entries = |turns: std::ops::RangeInclusive<u64>| {
            turns
                .flat_map(|n| {
                    [
                        user(2 * n - 1, turn(n), n, &format!("prompt-{n}")),
                        step(2 * n, turn(n), 0)
                            .text(&format!("answer-{n}"))
                            .finished()
                            .entry(),
                    ]
                })
                .collect::<Vec<_>>()
        };
        for turns in [1..=1, 2..=3] {
            open_view(
                &mut app,
                head(2 * turns.end(), Some(turn(*turns.end())), false),
                entries(turns),
            );
        }
        app.term_height = 100;
        app.apply_action(KeyAction::ScrollPageUp);
        let request = app.older_request.take().expect("第 1 轮尚未装入");
        app.apply_older(
            request,
            Ok(LocalHistory {
                entries: entries(1..=1),
            }),
        );
        let lines = crate::view::transcript_lines(&app, 78)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            lines.iter().take(4).map(String::as_str).collect::<Vec<_>>(),
            [
                "LATTICE",
                "/help  显示命令",
                "/quit  退出",
                "/reconnect  重新读取状态并观察"
            ]
        );
        assert_eq!(
            lines.iter().filter(|line| line.contains("LATTICE")).count(),
            1
        );
        // 状态消息出现后不移动：它在视图开始前出现，插入的更早的轮也排在它之后。
        assert!(row(&app, "before-view") < row(&app, "prompt-1"));
        assert!(row(&app, "prompt-1") < row(&app, "prompt-2"));
    }

    /// WARN / ERROR 下屏蔽不可见输入编辑，全局动作照常。
    #[test]
    fn dock_status_blocks_invisible_input_but_keeps_globals() {
        for level in [NoticeLevel::Warn, NoticeLevel::Error] {
            let mut app = test_app(true);
            app.input.set_text("draft");
            app.push_status(status(level, "blocked"));
            for code in [
                KeyCode::Char('x'),
                KeyCode::Esc,
                KeyCode::Enter,
                KeyCode::Backspace,
                KeyCode::Delete,
                KeyCode::Left,
                KeyCode::Right,
                KeyCode::Up,
                KeyCode::Down,
                KeyCode::Home,
                KeyCode::End,
            ] {
                assert!(
                    handle_input_event(
                        &mut app,
                        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
                    )
                    .is_none()
                );
            }
            for modifiers in [
                KeyModifiers::SHIFT,
                KeyModifiers::ALT,
                KeyModifiers::CONTROL,
            ] {
                handle_input_event(
                    &mut app,
                    Event::Key(KeyEvent::new(KeyCode::Char(' '), modifiers)),
                );
            }
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL)),
            );
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL)),
            );
            handle_input_event(&mut app, Event::Paste("hidden\npaste".into()));
            assert_eq!(app.input.text(), "draft");
            assert_eq!(app.input.cursor, (0, 5));
            assert!(app.dock.message_id().is_some());
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
            );
            assert!(!app.viewport.following());
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL)),
            );
            assert!(app.viewport.following());
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            );
            assert_eq!(
                app.input.text(),
                "draft",
                "空闲 Warn/Error 下 Ctrl+C 不清空草稿"
            );
            assert!(app.last_ctrl_c.is_some() && !app.should_quit);
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            );
            assert!(app.should_quit);
            app.should_quit = false;
            app.last_ctrl_c = None;
            let id = lattice_api::local::new_turn_id();
            app.apply_view(LocalUpdate::Head(head(2, Some(id), true)));
            app.apply_view(entry_update(user(1, id, 1, "prompt")));
            app.apply_view(entry_update(step(2, id, 0).entry()));
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            );
            assert_eq!(app.cancel_requested, Some(CancelTarget::Other(id)));
            assert_eq!(app.input.text(), "draft");
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            );
            assert!(app.dock.shows_input());
            assert_eq!(app.input.text(), "draft", "释放键被消费");
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            );
            assert_eq!(app.input.text(), "draftx", "释放后恢复普通字符编辑");
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            );
            assert_eq!(app.input.text(), "draftx ");
        }
    }
}
