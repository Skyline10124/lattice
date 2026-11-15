// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 流式草稿节流与保活、活跃草稿停止按钮映射、最终回复组装、4096 字符三级切分与可靠重试发送。

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use lattice_ipc::{PublicTurnOutcome, TurnId};
use lattice_server::{InProcessTurn, InProcessTurnOutcome};
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::{
    identity::ChatKey,
    redact::ImRedactor,
    telegram::{TelegramApiError, TelegramIo},
};

/// Telegram 单条文本消息与草稿正文的最大 Unicode 字符数。
pub(crate) const MAX_TELEGRAM_MESSAGE_CHARS: usize = 4096;

/// 单个对话共享的流式草稿最小刷新间隔（每秒至多 1 条）。
pub(crate) const DRAFT_THROTTLE_INTERVAL: Duration = Duration::from_secs(1);

/// 流式草稿正文未变化时的保活刷新周期（早于 Telegram 约 30 秒草稿失效窗口）。
pub(crate) const DRAFT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(25);

const INITIAL_SEND_BACKOFF: Duration = Duration::from_millis(200);
const MAX_SEND_BACKOFF: Duration = Duration::from_secs(30);

/// 截取草稿正文的末尾 4096 个 Unicode 字符（按字符边界截取，绝不切碎多字节字符）。
pub(crate) fn truncate_draft_tail(text: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= MAX_TELEGRAM_MESSAGE_CHARS {
        return text.to_owned();
    }
    text.chars()
        .skip(char_count - MAX_TELEGRAM_MESSAGE_CHARS)
        .collect()
}

fn random_nonzero_draft_id() -> i64 {
    loop {
        let candidate = ((uuid::Uuid::new_v4().as_u128() as u32) & 0x7fff_ffff) as i64;
        if candidate > 0 {
            return candidate;
        }
    }
}

/// 内存中维护本实例发起的活跃轮次草稿映射 `(ChatKey, draft_id) -> TurnId`。
///
/// 每轮草稿编号由操作系统随机源生成正 31 位非零整数，避免重启或重新挂载后新实例从固定起点分配而与旧实例迟到停止按钮碰撞。
#[derive(Default)]
pub(crate) struct ActiveDrafts {
    entries: StdMutex<BTreeMap<(ChatKey, i64), TurnId>>,
}

impl ActiveDrafts {
    /// 为本实例新接受的轮次分配随机非零 `draft_id` 并登记映射，返回守卫在轮次结束时自动清理。
    pub(crate) fn register(self: &Arc<Self>, chat: ChatKey, turn_id: TurnId) -> DraftLease {
        let mut guard = self.entries.lock().expect("活跃草稿映射锁不应中毒");
        let draft_id = loop {
            let candidate = random_nonzero_draft_id();
            if !guard.contains_key(&(chat.clone(), candidate)) {
                break candidate;
            }
        };
        guard.insert((chat.clone(), draft_id), turn_id);
        drop(guard);
        DraftLease {
            registry: Arc::clone(self),
            chat,
            draft_id,
        }
    }

    /// 按对话与 `draft_id` 查找本实例正在执行的 `TurnId`。
    pub(crate) fn lookup(&self, chat: &ChatKey, draft_id: i64) -> Option<TurnId> {
        if draft_id <= 0 {
            return None;
        }
        self.entries
            .lock()
            .expect("活跃草稿映射锁不应中毒")
            .get(&(chat.clone(), draft_id))
            .copied()
    }
}

/// 绑定单轮生命周期的活跃草稿登记守卫，释放时仅移除本轮自身的 `draft_id`。
pub(crate) struct DraftLease {
    registry: Arc<ActiveDrafts>,
    chat: ChatKey,
    draft_id: i64,
}

impl DraftLease {
    pub(crate) fn draft_id(&self) -> i64 {
        self.draft_id
    }
}

impl Drop for DraftLease {
    fn drop(&mut self) {
        self.registry
            .entries
            .lock()
            .expect("活跃草稿映射锁不应中毒")
            .remove(&(self.chat.clone(), self.draft_id));
    }
}

/// 按 `ChatKey` 共享的 1 秒草稿节流表：同一对话下新旧会话的轮次共同遵守每秒至多 1 条限制。
#[derive(Default)]
pub(crate) struct ChatDraftThrottleMap {
    chats: StdMutex<BTreeMap<ChatKey, Arc<AsyncMutex<Option<tokio::time::Instant>>>>>,
}

impl ChatDraftThrottleMap {
    fn bucket(&self, chat: &ChatKey) -> Arc<AsyncMutex<Option<tokio::time::Instant>>> {
        let mut guard = self.chats.lock().expect("草稿节流表锁不应中毒");
        Arc::clone(
            guard
                .entry(chat.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(None))),
        )
    }

    async fn earliest_allowed(&self, chat: &ChatKey) -> tokio::time::Instant {
        let bucket = self.bucket(chat);
        let guard = bucket.lock().await;
        let now = tokio::time::Instant::now();
        match *guard {
            Some(until) if until > now => until,
            _ => now,
        }
    }

    async fn try_claim_now(&self, chat: &ChatKey) -> Result<(), tokio::time::Instant> {
        let bucket = self.bucket(chat);
        let mut guard = bucket.lock().await;
        let now = tokio::time::Instant::now();
        if let Some(until) = *guard
            && until > now
        {
            return Err(until);
        }
        *guard = Some(now + DRAFT_THROTTLE_INTERVAL);
        Ok(())
    }
}

pub(crate) async fn wait_stop_signal(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

enum DraftPaceStep {
    Stopped,
    ContinueNow,
    WaitUntil(Option<tokio::time::Instant>),
}

/// 单轮草稿节流与 25 秒保活状态机。
struct DraftPacer<'a> {
    chat: &'a ChatKey,
    draft_id: i64,
    throttles: &'a ChatDraftThrottleMap,
    latest_text: Option<String>,
    dirty: bool,
    last_sent_at: Option<tokio::time::Instant>,
}

impl<'a> DraftPacer<'a> {
    fn new(
        initial_draft: Option<String>,
        chat: &'a ChatKey,
        draft_id: i64,
        throttles: &'a ChatDraftThrottleMap,
    ) -> Self {
        let latest_text = initial_draft.filter(|s| !s.is_empty());
        let dirty = latest_text.is_some();
        Self {
            chat,
            draft_id,
            throttles,
            latest_text,
            dirty,
            last_sent_at: None,
        }
    }

    fn record_snapshot(&mut self, text: String) {
        self.latest_text = Some(text);
        self.dirty = true;
    }

    async fn poll_send_step(
        &mut self,
        io: &dyn TelegramIo,
        stop: &mut watch::Receiver<bool>,
    ) -> DraftPaceStep {
        if *stop.borrow() {
            return DraftPaceStep::Stopped;
        }

        let want_send_at = if self.dirty {
            Some(self.throttles.earliest_allowed(self.chat).await)
        } else if let Some(sent_at) = self.last_sent_at {
            let keepalive_at = sent_at + DRAFT_KEEPALIVE_INTERVAL;
            let allowed_at = self.throttles.earliest_allowed(self.chat).await;
            Some(keepalive_at.max(allowed_at))
        } else {
            None
        };

        if let Some(deadline) = want_send_at
            && tokio::time::Instant::now() >= deadline
        {
            match self.throttles.try_claim_now(self.chat).await {
                Ok(()) => {
                    let preview = truncate_draft_tail(self.latest_text.as_deref().unwrap_or(""));
                    self.dirty = false;
                    self.last_sent_at = Some(tokio::time::Instant::now());
                    tokio::select! {
                        _ = wait_stop_signal(stop) => return DraftPaceStep::Stopped,
                        res = io.send_message_draft(self.chat, self.draft_id, &preview, true) => {
                            if let Err(err) = res {
                                tracing::debug!(
                                    chat = %self.chat,
                                    draft_id = self.draft_id,
                                    diagnostic = %err.diagnostic(),
                                    "推送 Telegram 草稿失败，等待后续刷新"
                                );
                            }
                        }
                    }
                    return DraftPaceStep::ContinueNow;
                }
                Err(_) => {
                    // 同一 chat 下的并发轮次刚占用了当前秒配额，下一轮循环重新按最新时间等待并继续合并。
                    return DraftPaceStep::ContinueNow;
                }
            }
        }

        DraftPaceStep::WaitUntil(want_send_at)
    }
}

/// 驱动单轮流式草稿推送：合并节流窗口内的中间快照，并在无新增量时每 25 秒保活刷新。
///
/// 草稿发送失败仅记录调试日志且不重试，等待下一次正文更新或保活周期刷新。
pub(crate) async fn drive_turn_drafts(
    turn: &mut InProcessTurn,
    initial_draft: Option<String>,
    chat: &ChatKey,
    draft_id: i64,
    io: &dyn TelegramIo,
    throttles: &ChatDraftThrottleMap,
    mut stop: watch::Receiver<bool>,
) {
    let mut pacer = DraftPacer::new(initial_draft, chat, draft_id, throttles);

    loop {
        match pacer.poll_send_step(io, &mut stop).await {
            DraftPaceStep::Stopped => return,
            DraftPaceStep::ContinueNow => continue,
            DraftPaceStep::WaitUntil(Some(deadline)) => {
                tokio::select! {
                    _ = wait_stop_signal(&mut stop) => return,
                    next = turn.next_draft() => {
                        match next {
                            Some(text) => pacer.record_snapshot(text),
                            None => return,
                        }
                    }
                    _ = tokio::time::sleep_until(deadline) => {}
                }
            }
            DraftPaceStep::WaitUntil(None) => {
                tokio::select! {
                    _ = wait_stop_signal(&mut stop) => return,
                    next = turn.next_draft() => {
                        match next {
                            Some(text) => pacer.record_snapshot(text),
                            None => return,
                        }
                    }
                }
            }
        }
    }
}

/// 组装轮次结束后的最终纯文本回复：非 `Completed` 时附一行脱敏后的中文结局说明，不附用量。
pub(crate) fn compose_final_reply(outcome: &InProcessTurnOutcome, redactor: &ImRedactor) -> String {
    let body = &outcome.assistant_text;
    match outcome.detail.summary.outcome {
        PublicTurnOutcome::Completed => {
            if body.is_empty() {
                "（本轮已完成，模型未输出文本正文）".to_owned()
            } else {
                body.clone()
            }
        }
        PublicTurnOutcome::Cancelled => {
            let detail = extract_outcome_detail(outcome, redactor);
            let note = if detail.is_empty() {
                "［本轮已取消］".to_owned()
            } else {
                format!("［本轮已取消：{detail}］")
            };
            append_outcome_note(body, &note)
        }
        PublicTurnOutcome::Failed => {
            let detail = extract_outcome_detail(outcome, redactor);
            let note = if detail.is_empty() {
                "［本轮执行失败］".to_owned()
            } else {
                format!("［本轮执行失败：{detail}］")
            };
            append_outcome_note(body, &note)
        }
    }
}

fn extract_outcome_detail(outcome: &InProcessTurnOutcome, redactor: &ImRedactor) -> String {
    if let Some(diag) = &outcome.diagnostic {
        let cleaned = redactor.sanitize_single_line(&diag.message);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    if let Some(notice) = &outcome.detail.summary.notice {
        let cleaned = redactor.sanitize_single_line(&notice.text);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    String::new()
}

fn append_outcome_note(body: &str, note: &str) -> String {
    if body.is_empty() {
        note.to_owned()
    } else {
        format!("{body}\n\n{note}")
    }
}

/// 将任意非空回复无损切分为每段 `1..=4096` 个 Unicode 字符的有序消息序列（拼接所有分片精确还原原文）。
///
/// 切分优先级：段落边界（`\n\n`） -> 行边界（`\n`） -> 4096 字符硬切。
pub(crate) fn split_message_chunks(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.chars().count() <= MAX_TELEGRAM_MESSAGE_CHARS {
            chunks.push(remaining.to_owned());
            break;
        }

        let window_end = nth_char_byte_index(remaining, MAX_TELEGRAM_MESSAGE_CHARS);
        let window = &remaining[..window_end];

        if let Some(para_pos) = window.rfind("\n\n") {
            let split_at = para_pos + 2;
            if !window[..split_at].trim().is_empty() {
                chunks.push(window[..split_at].to_owned());
                remaining = &remaining[split_at..];
                continue;
            }
        }

        if let Some(line_pos) = window.rfind('\n') {
            let split_at = line_pos + 1;
            if !window[..split_at].trim().is_empty() {
                chunks.push(window[..split_at].to_owned());
                remaining = &remaining[split_at..];
                continue;
            }
        }

        chunks.push(window.to_owned());
        remaining = &remaining[window_end..];
    }

    chunks
}

fn nth_char_byte_index(text: &str, max_chars: usize) -> usize {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => idx,
        None => text.len(),
    }
}

/// 可靠发送整段或多段文本的最终结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SendDisposition {
    /// 全部切片已由 Telegram 接收。
    Delivered,
    /// Telegram 返回 HTTP 400 或 403 明确拒收，不再重试，调用方可推进台账为 `Delivered`。
    PlatformRejected,
}

/// 发送过程被组件停止信号打断，尚未完成投递（关联台账须保留原相位以便下次补发）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SendInterrupted;

/// 按 4096 规则切分并按序可靠发送文本：对 `429`（完整保留服务端 `retry_after`）与 `5xx`/传输错误退避重试，对 `400`/`403` 记警告并终止。
pub(crate) async fn send_text_reliably(
    io: &dyn TelegramIo,
    chat: &ChatKey,
    text: &str,
    mut stop: watch::Receiver<bool>,
) -> Result<SendDisposition, SendInterrupted> {
    let chunks = split_message_chunks(text);
    for chunk in &chunks {
        let mut backoff = INITIAL_SEND_BACKOFF;
        loop {
            if *stop.borrow() {
                return Err(SendInterrupted);
            }
            let send_res = tokio::select! {
                _ = wait_stop_signal(&mut stop) => return Err(SendInterrupted),
                res = io.send_message(chat, chunk) => res,
            };

            match send_res {
                Ok(()) => break,
                Err(TelegramApiError::Rejected { status, diagnostic }) => {
                    tracing::warn!(
                        chat = %chat,
                        status,
                        diagnostic = %diagnostic,
                        "Telegram 拒收回复消息，视为平台拒收终态并不再重试"
                    );
                    return Ok(SendDisposition::PlatformRejected);
                }
                Err(TelegramApiError::RateLimited {
                    retry_after,
                    diagnostic,
                }) => {
                    let wait = retry_after.unwrap_or(backoff);
                    tracing::warn!(
                        chat = %chat,
                        wait_ms = wait.as_millis() as u64,
                        diagnostic = %diagnostic,
                        "Telegram 发送消息触发限流，退避后重试"
                    );
                    tokio::select! {
                        _ = wait_stop_signal(&mut stop) => return Err(SendInterrupted),
                        _ = tokio::time::sleep(wait) => {}
                    }
                    backoff = backoff.saturating_mul(2).min(MAX_SEND_BACKOFF);
                }
                Err(
                    TelegramApiError::Retryable { diagnostic }
                    | TelegramApiError::UnauthorizedOrInvalid { diagnostic, .. },
                ) => {
                    tracing::warn!(
                        chat = %chat,
                        wait_ms = backoff.as_millis() as u64,
                        diagnostic = %diagnostic,
                        "Telegram 发送消息失败，退避后重试"
                    );
                    tokio::select! {
                        _ = wait_stop_signal(&mut stop) => return Err(SendInterrupted),
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = backoff.saturating_mul(2).min(MAX_SEND_BACKOFF);
                }
            }
        }
    }
    Ok(SendDisposition::Delivered)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use chrono::Utc;
    use lattice_ipc::{
        AgentName, CostSummary, LocalError, LocalErrorKind, NodeId, Notice, NoticeLevel,
        StepProgress, TokenSummary, TurnDetail, TurnSummary, UsageCompleteness, UsageSummary,
    };
    use lattice_server::ImToken;
    use tokio::sync::mpsc;

    use super::*;
    use crate::{
        identity::{new_session_id, new_turn_id},
        telegram::TelegramUpdate,
    };

    #[test]
    fn splits_4096_preferring_paragraphs_then_lines_then_multibyte_chars_without_losing_newlines() {
        // 1. 段落边界切分优先且完整保留分隔换行
        let p1 = "甲".repeat(2500);
        let p2 = "乙".repeat(1500);
        let p3 = "丙".repeat(1200);
        let para_text = format!("{p1}\n\n{p2}\n\n{p3}");
        let para_chunks = split_message_chunks(&para_text);
        assert_eq!(para_chunks.len(), 2, "前两段合起来不足 4096 时应合并为首条");
        assert_eq!(para_chunks[0], format!("{p1}\n\n{p2}\n\n"));
        assert_eq!(para_chunks[1], p3);
        assert_eq!(para_chunks.concat(), para_text, "段落切分拼接必须还原原文");
        assert!(para_chunks.iter().all(|c| c.chars().count() <= 4096));

        // 核对接近 4096 边界的段落分隔反例：4090 个“甲” + 两个换行 + 10 个“乙”（共 4102 字符）
        let edge_para = format!("{}\n\n{}", "甲".repeat(4090), "乙".repeat(10));
        let edge_chunks = split_message_chunks(&edge_para);
        assert_eq!(edge_chunks.len(), 2);
        assert_eq!(edge_chunks.concat().chars().count(), 4102);
        assert_eq!(edge_chunks.concat(), edge_para);

        // 2. 单段超长时退化为行边界切分且完整保留换行符
        let l1 = "丁".repeat(2200);
        let l2 = "戊".repeat(1800);
        let l3 = "己".repeat(1500);
        let line_text = format!("{l1}\n{l2}\n{l3}");
        let line_chunks = split_message_chunks(&line_text);
        assert_eq!(line_chunks.len(), 2);
        assert_eq!(line_chunks[0], format!("{l1}\n{l2}\n"));
        assert_eq!(line_chunks[1], l3);
        assert_eq!(line_chunks.concat(), line_text, "行切分拼接必须还原原文");

        // 3. 前置空白换行不产生纯空白孤立消息，同时拼接还原原文
        let leading_nl = format!("\n\n{}", "庚".repeat(5000));
        let leading_chunks = split_message_chunks(&leading_nl);
        assert!(leading_chunks.iter().all(|c| !c.trim().is_empty()));
        assert_eq!(leading_chunks.concat(), leading_nl);

        // 4. 无换行的多字节中文与 Emoji 超长单行硬切不碎字符
        let unit = "中🎉"; // 2 个 Unicode 字符（7 个 UTF-8 字节）
        let hard_text = unit.repeat(4500); // 9000 个字符
        let hard_chunks = split_message_chunks(&hard_text);
        assert_eq!(hard_chunks.len(), 3);
        assert_eq!(hard_chunks[0].chars().count(), 4096);
        assert_eq!(hard_chunks[1].chars().count(), 4096);
        assert_eq!(hard_chunks[2].chars().count(), 808);
        assert_eq!(
            hard_chunks.concat(),
            hard_text,
            "硬切拼接后须与原文逐字符一致"
        );
    }

    #[test]
    fn composes_non_completed_outcome_suffix_within_4096_split_and_redacts() {
        let token = ImToken::parse("secret-tok-77").unwrap();
        let redactor = ImRedactor::new([token]);
        let empty_tok = TokenSummary {
            known: Some(0),
            complete: true,
            overflowed: false,
        };
        let outcome = InProcessTurnOutcome {
            detail: TurnDetail {
                turn_id: new_turn_id(),
                session_id: new_session_id(),
                user_node: NodeId(1),
                agent_name: AgentName::parse("test").unwrap(),
                accepted_at: Utc::now(),
                ended_at: Utc::now(),
                summary: TurnSummary {
                    outcome: PublicTurnOutcome::Failed,
                    notice: Some(Notice {
                        level: NoticeLevel::Error,
                        text: "上游错误 secret-tok-77".into(),
                    }),
                    progress: StepProgress::Reserved,
                },
                usage: Box::new(UsageSummary {
                    attempts: 0,
                    partial_attempts: 0,
                    kind: UsageCompleteness::Complete,
                    input: empty_tok,
                    cache_read: empty_tok,
                    cache_write: empty_tok,
                    output: empty_tok,
                    reasoning: empty_tok,
                    cost: CostSummary {
                        known: None,
                        unknown_attempts: 0,
                        overflowed: false,
                    },
                }),
                steps: Vec::new(),
            },
            assistant_text: "前序输出".repeat(1022), // 4088 个字符，附结局说明后超过 4096
            diagnostic: Some(LocalError::new(
                LocalErrorKind::Internal,
                "连接中断 secret-tok-77",
            )),
        };

        let composed = compose_final_reply(&outcome, &redactor);
        assert!(!composed.contains("secret-tok-77"));
        assert!(composed.contains("［本轮执行失败：连接中断 [REDACTED]］"));

        let chunks = split_message_chunks(&composed);
        assert_eq!(
            chunks.len(),
            2,
            "附上结局说明后超 4096 字符应自动切分为两条"
        );
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(
            chunks.concat(),
            composed,
            "拼接分片必须与组装后的回复完全一致"
        );
        assert_eq!(chunks[1], "［本轮执行失败：连接中断 [REDACTED]］");
    }

    #[test]
    fn active_drafts_allocates_positive_random_ids_across_instances() {
        let chat = ChatKey::from_platform_id("tg", "1001");
        let inst_a = Arc::new(ActiveDrafts::default());
        let inst_b = Arc::new(ActiveDrafts::default());

        let lease_a = inst_a.register(chat.clone(), new_turn_id());
        let lease_b = inst_b.register(chat.clone(), new_turn_id());
        assert!(lease_a.draft_id() > 0 && lease_a.draft_id() <= i64::from(i32::MAX));
        assert!(lease_b.draft_id() > 0 && lease_b.draft_id() <= i64::from(i32::MAX));
        assert_ne!(
            lease_a.draft_id(),
            lease_b.draft_id(),
            "跨实例草稿编号应由随机源独立分配，避免重挂后从固定起点复用"
        );
        assert_eq!(
            inst_b.lookup(&chat, lease_a.draft_id()),
            None,
            "新实例不得将旧实例的 draft_id 误认为本实例轮次"
        );
    }

    type RecordedDraft = (tokio::time::Instant, ChatKey, i64, String, bool);
    type RecordedSendAttempt = (tokio::time::Instant, String);

    #[derive(Default)]
    struct RecordingDraftIo {
        drafts: StdMutex<Vec<RecordedDraft>>,
        send_outcomes: StdMutex<VecDeque<Result<(), TelegramApiError>>>,
        send_attempts: StdMutex<Vec<RecordedSendAttempt>>,
        sent_messages: StdMutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl TelegramIo for RecordingDraftIo {
        async fn get_updates(
            &self,
            _timeout_secs: u32,
        ) -> Result<Vec<TelegramUpdate>, TelegramApiError> {
            Ok(Vec::new())
        }

        fn acknowledge(&self, _inbound_id: &crate::identity::InboundId) {}

        async fn send_message(&self, _chat: &ChatKey, text: &str) -> Result<(), TelegramApiError> {
            self.send_attempts
                .lock()
                .unwrap()
                .push((tokio::time::Instant::now(), text.to_owned()));
            if let Some(next) = self.send_outcomes.lock().unwrap().pop_front() {
                next?;
            }
            self.sent_messages.lock().unwrap().push(text.to_owned());
            Ok(())
        }

        async fn send_message_draft(
            &self,
            chat: &ChatKey,
            draft_id: i64,
            text: &str,
            can_stop: bool,
        ) -> Result<(), TelegramApiError> {
            self.drafts.lock().unwrap().push((
                tokio::time::Instant::now(),
                chat.clone(),
                draft_id,
                text.to_owned(),
                can_stop,
            ));
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn draft_pacer_throttles_to_1s_coalesces_keeps_alive_at_25s_and_truncates_tail() {
        let start = tokio::time::Instant::now();
        let io = Arc::new(RecordingDraftIo::default());
        let throttles = Arc::new(ChatDraftThrottleMap::default());
        let chat = ChatKey::from_platform_id("tg", "888");
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_stop_tx, mut stop_rx) = watch::channel(false);

        let io_clone = Arc::clone(&io);
        let throttles_clone = Arc::clone(&throttles);
        let chat_clone = chat.clone();
        let driver = tokio::spawn(async move {
            let mut pacer = DraftPacer::new(None, &chat_clone, 42, &throttles_clone);
            loop {
                match pacer.poll_send_step(io_clone.as_ref(), &mut stop_rx).await {
                    DraftPaceStep::Stopped => return,
                    DraftPaceStep::ContinueNow => continue,
                    DraftPaceStep::WaitUntil(Some(deadline)) => {
                        tokio::select! {
                            _ = wait_stop_signal(&mut stop_rx) => return,
                            next = rx.recv() => match next {
                                Some(text) => pacer.record_snapshot(text),
                                None => return,
                            },
                            _ = tokio::time::sleep_until(deadline) => {}
                        }
                    }
                    DraftPaceStep::WaitUntil(None) => {
                        tokio::select! {
                            _ = wait_stop_signal(&mut stop_rx) => return,
                            next = rx.recv() => match next {
                                Some(text) => pacer.record_snapshot(text),
                                None => return,
                            },
                        }
                    }
                }
            }
        });

        // t=0：第一份快照立即发出
        tx.send("首段".into()).unwrap();
        tokio::task::yield_now().await;
        assert_eq!(io.drafts.lock().unwrap().len(), 1);
        assert_eq!(io.drafts.lock().unwrap()[0].3, "首段");
        assert!(io.drafts.lock().unwrap()[0].4, "草稿必须附带 can_stop=true");

        // 1 秒节流窗口内连续产生多份快照（最后一份超过 4096 字符），应合并并在 t=1s 只发最后一份末尾 4096 字
        tokio::time::advance(Duration::from_millis(200)).await;
        tx.send("中间态一".into()).unwrap();
        tokio::time::advance(Duration::from_millis(300)).await;
        let long_draft = format!("头部被截掉{}", "尾".repeat(4096));
        tx.send(long_draft).unwrap();
        tokio::task::yield_now().await;
        assert_eq!(
            io.drafts.lock().unwrap().len(),
            1,
            "未满 1 秒节流间隔前不得提前推送"
        );

        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
        {
            let recorded = io.drafts.lock().unwrap();
            assert_eq!(recorded.len(), 2, "满 1 秒后应发出合并后的最新草稿");
            assert_eq!(recorded[1].0.duration_since(start), Duration::from_secs(1));
            assert_eq!(recorded[1].3.chars().count(), 4096);
            assert!(
                !recorded[1].3.contains("头部"),
                "超过 4096 字符的草稿只能保留末尾 4096 字符"
            );
            assert_eq!(recorded[1].3, "尾".repeat(4096));
        }

        // 正文不再变化，推进 24 秒仍不重发，满 25 秒（即 t=26s）触发保活刷新
        tokio::time::advance(Duration::from_secs(24)).await;
        tokio::task::yield_now().await;
        assert_eq!(io.drafts.lock().unwrap().len(), 2);

        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        {
            let recorded = io.drafts.lock().unwrap();
            assert_eq!(
                recorded.len(),
                3,
                "无新增量的在途轮次应在 25 秒时保活刷新草稿"
            );
            assert_eq!(recorded[2].0.duration_since(start), Duration::from_secs(26));
        }

        drop(tx);
        driver.await.expect("草稿驱动任务应正常收尾");
    }

    #[tokio::test(start_paused = true)]
    async fn send_text_reliably_honors_unclamped_429_retry_after_and_treats_403_as_terminal() {
        let start = tokio::time::Instant::now();
        let io = RecordingDraftIo::default();
        let chat = ChatKey::from_platform_id("tg", "999");
        let (_stop_tx, stop_rx) = watch::channel(false);

        {
            let mut outcomes = io.send_outcomes.lock().unwrap();
            // 服务端要求 120 秒（超过本地 30 秒退避上限），必须完整等待 120 秒后才重试
            outcomes.push_back(Err(TelegramApiError::RateLimited {
                retry_after: Some(Duration::from_secs(120)),
                diagnostic: "429".into(),
            }));
            outcomes.push_back(Err(TelegramApiError::Retryable {
                diagnostic: "502".into(),
            }));
            outcomes.push_back(Ok(()));
        }

        let res = send_text_reliably(&io, &chat, "重试后成功", stop_rx.clone())
            .await
            .expect("不应被停止打断");
        assert_eq!(res, SendDisposition::Delivered);
        assert_eq!(io.sent_messages.lock().unwrap().as_slice(), &["重试后成功"]);
        {
            let attempts = io.send_attempts.lock().unwrap();
            assert_eq!(attempts.len(), 3);
            assert_eq!(attempts[0].0.duration_since(start), Duration::ZERO);
            assert_eq!(
                attempts[1].0.duration_since(start),
                Duration::from_secs(120),
                "HTTP 429 指定 retry_after=120s 时不得被本地 30s 上限截断"
            );
            assert_eq!(
                attempts[2].0.duration_since(start),
                Duration::from_secs(120) + INITIAL_SEND_BACKOFF * 2,
                "后续无 retry_after 的 502 错误使用受限本地退避"
            );
        }

        // 403 拒收：不重试，直接返回 PlatformRejected
        {
            let mut outcomes = io.send_outcomes.lock().unwrap();
            outcomes.push_back(Err(TelegramApiError::Rejected {
                status: 403,
                diagnostic: "用户已屏蔽机器人".into(),
            }));
            outcomes.push_back(Ok(()));
        }
        let rejected = send_text_reliably(&io, &chat, "被拒消息", stop_rx)
            .await
            .expect("不应被停止打断");
        assert_eq!(rejected, SendDisposition::PlatformRejected);
        assert_eq!(
            io.sent_messages.lock().unwrap().len(),
            1,
            "403 平台拒收后不得继续重试发送"
        );
    }
}
