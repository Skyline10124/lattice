// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `local` 下各测试模块共用的辅助函数。

use std::time::Duration;

/// 保持暂停的虚拟时钟不动，等待 `future` 完成；`stage` 在卡死时指出是哪一步。
///
/// 用于 `start_paused = true` 的 current-thread 运行时。运行时空闲时，暂停的时钟会自动
/// 跳到下一个计时点，是否跳、跳到哪一点取决于阻塞线程与真实 I/O 的完成顺序。这里每轮只
/// 轮询一次 `future`，未完成就 `yield_now` 让出执行权。有让出的任务等待重新调度时，
/// 调度器只以零超时检查驱动，时钟推进的时长为零（依据 Tokio 1.52.3 的 current-thread
/// 调度器与时间驱动）。阻塞线程照常运行，驱动照常检查真实 I/O，但每一轮让出不保证它们完成。
///
/// 所等的事件必须能在当前虚拟时刻完成，不能依赖尚未到期的计时器。真实时间 10 秒只用于
/// 报告卡死，不参与业务期限判断。
pub(super) async fn without_auto_advance<T>(
    stage: &str,
    future: impl std::future::Future<Output = T>,
) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    tokio::pin!(future);
    loop {
        if let std::task::Poll::Ready(value) = futures::poll!(future.as_mut()) {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{stage}：固定模拟时刻等待超过十秒真实时间"
        );
        tokio::task::yield_now().await;
    }
}
