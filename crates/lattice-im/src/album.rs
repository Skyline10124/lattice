// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Telegram `media_group_id` 多图相册内存收集器、静默窗口闭合与迟到分片识别。

use std::{
    collections::{BTreeMap, VecDeque},
    time::Duration,
};

use tokio::time::Instant;

use crate::{
    gateway::InFlightGuard,
    identity::{ChatKey, InboundId},
    media::IncomingMediaItem,
};

/// 相册收集静默窗口：每收到同对话同 `media_group_id` 的新分片即重置为 `now + 600ms`。
pub(crate) const ALBUM_QUIET_WINDOW: Duration = Duration::from_millis(600);

/// Telegram 单相册协议上限：收满 10 条立即闭合（不在 8 张处截断，使 9～10 张整组被拒一次）。
pub(crate) const ALBUM_MAX_ITEMS: usize = 10;

/// 每个对话内存中保留的近期已闭合 `media_group_id` 数量上限（FIFO 淘汰）。
pub(crate) const RECENT_CLOSED_ALBUMS_PER_CHAT: usize = 64;

/// 相册闭合后迟到到达的同 `media_group_id` 分片固定提示。
pub(crate) const LATE_ALBUM_FRAGMENT_REPLY: &str = "该相册部分图片延迟到达，未并入上一轮";

/// 暂存在开放相册中的单条分片（全程持有 `InFlightGuard` 直至移交单对话准入队列并完成落账或拒绝确认）。
pub(crate) struct BufferedAlbumItem {
    pub(crate) update_id: i64,
    pub(crate) inbound_id: InboundId,
    pub(crate) item: IncomingMediaItem,
    pub(crate) in_flight_guard: InFlightGuard,
}

struct OpenAlbum {
    media_group_id: String,
    deadline: Instant,
    items: BTreeMap<i64, BufferedAlbumItem>,
}

/// 已闭合的整组相册，全部成员严格按数值 `update_id` 升序排列。
pub(crate) struct ClosedAlbum {
    pub(crate) chat: ChatKey,
    pub(crate) primary_inbound_id: InboundId,
    pub(crate) merged_inbound_ids: Vec<InboundId>,
    pub(crate) media_items: Vec<IncomingMediaItem>,
    pub(crate) in_flight_guards: Vec<InFlightGuard>,
}

/// 由 `poll_updates_loop` 独占持有的相册收集状态机。
#[derive(Default)]
pub(crate) struct AlbumCollector {
    open: BTreeMap<ChatKey, OpenAlbum>,
    recent_closed: BTreeMap<ChatKey, VecDeque<String>>,
}

impl AlbumCollector {
    /// 判断指定对话的 `group_id` 是否命中近期已闭合集合。
    pub(crate) fn is_recently_closed(&self, chat: &ChatKey, group_id: &str) -> bool {
        self.recent_closed
            .get(chat)
            .is_some_and(|recent| recent.iter().any(|existing| existing == group_id))
    }

    /// 返回所有开放相册中最早到期的截止时刻。
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.open.values().map(|album| album.deadline).min()
    }

    /// 立即闭合指定对话当前的开放相册（若存在），记入近期已闭合集合并返回有序整组产物。
    pub(crate) fn close_for_chat(&mut self, chat: &ChatKey) -> Option<ClosedAlbum> {
        let open = self.open.remove(chat)?;
        self.record_recently_closed(chat, open.media_group_id);

        let total = open.items.len();
        let mut iter = open.items.into_values();
        let first = iter.next()?;

        let primary_inbound_id = first.inbound_id;
        let mut merged_inbound_ids = Vec::with_capacity(total.saturating_sub(1));
        let mut media_items = Vec::with_capacity(total);
        let mut in_flight_guards = Vec::with_capacity(total);

        media_items.push(first.item);
        in_flight_guards.push(first.in_flight_guard);

        for rest in iter {
            merged_inbound_ids.push(rest.inbound_id);
            media_items.push(rest.item);
            in_flight_guards.push(rest.in_flight_guard);
        }

        Some(ClosedAlbum {
            chat: chat.clone(),
            primary_inbound_id,
            merged_inbound_ids,
            media_items,
            in_flight_guards,
        })
    }

    /// 闭合全部截止时刻不晚于 `now` 的开放相册，按截止时刻升序产出。
    pub(crate) fn close_due(&mut self, now: Instant) -> Vec<ClosedAlbum> {
        let mut due_chats: Vec<(Instant, ChatKey)> = self
            .open
            .iter()
            .filter(|(_, album)| album.deadline <= now)
            .map(|(chat, album)| (album.deadline, chat.clone()))
            .collect();
        due_chats.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let mut closed = Vec::with_capacity(due_chats.len());
        for (_, chat) in due_chats {
            if let Some(album) = self.close_for_chat(&chat) {
                closed.push(album);
            }
        }
        closed
    }

    /// 若指定对话当前存在开放相册且其 `media_group_id` 与 `group_id` 不同，立即将其闭合返回。
    pub(crate) fn close_if_different_group(
        &mut self,
        chat: &ChatKey,
        group_id: &str,
    ) -> Option<ClosedAlbum> {
        if self
            .open
            .get(chat)
            .is_some_and(|existing| existing.media_group_id != group_id)
        {
            self.close_for_chat(chat)
        } else {
            None
        }
    }

    /// 向指定对话推入一条相册分片。
    ///
    /// 返回 `(被新相册顶替提前闭合的旧相册, 因收满 10 项立即闭合的当前相册)`。
    pub(crate) fn push_fragment(
        &mut self,
        chat: ChatKey,
        group_id: String,
        item: BufferedAlbumItem,
        now: Instant,
    ) -> (Option<ClosedAlbum>, Option<ClosedAlbum>) {
        let displaced = self.close_if_different_group(&chat, &group_id);

        let reached_max = match self.open.get_mut(&chat) {
            Some(open) => {
                if let std::collections::btree_map::Entry::Vacant(vacant) =
                    open.items.entry(item.update_id)
                {
                    vacant.insert(item);
                    open.deadline = now + ALBUM_QUIET_WINDOW;
                }
                open.items.len() >= ALBUM_MAX_ITEMS
            }
            None => {
                let mut items = BTreeMap::new();
                items.insert(item.update_id, item);
                let full = items.len() >= ALBUM_MAX_ITEMS;
                self.open.insert(
                    chat.clone(),
                    OpenAlbum {
                        media_group_id: group_id,
                        deadline: now + ALBUM_QUIET_WINDOW,
                        items,
                    },
                );
                full
            }
        };

        let closed_current = if reached_max {
            self.close_for_chat(&chat)
        } else {
            None
        };

        (displaced, closed_current)
    }

    fn record_recently_closed(&mut self, chat: &ChatKey, group_id: String) {
        let recent = self.recent_closed.entry(chat.clone()).or_default();
        if let Some(pos) = recent.iter().position(|existing| existing == &group_id) {
            recent.remove(pos);
        } else if recent.len() >= RECENT_CLOSED_ALBUMS_PER_CHAT {
            recent.pop_front();
        }
        recent.push_back(group_id);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex as StdMutex},
    };

    use super::*;
    use crate::telegram::{chat_key_from_id, inbound_id_from_update_id};

    fn make_buffered_image(
        in_flight_set: &Arc<StdMutex<BTreeSet<InboundId>>>,
        update_id: i64,
        file_id: &str,
        caption: Option<&str>,
    ) -> BufferedAlbumItem {
        let inbound_id = inbound_id_from_update_id(update_id);
        let guard = InFlightGuard::new_for_test(Arc::clone(in_flight_set), inbound_id.clone());
        let item =
            IncomingMediaItem::image(file_id.to_owned(), Some(128), caption.map(str::to_owned));
        BufferedAlbumItem {
            update_id,
            inbound_id,
            item,
            in_flight_guard: guard,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_window_boundary_at_599ms_and_600ms_and_fragment_resets_deadline() {
        let mut collector = AlbumCollector::default();
        let in_flight = Arc::new(StdMutex::new(BTreeSet::new()));
        let chat = chat_key_from_id(1001);
        let t0 = Instant::now();

        // 单分片边界：599ms 未到期，600ms 精确到期
        let (d0, c0) = collector.push_fragment(
            chat.clone(),
            "g-single".into(),
            make_buffered_image(&in_flight, 10, "f10", None),
            t0,
        );
        assert!(d0.is_none() && c0.is_none());
        assert_eq!(
            collector.next_deadline(),
            Some(t0 + Duration::from_millis(600))
        );
        assert!(
            collector
                .close_due(t0 + Duration::from_millis(599))
                .is_empty(),
            "599ms 时相册尚未满静默窗口，不得提前闭合"
        );
        assert!(!collector.is_recently_closed(&chat, "g-single"));

        let due = collector.close_due(t0 + Duration::from_millis(600));
        assert_eq!(due.len(), 1, "满 600ms 时相册必须立即闭合");
        assert_eq!(due[0].primary_inbound_id, inbound_id_from_update_id(10));
        assert!(collector.is_recently_closed(&chat, "g-single"));
        assert_eq!(collector.next_deadline(), None);

        // 后续分片到达将截止重置为最新分片时刻 + 600ms
        let t1 = t0 + Duration::from_secs(1);
        collector.push_fragment(
            chat.clone(),
            "g-reset".into(),
            make_buffered_image(&in_flight, 20, "f20", None),
            t1,
        );
        let t2 = t1 + Duration::from_millis(450);
        collector.push_fragment(
            chat.clone(),
            "g-reset".into(),
            make_buffered_image(&in_flight, 21, "f21", None),
            t2,
        );
        assert_eq!(
            collector.next_deadline(),
            Some(t2 + Duration::from_millis(600)),
            "新分片必须将静默截止重置为到达时刻加 600ms"
        );
        assert!(
            collector
                .close_due(t1 + Duration::from_millis(600))
                .is_empty(),
            "原首片 600ms 时刻不得闭合已被新分片续期的相册"
        );
        assert!(
            collector
                .close_due(t2 + Duration::from_millis(599))
                .is_empty(),
            "重置后的 599ms 边界仍不得闭合"
        );
        let due_reset = collector.close_due(t2 + Duration::from_millis(600));
        assert_eq!(due_reset.len(), 1);
        assert_eq!(due_reset[0].media_items.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn orders_out_of_order_update_ids_numerically_selecting_99_before_100() {
        let mut collector = AlbumCollector::default();
        let in_flight = Arc::new(StdMutex::new(BTreeSet::new()));
        let chat = chat_key_from_id(1002);
        let now = Instant::now();

        // 字典序中 "tg:100" < "tg:99"，但数值序中 99 < 100 < 102；验证按数值 update_id 升序产出且守卫全程不断档
        collector.push_fragment(
            chat.clone(),
            "g-order".into(),
            make_buffered_image(&in_flight, 102, "img_102", Some("说明102")),
            now,
        );
        collector.push_fragment(
            chat.clone(),
            "g-order".into(),
            make_buffered_image(&in_flight, 100, "img_100", Some("说明100")),
            now,
        );
        collector.push_fragment(
            chat.clone(),
            "g-order".into(),
            make_buffered_image(&in_flight, 99, "img_99", Some("说明99")),
            now,
        );

        assert_eq!(
            in_flight.lock().unwrap().len(),
            3,
            "缓冲期间全部分片的在途去重守卫必须持续存活"
        );

        let closed = collector
            .close_for_chat(&chat)
            .expect("开放相册闭合应返回产物");
        assert_eq!(
            in_flight.lock().unwrap().len(),
            3,
            "闭合移交阶段不得提前释放任何分片的在途去重守卫"
        );
        assert_eq!(
            closed.primary_inbound_id,
            inbound_id_from_update_id(99),
            "首条必须为数值 update_id 最小的 tg:99，而非字典序较小的 tg:100"
        );
        assert_eq!(
            closed.merged_inbound_ids,
            vec![
                inbound_id_from_update_id(100),
                inbound_id_from_update_id(102),
            ]
        );
        assert_eq!(
            closed.media_items,
            vec![
                IncomingMediaItem::image("img_99".into(), Some(128), Some("说明99".into())),
                IncomingMediaItem::image("img_100".into(), Some(128), Some("说明100".into())),
                IncomingMediaItem::image("img_102".into(), Some(128), Some("说明102".into())),
            ]
        );
        assert_eq!(closed.in_flight_guards.len(), 3);

        drop(closed);
        assert!(
            in_flight.lock().unwrap().is_empty(),
            "丢弃 ClosedAlbum 后整组守卫应自动清空在途标记"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn keeps_nine_items_open_and_closes_immediately_on_tenth_item() {
        let mut collector = AlbumCollector::default();
        let in_flight = Arc::new(StdMutex::new(BTreeSet::new()));
        let chat = chat_key_from_id(1003);
        let now = Instant::now();

        for idx in 1..=9 {
            let (displaced, closed) = collector.push_fragment(
                chat.clone(),
                "g-ten".into(),
                make_buffered_image(&in_flight, 300 + idx, &format!("f_{idx}"), None),
                now,
            );
            assert!(
                displaced.is_none() && closed.is_none(),
                "前 9 项（含超单轮 8 张上限的第 9 项）必须继续等待，不在第 8 或第 9 项提前截断闭合"
            );
        }
        assert!(collector.next_deadline().is_some());
        assert!(!collector.is_recently_closed(&chat, "g-ten"));

        let (displaced, closed_tenth) = collector.push_fragment(
            chat.clone(),
            "g-ten".into(),
            make_buffered_image(&in_flight, 310, "f_10", None),
            now,
        );
        assert!(displaced.is_none());
        let closed = closed_tenth.expect("收满 Telegram 协议上限 10 项时必须立即闭合");
        assert_eq!(closed.media_items.len(), 10);
        assert_eq!(closed.primary_inbound_id, inbound_id_from_update_id(301));
        assert_eq!(closed.merged_inbound_ids.len(), 9);
        assert!(collector.is_recently_closed(&chat, "g-ten"));
        assert_eq!(collector.next_deadline(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn closes_open_album_when_same_chat_switches_group_id_and_isolates_across_chats() {
        let mut collector = AlbumCollector::default();
        let in_flight = Arc::new(StdMutex::new(BTreeSet::new()));
        let chat_a = chat_key_from_id(2001);
        let chat_b = chat_key_from_id(2002);
        let t0 = Instant::now();

        collector.push_fragment(
            chat_a.clone(),
            "ga-1".into(),
            make_buffered_image(&in_flight, 401, "fa1", None),
            t0,
        );
        collector.push_fragment(
            chat_b.clone(),
            "gb-1".into(),
            make_buffered_image(&in_flight, 501, "fb1", None),
            t0 + Duration::from_millis(100),
        );

        // 同对话 chat_a 切换为新 media_group_id "ga-2"：立即提前闭合 "ga-1"，但绝不影响 chat_b 的 "gb-1"
        let (displaced_a, closed_a2) = collector.push_fragment(
            chat_a.clone(),
            "ga-2".into(),
            make_buffered_image(&in_flight, 402, "fa2", None),
            t0 + Duration::from_millis(200),
        );
        assert!(closed_a2.is_none());
        let closed_a1 = displaced_a.expect("同对话收到新 media_group_id 必须提前闭合旧相册");
        assert_eq!(closed_a1.chat, chat_a);
        assert_eq!(closed_a1.primary_inbound_id, inbound_id_from_update_id(401));
        assert!(collector.is_recently_closed(&chat_a, "ga-1"));
        assert!(!collector.is_recently_closed(&chat_a, "ga-2"));
        assert!(!collector.is_recently_closed(&chat_b, "gb-1"));

        // 同组名调用 close_if_different_group 不闭合，不同组名调用立即闭合并记入近期已闭合集合
        assert!(
            collector
                .close_if_different_group(&chat_a, "ga-2")
                .is_none(),
            "同 media_group_id 调用 close_if_different_group 不得提前闭合当前开放相册"
        );
        let closed_a2_manual = collector
            .close_if_different_group(&chat_a, "ga-1")
            .expect("收到另一 media_group_id（含此前已闭合的组）时必须提前闭合 ga-2");
        assert_eq!(
            closed_a2_manual.primary_inbound_id,
            inbound_id_from_update_id(402)
        );
        assert!(collector.is_recently_closed(&chat_a, "ga-1"));
        assert!(collector.is_recently_closed(&chat_a, "ga-2"));
        assert!(
            collector
                .close_if_different_group(&chat_a, "ga-3")
                .is_none(),
            "无开放相册时调用 close_if_different_group 应返回 None"
        );
        assert_eq!(
            collector.next_deadline(),
            Some(t0 + Duration::from_millis(700)),
            "跨对话 chat_b 的开放相册截止时刻应原样保留"
        );

        let closed_b = collector
            .close_for_chat(&chat_b)
            .expect("gb-1 应仍保持开放");
        assert_eq!(closed_b.chat, chat_b);
        assert_eq!(closed_b.primary_inbound_id, inbound_id_from_update_id(501));
    }

    #[tokio::test(start_paused = true)]
    async fn evicts_oldest_closed_group_id_in_fifo_order_after_64_per_chat() {
        let mut collector = AlbumCollector::default();
        let in_flight = Arc::new(StdMutex::new(BTreeSet::new()));
        let chat_a = chat_key_from_id(3001);
        let chat_b = chat_key_from_id(3002);
        let now = Instant::now();

        for idx in 1..=64 {
            let group_id = format!("album-{idx}");
            collector.push_fragment(
                chat_a.clone(),
                group_id,
                make_buffered_image(&in_flight, 1000 + idx, "f", None),
                now,
            );
            assert!(collector.close_for_chat(&chat_a).is_some());
        }

        for idx in 1..=64 {
            let group_id = format!("album-{idx}");
            assert!(
                collector.is_recently_closed(&chat_a, &group_id),
                "前 64 个已闭合相册 {group_id} 均应保留在近期集合内"
            );
            assert!(
                !collector.is_recently_closed(&chat_b, &group_id),
                "近期已闭合集合必须按对话隔离"
            );
        }

        // 闭合第 65 个不同相册：第 1 个被 FIFO 挤出，第 2..=65 个全部保留
        collector.push_fragment(
            chat_a.clone(),
            "album-65".into(),
            make_buffered_image(&in_flight, 1065, "f", None),
            now,
        );
        assert!(collector.close_for_chat(&chat_a).is_some());

        assert!(
            !collector.is_recently_closed(&chat_a, "album-1"),
            "闭合第 65 个相册后最早的 album-1 必须被 FIFO 淘汰"
        );
        for idx in 2..=65 {
            let group_id = format!("album-{idx}");
            assert!(
                collector.is_recently_closed(&chat_a, &group_id),
                "第 {idx} 个相册仍应保留在最近 64 个集合内"
            );
        }
    }
}
