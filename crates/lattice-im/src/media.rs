// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 单图与相册共用的入站媒体静态快筛、受控有界重试下载与文件头魔数核验。

use std::time::Duration;

use lattice_ipc::{
    ImageMediaType, MAX_IMAGE_BYTES, MAX_USER_IMAGE_TOTAL_BYTES, MAX_USER_IMAGES_PER_TURN,
    MAX_USER_TEXT_BYTES, UserInputPart,
};
use tokio::sync::watch;

use crate::{
    command::UNSUPPORTED_MEDIA_REPLY,
    reply::wait_stop_signal,
    telegram::{TelegramApiError, TelegramDownloadError, TelegramIo},
};

/// 单张图片下载失败后的最大重试次数（首次尝试 + 最多重试 3 次，共至多 4 次请求）。
pub(crate) const MAX_DOWNLOAD_RETRIES: u32 = 3;

const INITIAL_DOWNLOAD_BACKOFF: Duration = Duration::from_millis(200);
const MAX_DOWNLOAD_BACKOFF: Duration = Duration::from_secs(30);

/// 待进入静态快筛与受控下载的单张入站图片元数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IncomingImageItem {
    pub(crate) file_id: String,
    pub(crate) file_size: Option<u64>,
    pub(crate) caption: Option<String>,
}

/// 单条图片或已闭合相册中的一项入站媒体元素：受支持候选图片，或相册中混入的不受支持媒体。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IncomingMediaItem {
    Image(IncomingImageItem),
    Unsupported,
}

impl IncomingMediaItem {
    pub(crate) fn image(file_id: String, file_size: Option<u64>, caption: Option<String>) -> Self {
        Self::Image(IncomingImageItem {
            file_id,
            file_size,
            caption,
        })
    }

    pub(crate) fn unsupported() -> Self {
        Self::Unsupported
    }
}

/// 单图或整组相册经快筛、受控下载与魔数核验后的处理结果。
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum MediaBatchOutcome {
    /// 全部图片核验通过，已组装为 `[Image_1, ..., Image_k]` 及可选末尾 `Text`。
    Ready(Vec<UserInputPart>),
    /// 静态快筛未通过、下载不可恢复失败、重试耗尽或字节/魔数不合规（携带脱敏中文原因）。
    Rejected(String),
    /// 下载或退避等待期间收到停机信号，调用方须立即退出且不得确认平台更新。
    Interrupted,
}

impl std::fmt::Debug for MediaBatchOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(parts) => f
                .debug_struct("Ready")
                .field("part_count", &parts.len())
                .finish(),
            Self::Rejected(reason) => f.debug_tuple("Rejected").field(reason).finish(),
            Self::Interrupted => f.write_str("Interrupted"),
        }
    }
}

struct PreflightedBatch<'a> {
    images: Vec<&'a IncomingImageItem>,
    joined_caption: Option<String>,
}

/// 下载前静态快筛：不发起任何网络请求，预先拦截不支持媒体、张数超限、已知大小超限与说明文字超限。
fn preflight_media_batch(items: &[IncomingMediaItem]) -> Result<PreflightedBatch<'_>, String> {
    let mut images = Vec::with_capacity(items.len());
    for item in items {
        match item {
            IncomingMediaItem::Image(img) => images.push(img),
            IncomingMediaItem::Unsupported => return Err(UNSUPPORTED_MEDIA_REPLY.to_owned()),
        }
    }

    if images.is_empty() {
        return Err("图片消息为空，无法提交".to_owned());
    }
    if images.len() > MAX_USER_IMAGES_PER_TURN {
        return Err(format!(
            "单轮最多支持 {MAX_USER_IMAGES_PER_TURN} 张图片（当前为 {} 张）",
            images.len()
        ));
    }

    let mut known_total_bytes: u64 = 0;
    for img in &images {
        if let Some(size) = img.file_size {
            if size == 0 {
                return Err("图片文件为空（0 字节），无法提交".to_owned());
            }
            if size > MAX_IMAGE_BYTES as u64 {
                return Err("单张图片不能超过 3.75 MiB（3,932,160 字节）".to_owned());
            }
            known_total_bytes = known_total_bytes.saturating_add(size);
        }
    }
    if known_total_bytes > MAX_USER_IMAGE_TOTAL_BYTES as u64 {
        return Err("单轮图片总大小不能超过 10 MiB（10,485,760 字节）".to_owned());
    }

    let non_empty_captions: Vec<&str> = images
        .iter()
        .filter_map(|img| img.caption.as_deref().filter(|c| !c.is_empty()))
        .collect();
    let caption_bytes = non_empty_captions
        .iter()
        .enumerate()
        .fold(0usize, |acc, (idx, cap)| {
            let sep = usize::from(idx > 0);
            acc.saturating_add(sep).saturating_add(cap.len())
        });
    if caption_bytes > MAX_USER_TEXT_BYTES {
        return Err("图片说明文字不能超过 2 MiB（2,097,152 字节）".to_owned());
    }

    let joined_caption = if non_empty_captions.is_empty() {
        None
    } else {
        Some(non_empty_captions.join("\n"))
    };

    Ok(PreflightedBatch {
        images,
        joined_caption,
    })
}

enum SingleDownloadOutcome {
    Bytes(Vec<u8>),
    Rejected(String),
    Interrupted,
}

async fn download_single_file_bounded(
    io: &dyn TelegramIo,
    file_id: &str,
    max_bytes: usize,
    stop: &mut watch::Receiver<bool>,
) -> SingleDownloadOutcome {
    let mut retries_used: u32 = 0;
    let mut backoff = INITIAL_DOWNLOAD_BACKOFF;

    loop {
        if *stop.borrow() {
            return SingleDownloadOutcome::Interrupted;
        }

        let res = tokio::select! {
            biased;
            _ = wait_stop_signal(stop) => return SingleDownloadOutcome::Interrupted,
            res = io.download_file(file_id, max_bytes) => res,
        };

        match res {
            Ok(bytes) => return SingleDownloadOutcome::Bytes(bytes),
            Err(TelegramDownloadError::TooLarge) => {
                let reason = if max_bytes < MAX_IMAGE_BYTES {
                    "单轮图片总大小不能超过 10 MiB（10,485,760 字节）"
                } else {
                    "单张图片不能超过 3.75 MiB（3,932,160 字节）"
                };
                return SingleDownloadOutcome::Rejected(reason.to_owned());
            }
            Err(TelegramDownloadError::InvalidFilePath) => {
                tracing::warn!("Telegram getFile 返回非法或缺失的相对文件路径，拒绝下载");
                return SingleDownloadOutcome::Rejected(
                    "下载图片失败：平台返回的文件路径无效".to_owned(),
                );
            }
            Err(TelegramDownloadError::Api(
                TelegramApiError::Rejected { status, diagnostic }
                | TelegramApiError::UnauthorizedOrInvalid { status, diagnostic },
            )) => {
                tracing::warn!(
                    status,
                    diagnostic = %diagnostic,
                    "下载 Telegram 图片遭到不可重试拒绝"
                );
                return SingleDownloadOutcome::Rejected(format!(
                    "下载图片失败（HTTP {status}），请重新发送图片后再试"
                ));
            }
            Err(TelegramDownloadError::Api(TelegramApiError::RateLimited {
                retry_after,
                diagnostic,
            })) => {
                if retries_used >= MAX_DOWNLOAD_RETRIES {
                    tracing::warn!(
                        retries = retries_used,
                        diagnostic = %diagnostic,
                        "下载 Telegram 图片限流重试次数已耗尽"
                    );
                    return SingleDownloadOutcome::Rejected(
                        "下载图片失败：平台请求过于频繁，请稍后重试".to_owned(),
                    );
                }
                retries_used += 1;
                let wait = retry_after.unwrap_or(backoff);
                tracing::warn!(
                    retry = retries_used,
                    wait_ms = wait.as_millis() as u64,
                    diagnostic = %diagnostic,
                    "下载 Telegram 图片触发限流，退避后重试"
                );
                tokio::select! {
                    biased;
                    _ = wait_stop_signal(stop) => return SingleDownloadOutcome::Interrupted,
                    _ = tokio::time::sleep(wait) => {}
                }
                backoff = backoff.saturating_mul(2).min(MAX_DOWNLOAD_BACKOFF);
            }
            Err(TelegramDownloadError::Api(TelegramApiError::Retryable { diagnostic })) => {
                if retries_used >= MAX_DOWNLOAD_RETRIES {
                    tracing::warn!(
                        retries = retries_used,
                        diagnostic = %diagnostic,
                        "下载 Telegram 图片可重试错误次数已耗尽"
                    );
                    return SingleDownloadOutcome::Rejected(
                        "下载图片失败：网络连接异常或平台暂时不可用，请稍后重试".to_owned(),
                    );
                }
                retries_used += 1;
                let wait = backoff;
                tracing::warn!(
                    retry = retries_used,
                    wait_ms = wait.as_millis() as u64,
                    diagnostic = %diagnostic,
                    "下载 Telegram 图片暂时失败，退避后重试"
                );
                tokio::select! {
                    biased;
                    _ = wait_stop_signal(stop) => return SingleDownloadOutcome::Interrupted,
                    _ = tokio::time::sleep(wait) => {}
                }
                backoff = backoff.saturating_mul(2).min(MAX_DOWNLOAD_BACKOFF);
            }
        }
    }
}

/// 对单图或已闭合相册执行静态快筛、按序受控下载、累计体积与文件头魔数核验，产出待提交的 `UserInputPart` 列表。
pub(crate) async fn prepare_media_turn(
    io: &dyn TelegramIo,
    items: &[IncomingMediaItem],
    mut stop: watch::Receiver<bool>,
) -> MediaBatchOutcome {
    let preflight = match preflight_media_batch(items) {
        Ok(batch) => batch,
        Err(reason) => return MediaBatchOutcome::Rejected(reason),
    };

    let mut total_bytes: usize = 0;
    let mut parts = Vec::with_capacity(
        preflight.images.len() + usize::from(preflight.joined_caption.is_some()),
    );

    for img in preflight.images {
        let remaining_total = MAX_USER_IMAGE_TOTAL_BYTES.saturating_sub(total_bytes);
        if remaining_total == 0 {
            return MediaBatchOutcome::Rejected(
                "单轮图片总大小不能超过 10 MiB（10,485,760 字节）".to_owned(),
            );
        }
        let max_bytes = MAX_IMAGE_BYTES.min(remaining_total);

        let bytes = match download_single_file_bounded(io, &img.file_id, max_bytes, &mut stop).await
        {
            SingleDownloadOutcome::Bytes(bytes) => bytes,
            SingleDownloadOutcome::Rejected(reason) => return MediaBatchOutcome::Rejected(reason),
            SingleDownloadOutcome::Interrupted => return MediaBatchOutcome::Interrupted,
        };

        if bytes.is_empty() {
            return MediaBatchOutcome::Rejected("图片文件为空（0 字节），无法提交".to_owned());
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return MediaBatchOutcome::Rejected(
                "单张图片不能超过 3.75 MiB（3,932,160 字节）".to_owned(),
            );
        }
        let Some(next_total) = total_bytes
            .checked_add(bytes.len())
            .filter(|&sum| sum <= MAX_USER_IMAGE_TOTAL_BYTES)
        else {
            return MediaBatchOutcome::Rejected(
                "单轮图片总大小不能超过 10 MiB（10,485,760 字节）".to_owned(),
            );
        };
        if ImageMediaType::sniff(&bytes).is_none() {
            return MediaBatchOutcome::Rejected(
                "不支持的图片格式，仅支持 PNG、JPEG、WebP、GIF".to_owned(),
            );
        }

        total_bytes = next_total;
        parts.push(UserInputPart::Image { image: bytes });
    }

    if let Some(text) = preflight.joined_caption {
        parts.push(UserInputPart::Text { text });
    }

    MediaBatchOutcome::Ready(parts)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex as StdMutex};

    use super::*;
    use crate::{
        identity::{ChatKey, InboundId},
        telegram::TelegramUpdate,
    };

    #[derive(Default)]
    struct StubMediaIo {
        files: StdMutex<BTreeMap<String, Vec<u8>>>,
        calls: StdMutex<Vec<(String, usize)>>,
    }

    #[async_trait::async_trait]
    impl TelegramIo for StubMediaIo {
        async fn get_updates(
            &self,
            _timeout_secs: u32,
        ) -> Result<Vec<TelegramUpdate>, TelegramApiError> {
            Ok(Vec::new())
        }

        fn acknowledge(&self, _inbound_ids: &[InboundId]) {}

        async fn send_message(&self, _chat: &ChatKey, _text: &str) -> Result<(), TelegramApiError> {
            Ok(())
        }

        async fn send_message_draft(
            &self,
            _chat: &ChatKey,
            _draft_id: i64,
            _text: &str,
            _can_stop: bool,
        ) -> Result<(), TelegramApiError> {
            Ok(())
        }

        async fn download_file(
            &self,
            file_id: &str,
            max_bytes: usize,
        ) -> Result<Vec<u8>, TelegramDownloadError> {
            self.calls
                .lock()
                .unwrap()
                .push((file_id.to_owned(), max_bytes));
            let map = self.files.lock().unwrap();
            let Some(bytes) = map.get(file_id) else {
                return Err(TelegramDownloadError::InvalidFilePath);
            };
            if bytes.len() > max_bytes {
                return Err(TelegramDownloadError::TooLarge);
            }
            Ok(bytes.clone())
        }
    }

    #[tokio::test]
    async fn static_preflight_blocks_unsupported_count_sizes_and_caption_without_downloading() {
        let io = StubMediaIo::default();
        let (_stop_tx, stop_rx) = watch::channel(false);

        // 1. 含 Unsupported 项直接返回固定提示
        assert_eq!(
            prepare_media_turn(
                &io,
                &[
                    IncomingMediaItem::image("f1".into(), Some(100), None),
                    IncomingMediaItem::unsupported(),
                ],
                stop_rx.clone(),
            )
            .await,
            MediaBatchOutcome::Rejected(UNSUPPORTED_MEDIA_REPLY.to_owned())
        );

        // 2. 0 张与超过 8 张图片被静态拦截
        assert!(matches!(
            prepare_media_turn(&io, &[], stop_rx.clone()).await,
            MediaBatchOutcome::Rejected(msg) if msg.contains("为空")
        ));
        let nine: Vec<_> = (0..9)
            .map(|i| IncomingMediaItem::image(format!("f{i}"), Some(128), None))
            .collect();
        assert!(matches!(
            prepare_media_turn(&io, &nine, stop_rx.clone()).await,
            MediaBatchOutcome::Rejected(msg) if msg.contains("最多支持 8 张")
        ));

        // 3. 已知单图 0 字节、单图超 3.75 MiB、多图总和超 10 MiB 被静态拦截
        assert!(matches!(
            prepare_media_turn(
                &io,
                &[IncomingMediaItem::image("zero".into(), Some(0), None)],
                stop_rx.clone(),
            )
            .await,
            MediaBatchOutcome::Rejected(msg) if msg.contains("0 字节")
        ));
        assert!(matches!(
            prepare_media_turn(
                &io,
                &[IncomingMediaItem::image(
                    "over_single".into(),
                    Some(MAX_IMAGE_BYTES as u64 + 1),
                    None,
                )],
                stop_rx.clone(),
            )
            .await,
            MediaBatchOutcome::Rejected(msg) if msg.contains("3.75 MiB")
        ));
        assert!(matches!(
            prepare_media_turn(
                &io,
                &[
                    IncomingMediaItem::image("a".into(), Some(3_932_160), None),
                    IncomingMediaItem::image("b".into(), Some(3_932_160), None),
                    IncomingMediaItem::image("c".into(), Some(2_621_441), None),
                ],
                stop_rx.clone(),
            )
            .await,
            MediaBatchOutcome::Rejected(msg) if msg.contains("10 MiB")
        ));

        // 4. 非空 caption 拼接后超过 2 MiB 被静态拦截
        let half_plus = "甲".repeat(MAX_USER_TEXT_BYTES / 3);
        assert!(matches!(
            prepare_media_turn(
                &io,
                &[
                    IncomingMediaItem::image("a".into(), Some(64), Some(half_plus.clone())),
                    IncomingMediaItem::image("b".into(), Some(64), Some("乙".into())),
                ],
                stop_rx,
            )
            .await,
            MediaBatchOutcome::Rejected(msg) if msg.contains("2 MiB")
        ));

        assert!(
            io.calls.lock().unwrap().is_empty(),
            "全部静态快筛失败用例均不得触发任何 download_file 调用"
        );
    }

    #[tokio::test]
    async fn joins_non_empty_captions_without_trimming_and_caps_remaining_total_budget() {
        let io = StubMediaIo::default();
        let mut png1 = vec![0x11u8; 3_932_160];
        png1[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut png2 = vec![0x22u8; 3_932_160];
        png2[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut png3_ok = vec![0x33u8; 2_621_440];
        png3_ok[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut png3_over = vec![0x44u8; 2_621_441];
        png3_over[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");

        {
            let mut map = io.files.lock().unwrap();
            map.insert("p1".into(), png1.clone());
            map.insert("p2".into(), png2.clone());
            map.insert("p3_ok".into(), png3_ok.clone());
            map.insert("p3_over".into(), png3_over);
        }
        let (_stop_tx, stop_rx) = watch::channel(false);

        // 未声明 file_size 时，第 3 张下载传入的 max_bytes 精确收敛为剩余额度 2_621_440；
        // 同时保留纯空白 caption（不做空白裁剪），忽略空字符串 caption，以 \n 拼接
        let outcome = prepare_media_turn(
            &io,
            &[
                IncomingMediaItem::image("p1".into(), None, Some("  首段保留空白  ".into())),
                IncomingMediaItem::image("p2".into(), None, Some(String::new())),
                IncomingMediaItem::image("p3_ok".into(), None, Some("末段说明".into())),
            ],
            stop_rx.clone(),
        )
        .await;
        assert_eq!(
            outcome,
            MediaBatchOutcome::Ready(vec![
                UserInputPart::Image { image: png1 },
                UserInputPart::Image { image: png2 },
                UserInputPart::Image { image: png3_ok },
                UserInputPart::Text {
                    text: "  首段保留空白  \n末段说明".into(),
                },
            ])
        );

        let recorded_calls = io.calls.lock().unwrap().clone();
        assert_eq!(
            recorded_calls,
            vec![
                ("p1".into(), 3_932_160),
                ("p2".into(), 3_932_160),
                ("p3_ok".into(), 2_621_440),
            ],
            "每张图片下载上限必须取单图上限与剩余总字节额度的较小值"
        );

        // 超过 10 MiB 剩余额度 1 字节时拒绝整组
        let over_outcome = prepare_media_turn(
            &io,
            &[
                IncomingMediaItem::image("p1".into(), None, None),
                IncomingMediaItem::image("p2".into(), None, None),
                IncomingMediaItem::image("p3_over".into(), None, None),
            ],
            stop_rx,
        )
        .await;
        assert!(
            matches!(over_outcome, MediaBatchOutcome::Rejected(ref msg) if msg.contains("10 MiB")),
            "累计下载字节越过 10 MiB 必须整组拒绝: {over_outcome:?}"
        );
    }
}
