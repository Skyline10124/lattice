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

/// 解析后的单条 Telegram 入站更新（原始整数标识已在平台模块边界转换为身份键，同时保留 `update_id` 供有序处理）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TelegramUpdate {
    pub(crate) update_id: i64,
    pub(crate) inbound_id: InboundId,
    pub(crate) payload: TelegramUpdatePayload,
}

/// 转换为平台无关身份键后的更新载荷。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TelegramUpdatePayload {
    /// `message` 更新：识别纯文本、受支持图片（压缩照片或未压缩图片文档）与其他不支持媒体。
    Message {
        chat: ChatKey,
        is_private: bool,
        sender: Option<SenderKey>,
        media_group_id: Option<String>,
        content: TelegramMessageContent,
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

/// 解析后的 Telegram `message` 正文分类。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TelegramMessageContent {
    /// 纯文本消息。
    Text(String),
    /// 压缩图片（`photo` 最高分辨率项）或声明 MIME 属于四种受支持图片格式的未压缩 `document`。
    Image {
        file_id: String,
        file_size: Option<u64>,
        caption: Option<String>,
    },
    /// 贴纸、转码动图、视频、语音、音频、非图片文档或空消息等不受支持内容。
    Unsupported,
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
    #[serde(default)]
    pub(crate) photo: Option<Vec<RawPhotoSize>>,
    #[serde(default)]
    pub(crate) document: Option<RawDocument>,
    #[serde(default)]
    pub(crate) animation: Option<RawAnimation>,
    #[serde(default)]
    pub(crate) caption: Option<String>,
    #[serde(default)]
    pub(crate) media_group_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPhotoSize {
    pub(crate) file_id: String,
    #[serde(default)]
    pub(crate) file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawDocument {
    pub(crate) file_id: String,
    #[serde(default)]
    pub(crate) file_size: Option<u64>,
    #[serde(default)]
    pub(crate) mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawAnimation {
    #[serde(default)]
    pub(crate) mime_type: Option<String>,
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

fn normalize_mime_essence(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_supported_document_image_mime(mime: &str) -> bool {
    matches!(
        normalize_mime_essence(mime).as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
}

fn is_gif_mime(mime: &str) -> bool {
    normalize_mime_essence(mime) == "image/gif"
}

impl RawTelegramUpdate {
    /// 将 Telegram 原始更新结构转换为使用 `tg:` 身份键的网关更新。
    pub(crate) fn into_domain(self) -> TelegramUpdate {
        let update_id = self.update_id;
        let inbound_id = inbound_id_from_update_id(update_id);
        let payload = if let Some(msg) = self.message {
            let chat = chat_key_from_id(msg.chat.id);
            let is_private = msg.chat.kind == "private";
            let sender = msg.from.map(|user| sender_key_from_id(user.id));
            let media_group_id = msg.media_group_id;
            let content = if let Some(photo) = msg.photo.and_then(|mut photos| photos.pop()) {
                TelegramMessageContent::Image {
                    file_id: photo.file_id,
                    file_size: photo.file_size,
                    caption: msg.caption,
                }
            } else if msg
                .animation
                .as_ref()
                .is_some_and(|anim| !anim.mime_type.as_deref().is_some_and(is_gif_mime))
            {
                // Telegram 对服务端转码为 MP4 的动图会同时填充 animation 与 document，必须先于 document 拦截；
                // 若未压缩 GIF 的 animation.mime_type 归一化后为 image/gif，则放行至后续 document 分支并经魔数核验。
                TelegramMessageContent::Unsupported
            } else if let Some(doc) = msg.document {
                if doc
                    .mime_type
                    .as_deref()
                    .is_some_and(is_supported_document_image_mime)
                {
                    TelegramMessageContent::Image {
                        file_id: doc.file_id,
                        file_size: doc.file_size,
                        caption: msg.caption,
                    }
                } else {
                    TelegramMessageContent::Unsupported
                }
            } else if let Some(text) = msg.text {
                TelegramMessageContent::Text(text)
            } else {
                TelegramMessageContent::Unsupported
            };

            TelegramUpdatePayload::Message {
                chat,
                is_private,
                sender,
                media_group_id,
                content,
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
            update_id,
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

    /// 登记收到的一条 `update_id`，并返回该更新在本实例中是否尚未确认（`true` 表示需分发，`false` 表示本实例已确认过，重投应直接过滤）。
    pub(crate) fn record_received(&mut self, update_id: i64) -> bool {
        if self.next_offset.is_some_and(|offset| update_id < offset) {
            return false;
        }
        match self.pending.entry(update_id) {
            std::collections::btree_map::Entry::Occupied(entry) => !*entry.get(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(false);
                true
            }
        }
    }

    /// 判断指定 `update_id` 在本实例中是否已完成本地确认。
    pub(crate) fn is_acked(&self, update_id: i64) -> bool {
        if self.next_offset.is_some_and(|offset| update_id < offset) {
            return true;
        }
        self.pending.get(&update_id).copied().unwrap_or(false)
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
            let Some(advanced) = front_id.checked_add(1) else {
                break;
            };
            self.pending.pop_first();
            self.next_offset = Some(advanced);
        }
    }

    /// 按平台入站身份键登记已接收事件，返回是否尚未确认。
    #[cfg(test)]
    pub(crate) fn record_received_inbound(&mut self, inbound_id: &InboundId) -> bool {
        match update_id_from_inbound_id(inbound_id) {
            Some(update_id) => self.record_received(update_id),
            None => true,
        }
    }

    /// 判断指定入站身份键在本实例中是否已完成本地确认。
    pub(crate) fn is_acked_inbound(&self, inbound_id: &InboundId) -> bool {
        update_id_from_inbound_id(inbound_id).is_some_and(|id| self.is_acked(id))
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

/// Telegram `getFile` 元数据查询与文件字节流下载的分类错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TelegramDownloadError {
    /// `getFile` 声明的 `file_size`、响应头 `Content-Length` 或实际响应字节流超过调用方上限。
    TooLarge,
    /// `getFile` 缺少 `file_path` 或返回了不合法的相对路径。
    InvalidFilePath,
    /// HTTP 非成功状态码、非成功响应包或传输层错误（已完成 URL 剥离与凭据脱敏）。
    Api(TelegramApiError),
}

/// `lattice-im` 中唯一的抽象接口：隔离 Telegram HTTPS 收发以便用真实 `Service` 做确定性集成测试。
#[async_trait::async_trait]
pub(crate) trait TelegramIo: Send + Sync {
    async fn get_updates(&self, timeout_secs: u32)
    -> Result<Vec<TelegramUpdate>, TelegramApiError>;

    /// 通知平台模块这批入站身份键已完成本地确认，推进平台内部连续前缀 `offset`。
    fn acknowledge(&self, inbound_ids: &[InboundId]);

    /// 查询该入站身份键在本实例中是否已完成本地确认，供分发前兜底过滤重投。
    fn is_acknowledged(&self, inbound_id: &InboundId) -> bool {
        let _ = inbound_id;
        false
    }

    async fn send_message(&self, chat: &ChatKey, text: &str) -> Result<(), TelegramApiError>;

    async fn send_message_draft(
        &self,
        chat: &ChatKey,
        draft_id: i64,
        text: &str,
        can_stop: bool,
    ) -> Result<(), TelegramApiError>;

    /// 执行一次受控 `getFile + GET /file/...` 文件下载尝试（有界重试由上层 `media` 模块负责）。
    async fn download_file(
        &self,
        file_id: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, TelegramDownloadError>;

    /// 等待本适配器派生的后台阻塞任务（如公网地址解析）全部退出。
    async fn drain(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_text_photo_image_document_animation_and_unsupported_updates() {
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
        assert_eq!(parsed.update_id, 501);
        assert_eq!(parsed.inbound_id.to_string(), "tg:501");
        assert_eq!(
            parsed.payload,
            TelegramUpdatePayload::Message {
                chat: ChatKey::from_platform_id("tg", "42"),
                is_private: true,
                sender: Some(SenderKey::from_platform_id("tg", "42")),
                media_group_id: None,
                content: TelegramMessageContent::Text("你好".into()),
            }
        );

        // 压缩图片 photo 数组：取最后一项（最高分辨率），保留 caption 与 media_group_id
        let json_photo = r#"{
            "update_id": 502,
            "message": {
                "message_id": 11,
                "from": {"id": 42, "is_bot": false, "first_name": "Alice"},
                "chat": {"id": 42, "type": "private"},
                "date": 1700000001,
                "media_group_id": "album-77",
                "photo": [
                    {"file_id": "thumb", "file_unique_id": "u1", "width": 90, "height": 90, "file_size": 1200},
                    {"file_id": "mid", "file_unique_id": "u2", "width": 320, "height": 320, "file_size": 8400},
                    {"file_id": "hi_res", "file_unique_id": "u3", "width": 1280, "height": 960, "file_size": 95000}
                ],
                "caption": "这张图片带说明文字"
            }
        }"#;
        let raw_photo: RawTelegramUpdate =
            serde_json::from_str(json_photo).expect("解析带说明压缩图片消息应成功");
        let parsed_photo = raw_photo.into_domain();
        assert_eq!(parsed_photo.update_id, 502);
        assert_eq!(
            parsed_photo.payload,
            TelegramUpdatePayload::Message {
                chat: ChatKey::from_platform_id("tg", "42"),
                is_private: true,
                sender: Some(SenderKey::from_platform_id("tg", "42")),
                media_group_id: Some("album-77".into()),
                content: TelegramMessageContent::Image {
                    file_id: "hi_res".into(),
                    file_size: Some(95000),
                    caption: Some("这张图片带说明文字".into()),
                },
            }
        );

        // 未压缩图片 document：大小写、前后空白与分号参数规范化后识别四种受支持图片 MIME
        for raw_mime in [
            "image/png",
            "IMAGE/JPEG",
            "  Image/WebP ; charset=binary ",
            "image/gif; name=\"demo.gif\"",
        ] {
            let doc_update: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
                "update_id": 503,
                "message": {
                    "message_id": 12,
                    "from": {"id": 42},
                    "chat": {"id": 42, "type": "private"},
                    "media_group_id": "doc-group",
                    "document": {
                        "file_id": "doc_img_1",
                        "file_size": 4096,
                        "mime_type": raw_mime
                    },
                    "caption": "/new"
                }
            }))
            .expect("构造图片文档更新应成功");
            assert_eq!(
                doc_update.into_domain().payload,
                TelegramUpdatePayload::Message {
                    chat: ChatKey::from_platform_id("tg", "42"),
                    is_private: true,
                    sender: Some(SenderKey::from_platform_id("tg", "42")),
                    media_group_id: Some("doc-group".into()),
                    content: TelegramMessageContent::Image {
                        file_id: "doc_img_1".into(),
                        file_size: Some(4096),
                        caption: Some("/new".into()),
                    },
                },
                "文档 MIME {raw_mime:?} 规范化后应识别为受支持图片"
            );
        }

        // 转码动图 animation（video/mp4 或无 mime_type）+ document（即使声明 image/gif）：必须先于 document 拦截为 Unsupported
        for (anim_mime, anim_doc_mime) in [
            (Some("video/mp4"), "video/mp4"),
            (Some("video/mp4"), "image/gif"),
            (None, "image/gif"),
        ] {
            let mut anim_obj = serde_json::json!({
                "file_id": "anim_1",
                "width": 320,
                "height": 240,
                "duration": 3
            });
            if let Some(m) = anim_mime {
                anim_obj["mime_type"] = serde_json::json!(m);
            }
            let anim_update: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
                "update_id": 504,
                "message": {
                    "message_id": 13,
                    "from": {"id": 42},
                    "chat": {"id": 42, "type": "private"},
                    "animation": anim_obj,
                    "document": {
                        "file_id": "anim_1",
                        "file_size": 20480,
                        "mime_type": anim_doc_mime
                    },
                    "caption": "动图说明"
                }
            }))
            .expect("构造转码动图更新应成功");
            assert_eq!(
                anim_update.into_domain().payload,
                TelegramUpdatePayload::Message {
                    chat: ChatKey::from_platform_id("tg", "42"),
                    is_private: true,
                    sender: Some(SenderKey::from_platform_id("tg", "42")),
                    media_group_id: None,
                    content: TelegramMessageContent::Unsupported,
                },
                "非 image/gif 的 animation（{anim_mime:?}）必须先于 document（{anim_doc_mime}）拦截为 Unsupported"
            );
        }

        // 未压缩 GIF（animation 归一化 MIME 为 image/gif + document 声明 image/gif）：放行识别为 Image 并保留元数据
        for anim_gif_mime in ["image/gif", "  IMAGE/GIF; charset=binary "] {
            let gif_anim_update: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
                "update_id": 504,
                "message": {
                    "message_id": 14,
                    "from": {"id": 42},
                    "chat": {"id": 42, "type": "private"},
                    "animation": {
                        "file_id": "raw_gif_1",
                        "width": 320,
                        "height": 240,
                        "duration": 2,
                        "mime_type": anim_gif_mime
                    },
                    "document": {
                        "file_id": "raw_gif_1",
                        "file_size": 20480,
                        "mime_type": "image/gif"
                    },
                    "caption": "未压缩动图说明"
                }
            }))
            .expect("构造未压缩 GIF 动图更新应成功");
            assert_eq!(
                gif_anim_update.into_domain().payload,
                TelegramUpdatePayload::Message {
                    chat: ChatKey::from_platform_id("tg", "42"),
                    is_private: true,
                    sender: Some(SenderKey::from_platform_id("tg", "42")),
                    media_group_id: None,
                    content: TelegramMessageContent::Image {
                        file_id: "raw_gif_1".into(),
                        file_size: Some(20480),
                        caption: Some("未压缩动图说明".into()),
                    },
                },
                "animation({anim_gif_mime:?}) + document(image/gif) 应识别为受支持图片"
            );
        }

        // 非图片文档（PDF、SVG、缺省 MIME）、贴纸与空 photo 数组均归入 Unsupported
        let unsupported_payloads = [
            serde_json::json!({
                "update_id": 505,
                "message": {
                    "chat": {"id": 42, "type": "private"},
                    "from": {"id": 42},
                    "document": {"file_id": "pdf_1", "mime_type": "application/pdf"},
                    "caption": "文档说明"
                }
            }),
            serde_json::json!({
                "update_id": 506,
                "message": {
                    "chat": {"id": 42, "type": "private"},
                    "from": {"id": 42},
                    "document": {"file_id": "svg_1", "mime_type": "image/svg+xml"}
                }
            }),
            serde_json::json!({
                "update_id": 507,
                "message": {
                    "chat": {"id": 42, "type": "private"},
                    "from": {"id": 42},
                    "document": {"file_id": "bin_1"}
                }
            }),
            serde_json::json!({
                "update_id": 508,
                "message": {
                    "chat": {"id": 42, "type": "private"},
                    "from": {"id": 42},
                    "sticker": {"file_id": "stk_1", "width": 512, "height": 512}
                }
            }),
            serde_json::json!({
                "update_id": 509,
                "message": {
                    "chat": {"id": 42, "type": "private"},
                    "from": {"id": 42},
                    "photo": []
                }
            }),
        ];
        for sample in unsupported_payloads {
            let raw_unsup: RawTelegramUpdate =
                serde_json::from_value(sample.clone()).expect("构造不支持媒体更新应成功");
            match raw_unsup.into_domain().payload {
                TelegramUpdatePayload::Message { content, .. } => {
                    assert_eq!(
                        content,
                        TelegramMessageContent::Unsupported,
                        "样本 {sample} 必须归入 Unsupported"
                    );
                }
                other => panic!("应解析为 Message 变体，实际: {other:?}"),
            }
        }

        // 停止按钮事件
        let json_stop = r#"{
            "update_id": 510,
            "stopped_message_generation": {
                "chat": {"id": 42, "type": "private"},
                "draft_id": 9001
            }
        }"#;
        let raw_stop: RawTelegramUpdate =
            serde_json::from_str(json_stop).expect("解析停止生成事件应成功");
        let parsed_stop = raw_stop.into_domain();
        assert_eq!(parsed_stop.update_id, 510);
        assert_eq!(
            parsed_stop.payload,
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

        // 下一次重投再次收到 10、11、12：10 仍未确认应保留分发（返回 true），11 与 12 已确认应过滤（返回 false）且不冲掉确认状态
        assert!(tracker.record_received(10), "未确认的队首 10 仍应返回 true");
        assert!(
            !tracker.record_received(11),
            "本实例已确认的 11 重投时必须返回 false 过滤"
        );
        assert!(
            !tracker.record_received(12),
            "本实例已确认的 12 重投时必须返回 false 过滤"
        );
        assert!(!tracker.is_acked(10));
        assert!(tracker.is_acked(11));
        assert!(tracker.is_acked(12));

        // 一旦 10 确认，10..=12 形成连续前缀，直接推进到 13
        tracker.ack(10);
        assert_eq!(tracker.next_offset(), Some(13));
        assert!(tracker.is_acked(10));

        // 已弹出范围之前的旧重投直接返回 false，不会倒退 offset
        assert!(!tracker.record_received(11));
        tracker.ack(11);
        assert_eq!(tracker.next_offset(), Some(13));
    }
}
