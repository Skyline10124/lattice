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

#[derive(Clone, Copy, Default)]
enum DeadlineBound {
    #[default]
    Ready,
    Until(tokio::time::Instant),
    Indefinite,
}

/// 按 `ChatKey` 共享的 1 秒草稿节流表：同一对话下新旧会话的轮次共同遵守每秒至多 1 条限制。
#[derive(Default)]
pub(crate) struct ChatDraftThrottleMap {
    chats: StdMutex<BTreeMap<ChatKey, Arc<AsyncMutex<DeadlineBound>>>>,
}

impl ChatDraftThrottleMap {
    fn bucket(&self, chat: &ChatKey) -> Arc<AsyncMutex<DeadlineBound>> {
        let mut guard = self.chats.lock().expect("草稿节流表锁不应中毒");
        Arc::clone(
            guard
                .entry(chat.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(DeadlineBound::Ready))),
        )
    }

    async fn earliest_allowed(&self, chat: &ChatKey) -> Option<tokio::time::Instant> {
        let bucket = self.bucket(chat);
        let guard = bucket.lock().await;
        let now = tokio::time::Instant::now();
        match *guard {
            DeadlineBound::Ready => Some(now),
            DeadlineBound::Until(until) if until > now => Some(until),
            DeadlineBound::Until(_) => Some(now),
            DeadlineBound::Indefinite => None,
        }
    }

    async fn try_claim_now(&self, chat: &ChatKey) -> bool {
        let bucket = self.bucket(chat);
        let mut guard = bucket.lock().await;
        let now = tokio::time::Instant::now();
        match *guard {
            DeadlineBound::Indefinite => return false,
            DeadlineBound::Until(until) if until > now => return false,
            DeadlineBound::Ready | DeadlineBound::Until(_) => {}
        }
        *guard = DeadlineBound::Until(now + DRAFT_THROTTLE_INTERVAL);
        true
    }

    async fn record_rate_limit(&self, chat: &ChatKey, retry_until: Option<tokio::time::Instant>) {
        let bucket = self.bucket(chat);
        let mut guard = bucket.lock().await;
        *guard = match (*guard, retry_until) {
            (DeadlineBound::Indefinite, _) | (_, None) => DeadlineBound::Indefinite,
            (DeadlineBound::Until(existing), Some(until)) if existing > until => {
                DeadlineBound::Until(existing)
            }
            (_, Some(until)) => DeadlineBound::Until(until),
        };
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

/// 等待指定退避时长或停机信号：等待期满返回 `true`；收到停机信号（或 `wait` 超出 `Instant` 表示范围后等待停机唤醒）返回 `false`。
pub(crate) async fn sleep_or_stop(wait: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    let Some(deadline) = tokio::time::Instant::now().checked_add(wait) else {
        wait_stop_signal(stop).await;
        return false;
    };
    tokio::select! {
        biased;
        _ = wait_stop_signal(stop) => false,
        _ = tokio::time::sleep_until(deadline) => true,
    }
}

/// 单轮草稿节流、重试清空与 25 秒保活状态机。
///
/// - **清空更新**：模型重试或步骤切换产生空快照 `""` 时，若此前已有草稿快照则作为清空更新发往 Telegram，消除失败尝试残留正文。
/// - **成功保活与失败退避分离**：成功请求按**发起时刻** `started_at` 计算 25 秒保活（避免慢请求把耗时叠加到刷新周期而越过 Telegram 30 秒草稿有效期）；
///   失败请求不立即重试，按指数退避（及 429 `retry_after`）延迟后续发送，并在保活窗口已过时以失败完成时刻起算下一轮保活，避免慢超时后紧循环。
struct DraftPacer<'a> {
    chat: &'a ChatKey,
    draft_id: i64,
    throttles: &'a ChatDraftThrottleMap,
    latest_text: Option<String>,
    dirty: bool,
    last_sent_at: Option<tokio::time::Instant>,
    consecutive_failures: u32,
    backoff: DeadlineBound,
}

impl<'a> DraftPacer<'a> {
    fn new(chat: &'a ChatKey, draft_id: i64, throttles: &'a ChatDraftThrottleMap) -> Self {
        Self {
            chat,
            draft_id,
            throttles,
            latest_text: None,
            dirty: false,
            last_sent_at: None,
            consecutive_failures: 0,
            backoff: DeadlineBound::Ready,
        }
    }

    fn record_snapshot(&mut self, text: String) {
        if text.is_empty() && self.latest_text.is_none() {
            return;
        }
        if self.latest_text.as_deref() != Some(text.as_str()) {
            self.latest_text = Some(text);
            self.dirty = true;
        }
    }

    async fn next_deadline(&self) -> Option<tokio::time::Instant> {
        self.latest_text.as_ref()?;
        let allowed_at = self.throttles.earliest_allowed(self.chat).await?;
        let floor = match self.backoff {
            DeadlineBound::Ready => allowed_at,
            DeadlineBound::Until(until) => allowed_at.max(until),
            DeadlineBound::Indefinite => return None,
        };
        if self.dirty {
            Some(floor)
        } else if let Some(sent_at) = self.last_sent_at {
            let keepalive_at = sent_at + DRAFT_KEEPALIVE_INTERVAL;
            Some(keepalive_at.max(floor))
        } else {
            None
        }
    }

    async fn prepare_send(&mut self) -> Option<(String, tokio::time::Instant)> {
        let text = self.latest_text.as_deref()?;
        match self.backoff {
            DeadlineBound::Ready => {}
            DeadlineBound::Until(until) if tokio::time::Instant::now() >= until => {}
            DeadlineBound::Until(_) | DeadlineBound::Indefinite => return None,
        }
        if !self.throttles.try_claim_now(self.chat).await {
            return None;
        }
        let started_at = tokio::time::Instant::now();
        let preview = truncate_draft_tail(text);
        self.dirty = false;
        Some((preview, started_at))
    }

    async fn finish_send(
        &mut self,
        started_at: tokio::time::Instant,
        res: Result<(), TelegramApiError>,
    ) {
        match res {
            Ok(()) => {
                self.last_sent_at = Some(started_at);
                self.consecutive_failures = 0;
                self.backoff = DeadlineBound::Ready;
            }
            Err(err) => {
                let completed_at = tokio::time::Instant::now();
                if self
                    .last_sent_at
                    .is_none_or(|sent_at| completed_at >= sent_at + DRAFT_KEEPALIVE_INTERVAL)
                {
                    self.last_sent_at = Some(completed_at);
                }
                let shift = self.consecutive_failures.min(5);
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                let exp_backoff = DRAFT_THROTTLE_INTERVAL
                    .saturating_mul(1u32 << shift)
                    .min(DRAFT_KEEPALIVE_INTERVAL);
                let wait = match &err {
                    TelegramApiError::RateLimited {
                        retry_after: Some(ra),
                        ..
                    } => {
                        self.throttles
                            .record_rate_limit(self.chat, completed_at.checked_add(*ra))
                            .await;
                        (*ra).max(exp_backoff)
                    }
                    _ => exp_backoff,
                };
                self.backoff = match completed_at.checked_add(wait) {
                    Some(until) => DeadlineBound::Until(until),
                    None => DeadlineBound::Indefinite,
                };
                let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
                tracing::debug!(
                    chat = %self.chat,
                    draft_id = self.draft_id,
                    wait_ms,
                    diagnostic = %err.diagnostic(),
                    "推送 Telegram 草稿失败，退避后等待后续刷新"
                );
            }
        }
    }
}

/// 非阻塞排空 `turn` 当前已就绪的全部草稿快照；若轮次已收尾或会话库停写则返回 `false`。
fn drain_ready_turn_updates(turn: &mut InProcessTurn, pacer: &mut DraftPacer<'_>) -> bool {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    loop {
        let mut fut = std::pin::pin!(turn.next_draft());
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(Some(text)) => pacer.record_snapshot(text),
            std::task::Poll::Ready(None) => return false,
            std::task::Poll::Pending => return true,
        }
    }
}

/// 驱动单轮流式草稿推送：合并节流窗口内的中间快照，并在无新增量时每 25 秒保活刷新。
///
/// 草稿发送失败仅记录调试日志且不重试，等待下一次正文更新或保活周期刷新；
/// 一旦轮次进入终态，立即结束草稿循环（含取消在途慢草稿请求），确保最终回复不被阻塞。
pub(crate) async fn drive_turn_drafts(
    turn: &mut InProcessTurn,
    chat: &ChatKey,
    draft_id: i64,
    io: &dyn TelegramIo,
    throttles: &ChatDraftThrottleMap,
    mut stop: watch::Receiver<bool>,
) {
    let mut pacer = DraftPacer::new(chat, draft_id, throttles);

    loop {
        if *stop.borrow() || !drain_ready_turn_updates(turn, &mut pacer) {
            return;
        }

        let Some(deadline) = pacer.next_deadline().await else {
            tokio::select! {
                _ = wait_stop_signal(&mut stop) => return,
                next = turn.next_draft() => match next {
                    Some(text) => pacer.record_snapshot(text),
                    None => return,
                },
            }
            continue;
        };

        if tokio::time::Instant::now() < deadline {
            tokio::select! {
                _ = wait_stop_signal(&mut stop) => return,
                next = turn.next_draft() => match next {
                    Some(text) => pacer.record_snapshot(text),
                    None => return,
                },
                _ = tokio::time::sleep_until(deadline) => {}
            }
            continue;
        }

        let Some((preview, started_at)) = pacer.prepare_send().await else {
            continue;
        };

        let mut send_fut = std::pin::pin!(io.send_message_draft(chat, draft_id, &preview, true));
        let send_res = loop {
            tokio::select! {
                biased;
                _ = wait_stop_signal(&mut stop) => return,
                next = turn.next_draft() => match next {
                    Some(text) => pacer.record_snapshot(text),
                    None => return,
                },
                res = &mut send_fut => break res,
            }
        };

        pacer.finish_send(started_at, send_res).await;
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
            let detail = extract_raw_outcome_detail(outcome)
                .map(|raw| redactor.sanitize_single_line(raw))
                .unwrap_or_default();
            let note = if detail.is_empty() {
                "［本轮已取消］".to_owned()
            } else {
                format!("［本轮已取消：{detail}］")
            };
            append_outcome_note(body, &note)
        }
        PublicTurnOutcome::Failed => {
            let formatted = extract_raw_outcome_detail(outcome)
                .map(|raw| {
                    crate::command::format_backend_image_rejection(raw, redactor)
                        .unwrap_or_else(|| redactor.sanitize_single_line(raw))
                })
                .unwrap_or_default();
            let note = if formatted.is_empty() {
                "［本轮执行失败］".to_owned()
            } else {
                format!("［本轮执行失败：{formatted}］")
            };
            append_outcome_note(body, &note)
        }
    }
}

fn extract_raw_outcome_detail(outcome: &InProcessTurnOutcome) -> Option<&str> {
    if let Some(diag) = &outcome.diagnostic
        && diag
            .message
            .chars()
            .any(|ch| !ch.is_control() && !ch.is_whitespace())
    {
        return Some(&diag.message);
    }
    if let Some(notice) = &outcome.detail.summary.notice
        && notice
            .text
            .chars()
            .any(|ch| !ch.is_control() && !ch.is_whitespace())
    {
        return Some(&notice.text);
    }
    None
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
                    let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
                    tracing::warn!(
                        chat = %chat,
                        wait_ms,
                        diagnostic = %diagnostic,
                        "Telegram 发送消息触发限流，退避后重试"
                    );
                    if !sleep_or_stop(wait, &mut stop).await {
                        return Err(SendInterrupted);
                    }
                    backoff = backoff.saturating_mul(2).min(MAX_SEND_BACKOFF);
                }
                Err(
                    TelegramApiError::Retryable { diagnostic }
                    | TelegramApiError::UnauthorizedOrInvalid { diagnostic, .. },
                ) => {
                    let wait_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX);
                    tracing::warn!(
                        chat = %chat,
                        wait_ms,
                        diagnostic = %diagnostic,
                        "Telegram 发送消息失败，退避后重试"
                    );
                    if !sleep_or_stop(backoff, &mut stop).await {
                        return Err(SendInterrupted);
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
        telegram::TelegramPollBatch,
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
                start_node: NodeId(2),
                input_nodes: vec![NodeId(1)],
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
    fn compose_final_reply_formats_image_and_size_rejections_from_live_diagnostic_and_recovered_notice()
     {
        let token = ImToken::parse("secret-tok-415").unwrap();
        let redactor = ImRedactor::new([token]);
        let empty_tok = TokenSummary {
            known: Some(0),
            complete: true,
            overflowed: false,
        };
        let make_outcome =
            |pub_outcome: PublicTurnOutcome,
             assistant_text: &str,
             diag_msg: Option<&str>,
             notice_msg: Option<&str>| InProcessTurnOutcome {
                detail: TurnDetail {
                    turn_id: new_turn_id(),
                    session_id: new_session_id(),
                    start_node: NodeId(2),
                    input_nodes: vec![NodeId(1)],
                    agent_name: AgentName::parse("test").unwrap(),
                    accepted_at: Utc::now(),
                    ended_at: Utc::now(),
                    summary: TurnSummary {
                        outcome: pub_outcome,
                        notice: notice_msg.map(|text| Notice {
                            level: NoticeLevel::Error,
                            text: text.to_owned(),
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
                assistant_text: assistant_text.to_owned(),
                diagnostic: diag_msg.map(|msg| LocalError::new(LocalErrorKind::BadRequest, msg)),
            };

        let cases = [
            (
                "不支持图片输入",
                "模型 local/m-secret-tok-415 不支持图片输入；请改用支持图片的模型，或执行 lattice session rewind 退回不含图片的节点",
                "不支持图片输入",
            ),
            (
                "32 MiB 本地请求体预算",
                "编码后的模型请求超过 LATTICE 本地请求体预算（32 MiB），请减少图片、执行 lattice session rewind 退回较早节点或新建会话",
                "本地请求体预算（32 MiB）",
            ),
            (
                "远端 HTTP 413",
                "HTTP 413 request_too_large: secret-tok-415 exceeds limit；目标服务拒绝了本次请求大小（HTTP 413），请减少图片、执行 lattice session rewind 退回较早节点或新建会话",
                "目标服务拒绝了本次请求大小（HTTP 413）",
            ),
        ];

        for (label, raw_msg, expected_reason) in cases {
            // 1. 实时路径：outcome.diagnostic 存在
            let live = compose_final_reply(
                &make_outcome(PublicTurnOutcome::Failed, "", Some(raw_msg), None),
                &redactor,
            );
            // 2. 重挂/重启恢复路径：outcome.diagnostic == None，从持久 summary.notice.text 恢复
            let recovered = compose_final_reply(
                &make_outcome(PublicTurnOutcome::Failed, "", None, Some(raw_msg)),
                &redactor,
            );
            assert_eq!(
                live, recovered,
                "{label} 在实时 diagnostic 与持久 summary.notice 恢复路径下的输出必须一致"
            );
            for rendered in [&live, &recovered] {
                assert!(
                    !rendered.contains("secret-tok-415"),
                    "{label} 不得泄露机器人凭据：{rendered}"
                );
                assert!(
                    rendered.starts_with("［本轮执行失败：") && rendered.ends_with('］'),
                    "{label} 应保持失败结局说明格式：{rendered}"
                );
                assert!(
                    rendered.contains(expected_reason)
                        && rendered.contains("减少图片")
                        && rendered.contains("/new")
                        && rendered.contains("lattice session rewind"),
                    "{label} 应包含脱敏中文原因与完整 IM 引导：{rendered}"
                );
                assert_eq!(
                    rendered.matches("减少图片").count(),
                    1,
                    "{label} 不得重复拼接引导文案：{rendered}"
                );
            }
        }

        // 反例与超长模型名核验：HTTP 404 含 "model HTTP 413 not found" 绝不附加大小拒绝引导，600 字符长模型名在截断前识别
        let not_found_413 = compose_final_reply(
            &make_outcome(
                PublicTurnOutcome::Failed,
                "",
                Some("HTTP 404 not_found_error: model HTTP 413 not found"),
                None,
            ),
            &redactor,
        );
        assert_eq!(
            not_found_413, "［本轮执行失败：HTTP 404 not_found_error: model HTTP 413 not found］",
            "HTTP 404 正文含 HTTP 413 字样时不得误触发请求大小拒绝引导"
        );
        assert!(!not_found_413.contains("目标服务拒绝了本次请求大小"));
        assert!(!not_found_413.contains("/new"));

        let long_model_raw = format!(
            "模型 local/{} 不支持图片输入；请改用支持图片的模型，或执行 lattice session rewind 退回不含图片的节点",
            "a".repeat(600)
        );
        let long_model_reply = compose_final_reply(
            &make_outcome(PublicTurnOutcome::Failed, "", None, Some(&long_model_raw)),
            &redactor,
        );
        assert!(
            long_model_reply.contains("不支持图片输入")
                && long_model_reply.contains("减少图片")
                && long_model_reply.contains("/new")
                && long_model_reply.contains("lattice session rewind"),
            "Failed 结局下超长模型名必须在截断前识别并保留完整引导：{long_model_reply}"
        );

        // 普通 Failed、Cancelled 与 Completed 保持不变
        assert_eq!(
            compose_final_reply(
                &make_outcome(
                    PublicTurnOutcome::Failed,
                    "",
                    None,
                    Some("HTTP 502 Bad Gateway"),
                ),
                &redactor,
            ),
            "［本轮执行失败：HTTP 502 Bad Gateway］"
        );
        assert_eq!(
            compose_final_reply(
                &make_outcome(
                    PublicTurnOutcome::Cancelled,
                    "半截输出",
                    None,
                    Some("用户取消"),
                ),
                &redactor,
            ),
            "半截输出\n\n［本轮已取消：用户取消］"
        );
        assert_eq!(
            compose_final_reply(
                &make_outcome(PublicTurnOutcome::Completed, "正常回复", None, None),
                &redactor,
            ),
            "正常回复"
        );
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
        ) -> Result<TelegramPollBatch, TelegramApiError> {
            Ok(TelegramPollBatch::default())
        }

        fn acknowledge(&self, _inbound_ids: &[crate::identity::InboundId]) {}

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

        async fn download_file(
            &self,
            _file_id: &str,
            _max_bytes: usize,
        ) -> Result<Vec<u8>, crate::telegram::TelegramDownloadError> {
            Err(crate::telegram::TelegramDownloadError::Api(
                TelegramApiError::Retryable {
                    diagnostic: "草稿节流单测替身未配置文件下载".into(),
                },
            ))
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
            let mut pacer = DraftPacer::new(&chat_clone, 42, &throttles_clone);
            loop {
                loop {
                    match rx.try_recv() {
                        Ok(text) => pacer.record_snapshot(text),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => return,
                    }
                }
                let Some(deadline) = pacer.next_deadline().await else {
                    tokio::select! {
                        _ = wait_stop_signal(&mut stop_rx) => return,
                        next = rx.recv() => match next {
                            Some(text) => pacer.record_snapshot(text),
                            None => return,
                        },
                    }
                    continue;
                };
                if tokio::time::Instant::now() < deadline {
                    tokio::select! {
                        _ = wait_stop_signal(&mut stop_rx) => return,
                        next = rx.recv() => match next {
                            Some(text) => pacer.record_snapshot(text),
                            None => return,
                        },
                        _ = tokio::time::sleep_until(deadline) => {}
                    }
                    continue;
                }
                let Some((preview, started_at)) = pacer.prepare_send().await else {
                    continue;
                };
                let mut send_fut =
                    std::pin::pin!(io_clone.send_message_draft(&chat_clone, 42, &preview, true));
                let send_res = loop {
                    tokio::select! {
                        biased;
                        _ = wait_stop_signal(&mut stop_rx) => return,
                        next = rx.recv() => match next {
                            Some(text) => pacer.record_snapshot(text),
                            None => return,
                        },
                        res = &mut send_fut => break res,
                    }
                };
                pacer.finish_send(started_at, send_res).await;
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

        // 模型重试或步骤切换发出空快照 "" 时，必须发送空草稿清除先前残留正文，随后新尝试正文正常发出
        tokio::time::advance(Duration::from_secs(1)).await;
        tx.send(String::new()).unwrap();
        tokio::task::yield_now().await;
        {
            let recorded = io.drafts.lock().unwrap();
            assert_eq!(
                recorded.len(),
                4,
                "重试产生空快照时必须向 Telegram 发送空草稿"
            );
            assert_eq!(recorded[3].3, "");
            assert!(recorded[3].4);
        }

        tokio::time::advance(Duration::from_secs(1)).await;
        tx.send("重试后新正文".into()).unwrap();
        tokio::task::yield_now().await;
        {
            let recorded = io.drafts.lock().unwrap();
            assert_eq!(recorded.len(), 5);
            assert_eq!(recorded[4].3, "重试后新正文");
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
        let rejected = send_text_reliably(&io, &chat, "被拒消息", stop_rx.clone())
            .await
            .expect("不应被停止打断");
        assert_eq!(rejected, SendDisposition::PlatformRejected);
        assert_eq!(
            io.sent_messages.lock().unwrap().len(),
            1,
            "403 平台拒收后不得继续重试发送"
        );

        // 超大 retry_after（u64::MAX 秒，溢出 Instant 表示范围）：send_text_reliably 与 DraftPacer 均不 panic 且正常响应停机
        let (overflow_stop_tx, overflow_stop_rx) = watch::channel(false);
        {
            let mut outcomes = io.send_outcomes.lock().unwrap();
            outcomes.clear();
            outcomes.push_back(Err(TelegramApiError::RateLimited {
                retry_after: Some(Duration::from_secs(u64::MAX)),
                diagnostic: "429 overflow".into(),
            }));
        }
        let throttles = ChatDraftThrottleMap::default();
        let mut pacer = DraftPacer::new(&chat, 7, &throttles);
        pacer.record_snapshot("首段草稿".into());
        let (_, started_at) = pacer.prepare_send().await.expect("首次草稿应允许准备发送");
        pacer
            .finish_send(
                started_at,
                Err(TelegramApiError::RateLimited {
                    retry_after: Some(Duration::from_secs(u64::MAX)),
                    diagnostic: "429 draft overflow".into(),
                }),
            )
            .await;
        pacer.record_snapshot("溢出退避后的草稿更新".into());
        assert_eq!(
            pacer.next_deadline().await,
            None,
            "草稿 retry_after 溢出 Instant 时应进入无限期退避，不得退化为立即重发"
        );
        assert!(pacer.prepare_send().await.is_none());

        let send_task = tokio::spawn(async move {
            send_text_reliably(&io, &chat, "等待停机", overflow_stop_rx).await
        });
        tokio::task::yield_now().await;
        assert!(
            !send_task.is_finished(),
            "溢出 Instant 的 retry_after 在停机前应持续挂起等待停机信号"
        );
        overflow_stop_tx.send(true).unwrap();
        assert_eq!(
            send_task.await.expect("send_text_reliably 不得 panic"),
            Err(SendInterrupted)
        );
    }
}
