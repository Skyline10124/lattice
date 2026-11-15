// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Telegram 更新解析、`tg:` 身份键边界转换、连续前缀 `offset` 推进与唯一测试替身 `TelegramIo`。

use std::{collections::BTreeMap, time::Duration};

use serde::Deserialize;

use crate::identity::{ChatKey, InboundId, SenderKey};

/// Telegram 平台身份键前缀。
pub(crate) const TELEGRAM_PLATFORM_PREFIX: &str = "tg";

/// `getUpdates` 订阅的固定更新类型集合。
pub(crate) const ALLOWED_UPDATES: [&str; 2] = ["message", "stopped_message_generation"];

/// Telegram 最长重投窗口（24 小时未取走保留上限 + 5 分钟时钟与传输裕量）。
pub(crate) const TELEGRAM_REDELIVERY_WINDOW: chrono::Duration =
    chrono::Duration::seconds(24 * 3600 + 5 * 60);

/// `getUpdates` 长轮询超时秒数。
pub(crate) const LONG_POLL_TIMEOUT_SECS: u32 = 30;

/// 将 Telegram 整数 `from.id` 转换为平台身份键。
pub(crate) fn sender_key_from_id(id: i64) -> SenderKey {
    SenderKey::from_platform_id(TELEGRAM_PLATFORM_PREFIX, &id.to_string())
}

/// 将 Telegram 整数 `chat.id` 转换为平台身份键。
pub(crate) fn chat_key_from_id(id: i64) -> ChatKey {
    ChatKey::from_platform_id(TELEGRAM_PLATFORM_PREFIX, &id.to_string())
}

/// 将 Telegram 整数 `update_id` 转换为平台入站去重键。
pub(crate) fn inbound_id_from_update_id(update_id: i64) -> InboundId {
    InboundId::from_platform_id(TELEGRAM_PLATFORM_PREFIX, &update_id.to_string())
}

/// 从 `ChatKey` 提取 Telegram `chat_id` 整数。
pub(crate) fn chat_id_from_key(chat: &ChatKey) -> Result<i64, &'static str> {
    let raw = chat
        .strip_platform(TELEGRAM_PLATFORM_PREFIX)
        .ok_or("对话身份键不属于 Telegram 平台")?;
    raw.parse::<i64>()
        .map_err(|_| "Telegram 对话身份键主体不是有效的 64 位整数")
}

fn update_id_from_inbound_id(inbound_id: &InboundId) -> Option<i64> {
    inbound_id
        .strip_platform(TELEGRAM_PLATFORM_PREFIX)?
        .parse::<i64>()
        .ok()
}

/// 解析后的单条 Telegram 入站更新（原始整数标识已在平台模块边界全部转换为身份键）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TelegramUpdate {
    pub(crate) inbound_id: InboundId,
    pub(crate) payload: TelegramUpdatePayload,
}

/// 转换为平台无关身份键后的更新载荷。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TelegramUpdatePayload {
    /// `message` 更新；仅提取 `message.text`（媒体消息附带的 `caption` 视为无文本）。
    Message {
        chat: ChatKey,
        is_private: bool,
        sender: Option<SenderKey>,
        text: Option<String>,
    },
    /// 用户点击流式草稿上的停止按钮后到达的 `stopped_message_generation` 更新。
    StoppedMessageGeneration {
        chat: ChatKey,
        is_private: bool,
        draft_id: i64,
    },
    /// 其他无需处理的更新变体（收到即确认，以免阻塞连续前缀）。
    Ignored,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawUpdateEnvelope {
    pub(crate) ok: bool,
    #[serde(default)]
    pub(crate) result: Vec<RawTelegramUpdate>,
    #[serde(default)]
    pub(crate) description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTelegramUpdate {
    pub(crate) update_id: i64,
    #[serde(default)]
    pub(crate) message: Option<RawMessage>,
    #[serde(default)]
    pub(crate) stopped_message_generation: Option<RawStoppedMessageGeneration>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawMessage {
    pub(crate) chat: RawChat,
    #[serde(default)]
    pub(crate) from: Option<RawUser>,
    #[serde(default)]
    pub(crate) text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawChat {
    pub(crate) id: i64,
    #[serde(rename = "type")]
    pub(crate) kind: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawUser {
    pub(crate) id: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawStoppedMessageGeneration {
    pub(crate) chat: RawChat,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) message_thread_id: Option<i64>,
    pub(crate) draft_id: i64,
}

impl RawTelegramUpdate {
    /// 将 Telegram 原始更新结构转换为使用 `tg:` 身份键的网关更新。
    pub(crate) fn into_domain(self) -> TelegramUpdate {
        let inbound_id = inbound_id_from_update_id(self.update_id);
        let payload = if let Some(msg) = self.message {
            TelegramUpdatePayload::Message {
                chat: chat_key_from_id(msg.chat.id),
                is_private: msg.chat.kind == "private",
                sender: msg.from.map(|user| sender_key_from_id(user.id)),
                text: msg.text,
            }
        } else if let Some(stopped) = self.stopped_message_generation {
            TelegramUpdatePayload::StoppedMessageGeneration {
                chat: chat_key_from_id(stopped.chat.id),
                is_private: stopped.chat.kind == "private",
                draft_id: stopped.draft_id,
            }
        } else {
            TelegramUpdatePayload::Ignored
        };

        TelegramUpdate {
            inbound_id,
            payload,
        }
    }
}

/// 内存中维护的 `getUpdates` 连续已确认前缀跟踪器。
///
/// 新实例首次轮询返回 `None`（不传 `offset`）；仅当队首起的更新连续确认后才弹出并推进 `offset`。
#[derive(Debug, Default)]
pub(crate) struct OffsetTracker {
    pending: BTreeMap<i64, bool>,
    next_offset: Option<i64>,
}

impl OffsetTracker {
    /// 返回下一轮 `getUpdates` 应携带的 `offset`。
    pub(crate) fn next_offset(&self) -> Option<i64> {
        self.next_offset
    }

    /// 登记一批新收到的 `update_id`；重投的未弹出条目保留已有确认标记，已弹出条目直接忽略。
    pub(crate) fn record_received(&mut self, update_id: i64) {
        if self.next_offset.is_some_and(|offset| update_id < offset) {
            return;
        }
        self.pending.entry(update_id).or_insert(false);
    }

    /// 标记指定 `update_id` 已完成本地确认，并从队首起尽可能推进连续已确认前缀。
    pub(crate) fn ack(&mut self, update_id: i64) {
        if self.next_offset.is_some_and(|offset| update_id < offset) {
            return;
        }
        self.pending.insert(update_id, true);
        while let Some((&front_id, &acked)) = self.pending.first_key_value() {
            if !acked {
                break;
            }
            self.pending.pop_first();
            if let Some(advanced) = front_id.checked_add(1) {
                self.next_offset = Some(advanced);
            }
        }
    }

    /// 按平台入站身份键登记已接收事件。
    #[cfg(test)]
    pub(crate) fn record_received_inbound(&mut self, inbound_id: &InboundId) {
        if let Some(update_id) = update_id_from_inbound_id(inbound_id) {
            self.record_received(update_id);
        }
    }

    /// 按平台入站身份键标记本地确认，并推进连续前缀。
    pub(crate) fn ack_inbound(&mut self, inbound_id: &InboundId) {
        if let Some(update_id) = update_id_from_inbound_id(inbound_id) {
            self.ack(update_id);
        }
    }
}

/// Telegram 收发接口的分类错误（所有诊断字段均已完成 URL 剥离与凭据脱敏）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TelegramApiError {
    /// HTTP 429 速率限制，携带可选的服务端建议重试等待时长。
    RateLimited {
        retry_after: Option<Duration>,
        diagnostic: String,
    },
    /// HTTP 5xx、网络传输中断、超时或运行期地址解析失败等可重试错误。
    Retryable { diagnostic: String },
    /// HTTP 400 或 403 平台拒收终态（如用户拉黑机器人，`sendMessage` 不再重试）。
    Rejected { status: u16, diagnostic: String },
    /// HTTP 401/404 等认证或请求配置错误。
    UnauthorizedOrInvalid { status: u16, diagnostic: String },
}

impl TelegramApiError {
    pub(crate) fn diagnostic(&self) -> &str {
        match self {
            Self::RateLimited { diagnostic, .. }
            | Self::Retryable { diagnostic }
            | Self::Rejected { diagnostic, .. }
            | Self::UnauthorizedOrInvalid { diagnostic, .. } => diagnostic,
        }
    }
}

/// `lattice-im` 中唯一的抽象接口：隔离 Telegram HTTPS 收发以便用真实 `Service` 做确定性集成测试。
#[async_trait::async_trait]
pub(crate) trait TelegramIo: Send + Sync {
    async fn get_updates(&self, timeout_secs: u32)
    -> Result<Vec<TelegramUpdate>, TelegramApiError>;

    /// 通知平台模块该入站身份键已完成本地确认，推进平台内部连续前缀 `offset`。
    fn acknowledge(&self, inbound_id: &InboundId);

    async fn send_message(&self, chat: &ChatKey, text: &str) -> Result<(), TelegramApiError>;

    async fn send_message_draft(
        &self,
        chat: &ChatKey,
        draft_id: i64,
        text: &str,
        can_stop: bool,
    ) -> Result<(), TelegramApiError>;

    /// 等待本适配器派生的后台阻塞任务（如公网地址解析）全部退出。
    async fn drain(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_raw_telegram_updates_to_prefixed_keys_and_ignores_caption() {
        let json_text = r#"{
            "update_id": 501,
            "message": {
                "message_id": 10,
                "from": {"id": 42, "is_bot": false, "first_name": "Alice"},
                "chat": {"id": 42, "type": "private"},
                "date": 1700000000,
                "text": "你好"
            }
        }"#;
        let raw: RawTelegramUpdate = serde_json::from_str(json_text).expect("解析文本消息应成功");
        let parsed = raw.into_domain();
        assert_eq!(parsed.inbound_id.to_string(), "tg:501");
        assert_eq!(
            parsed.payload,
            TelegramUpdatePayload::Message {
                chat: ChatKey::from_platform_id("tg", "42"),
                is_private: true,
                sender: Some(SenderKey::from_platform_id("tg", "42")),
                text: Some("你好".into()),
            }
        );

        // 带 caption 的图片消息：没有 text 字段，必须解析为 text: None
        let json_photo_caption = r#"{
            "update_id": 502,
            "message": {
                "message_id": 11,
                "from": {"id": 42, "is_bot": false, "first_name": "Alice"},
                "chat": {"id": 42, "type": "private"},
                "date": 1700000001,
                "photo": [{"file_id": "abc", "file_unique_id": "u", "width": 10, "height": 10}],
                "caption": "这张图片带说明文字"
            }
        }"#;
        let raw_photo: RawTelegramUpdate =
            serde_json::from_str(json_photo_caption).expect("解析带说明图片消息应成功");
        match raw_photo.into_domain().payload {
            TelegramUpdatePayload::Message { text, .. } => {
                assert_eq!(text, None, "带 caption 的图片消息不得当作文本消息");
            }
            other => panic!("应解析为 Message 变体，实际: {other:?}"),
        }

        // 停止按钮事件
        let json_stop = r#"{
            "update_id": 503,
            "stopped_message_generation": {
                "chat": {"id": 42, "type": "private"},
                "draft_id": 9001
            }
        }"#;
        let raw_stop: RawTelegramUpdate =
            serde_json::from_str(json_stop).expect("解析停止生成事件应成功");
        assert_eq!(
            raw_stop.into_domain().payload,
            TelegramUpdatePayload::StoppedMessageGeneration {
                chat: ChatKey::from_platform_id("tg", "42"),
                is_private: true,
                draft_id: 9001,
            }
        );
    }

    #[test]
    fn offset_tracker_advances_only_contiguous_acknowledged_prefix() {
        let mut tracker = OffsetTracker::default();
        assert_eq!(tracker.next_offset(), None, "首次轮询不得附带 offset");

        tracker.record_received(10);
        tracker.record_received(11);
        tracker.record_received(12);

        // 11 与 12 先乱序完成确认（如不落账命令或不同 chat），10 尚未写台账确认
        tracker.ack(12);
        assert_eq!(
            tracker.next_offset(),
            None,
            "队首 10 未确认时不得推进 offset"
        );
        tracker.ack(11);
        assert_eq!(
            tracker.next_offset(),
            None,
            "队首 10 未确认时仍不得越过它推进 offset"
        );

        // 下一次重投再次收到 10、11、12，不应冲掉 11 和 12 的已确认状态
        tracker.record_received(10);
        tracker.record_received(11);
        tracker.record_received(12);

        // 一旦 10 确认，10..=12 形成连续前缀，直接推进到 13
        tracker.ack(10);
        assert_eq!(tracker.next_offset(), Some(13));

        // 已弹出范围之前的旧重投不会倒退 offset
        tracker.record_received(11);
        tracker.ack(11);
        assert_eq!(tracker.next_offset(), Some(13));
    }
}
