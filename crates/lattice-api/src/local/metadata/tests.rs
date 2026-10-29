// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Semaphore, mpsc};

#[tokio::test]
async fn concurrent_callers_share_failure_and_the_next_call_can_retry() {
    let started = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Semaphore::new(0));
    let refresh = MetadataRefresh::with_work(None, {
        let started = started.clone();
        let release = release.clone();
        move |_| {
            started.fetch_add(1, Ordering::Relaxed);
            let release = release.clone();
            Box::pin(async move {
                release.acquire().await.unwrap().forget();
                Err(LocalError::new(
                    LocalErrorKind::Transport,
                    "metadata server returned HTTP 503",
                ))
            })
        }
    });
    let first = tokio::spawn({
        let refresh = refresh.clone();
        async move { refresh.refresh().await }
    });
    while started.load(Ordering::Relaxed) == 0 {
        tokio::task::yield_now().await;
    }
    let mut second = Box::pin(refresh.refresh());
    assert!(second.as_mut().now_or_never().is_none());
    release.add_permits(1);
    let error = first.await.unwrap().unwrap_err();
    assert_eq!(second.await.unwrap_err(), error);
    assert_eq!(started.load(Ordering::Relaxed), 1);
    release.add_permits(1);
    assert_eq!(refresh.refresh().await.unwrap_err(), error);
    assert_eq!(started.load(Ordering::Relaxed), 2);
    refresh.stop();
}

#[tokio::test(start_paused = true)]
async fn startup_and_daily_schedule_share_manual_refresh_and_stop_cancels_waiters() {
    let (started, mut calls) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let refresh = MetadataRefresh::with_work(None, {
        let release = release.clone();
        move |cancel| {
            let release = release.clone();
            started.send(cancel).unwrap();
            Box::pin(async move {
                release.acquire().await.unwrap().forget();
                Ok(())
            })
        }
    });
    let initial_time = tokio::time::Instant::now();
    let timer = refresh.start();
    let cancel = calls.recv().await.unwrap();
    assert_eq!(tokio::time::Instant::now(), initial_time);
    let mut manual = Box::pin(refresh.refresh());
    assert!(manual.as_mut().now_or_never().is_none());
    assert!(calls.try_recv().is_err());
    release.add_permits(1);
    manual.await.unwrap();
    tokio::time::advance(Duration::from_secs(86_399)).await;
    tokio::task::yield_now().await;
    assert!(calls.try_recv().is_err());
    tokio::time::advance(Duration::from_secs(1)).await;
    calls.recv().await.unwrap();
    assert_eq!(
        tokio::time::Instant::now(),
        initial_time + Duration::from_secs(86_400)
    );
    let mut manual = Box::pin(refresh.refresh());
    assert!(manual.as_mut().now_or_never().is_none());
    refresh.stop();
    assert_eq!(manual.await.unwrap_err().kind, LocalErrorKind::Stopping);
    timer.await.unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(
        refresh.refresh().await.unwrap_err().kind,
        LocalErrorKind::Stopping
    );
    tokio::time::advance(Duration::from_secs(86_400)).await;
    assert!(calls.try_recv().is_err());
}

#[tokio::test]
async fn automatic_refresh_reloads_configuration_and_manual_refresh_ignores_the_switch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    let calls = Arc::new(AtomicUsize::new(0));
    let refresh = MetadataRefresh::with_work(Some(Ok(directory.path().to_owned())), {
        let calls = calls.clone();
        move |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(()) })
        }
    });
    std::fs::write(&path, "[metadata]\nauto_refresh = false").unwrap();
    refresh.automatic_refresh().await;
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    refresh.refresh().await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    std::fs::write(&path, "[metadata]\nauto_refresh = true").unwrap();
    refresh.automatic_refresh().await;
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    std::fs::write(&path, "[metadata]\nauto_refresh = false").unwrap();
    refresh.automatic_refresh().await;
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    std::fs::write(&path, "[metadata]\nauto_refresh = true\nunknown = true").unwrap();
    refresh.automatic_refresh().await;
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    refresh.stop();
}

/// 测试中的墙钟：固定起点加上暂停的 tokio 时钟走过的时间，缓存新鲜度随 `advance` 变化。
const BASE: Duration = Duration::from_secs(1_800_000_000);

fn wall(elapsed: Duration) -> SystemTime {
    SystemTime::UNIX_EPOCH + BASE + elapsed
}

/// 定时任务报告的事件流；`armed` 暂存已读到、尚未消费的下一次等待截止时刻。
struct Timer {
    events: mpsc::UnboundedReceiver<TimerEvent>,
    armed: Option<tokio::time::Instant>,
}

/// 换上跟随暂停时钟的墙钟，并返回定时任务事件的接收端。
fn observed(mut refresh: Arc<MetadataRefresh>) -> (Arc<MetadataRefresh>, Timer) {
    let start = tokio::time::Instant::now();
    let inner = Arc::get_mut(&mut refresh).expect("refresh is not shared yet");
    inner.clock = Box::new(move || wall(tokio::time::Instant::now() - start));
    let (sender, events) = mpsc::unbounded_channel();
    *inner.outcomes.lock().unwrap() = Some(sender);
    (
        refresh,
        Timer {
            events,
            armed: None,
        },
    )
}

/// 等待定时任务的下一个事件。触发里的配置与缓存读取跑在 spawn_blocking 线程上；
/// 若用 `recv().await` 让运行时空闲，暂停的时钟会自动跳到下一个定时点。
/// 这里只让出执行权，时钟保持不动；真实时间只用作卡死时的失败期限。
async fn next_event(timer: &mut Timer) -> TimerEvent {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(event) = timer.events.try_recv() {
            return event;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no timer event within 5s of real time"
        );
        tokio::task::yield_now().await;
    }
}

/// 等待下一次定时触发的结果，跳过其间的等待事件。
async fn next_outcome(timer: &mut Timer) -> AutoRefresh {
    timer.armed = None;
    loop {
        if let TimerEvent::Fired(outcome) = next_event(timer).await {
            return outcome;
        }
    }
}

/// 等待定时任务进入下一次等待，返回其截止时刻。收到这个事件时任务已停在
/// `sleep_until` 上，之后只有时钟到点才会再运行。
async fn next_armed(timer: &mut Timer) -> tokio::time::Instant {
    if let Some(armed) = timer.armed.take() {
        return armed;
    }
    match next_event(timer).await {
        TimerEvent::Armed(armed) => armed,
        TimerEvent::Fired(outcome) => {
            panic!("expected the timer to wait, but it fired {outcome:?}")
        }
    }
}

/// 断言“尚未触发”前让定时器任务运行的轮数。
const SETTLE_ROUNDS: usize = 16;

/// 断言定时器下一次等待到 `at`：到 `at` 前 1ms 尚未触发，到 `at` 时恰好触发一次并得到
/// `expected`，随后进入更晚的下一次等待。
async fn assert_fires_once_at(
    initial: tokio::time::Instant,
    at: Duration,
    timer: &mut Timer,
    expected: AutoRefresh,
) {
    let armed = next_armed(timer).await;
    assert_eq!(
        armed - initial,
        at,
        "next trigger must be scheduled at {}s",
        at.as_secs()
    );
    tokio::time::advance(armed - Duration::from_millis(1) - tokio::time::Instant::now()).await;
    // 多轮让出并推进 0 时长，让定时器任务把能做的都做完，再断言仍未触发；
    // 只让出一次时，晚一轮才发出的错误触发会被漏看。
    for round in 0..SETTLE_ROUNDS {
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::ZERO).await;
        assert!(
            timer.events.try_recv().is_err(),
            "trigger must not fire 1ms before {}s (settle round {round})",
            at.as_secs()
        );
    }
    tokio::time::advance(Duration::from_millis(1)).await;
    match next_event(timer).await {
        TimerEvent::Fired(outcome) => assert_eq!(
            outcome,
            expected,
            "outcome of the trigger at {}s",
            at.as_secs()
        ),
        TimerEvent::Armed(other) => panic!(
            "trigger at {}s must fire, but the timer re-armed for {:?}",
            at.as_secs(),
            other - initial
        ),
    }
    let next = next_armed(timer).await;
    assert!(
        next > armed,
        "trigger at {}s must fire only once; next wait ends at {:?}",
        at.as_secs(),
        next - initial
    );
    timer.armed = Some(next);
}

#[tokio::test(start_paused = true)]
async fn scheduled_triggers_skip_while_auto_refresh_is_disabled() {
    let directory = tempfile::tempdir().unwrap();
    write_cache(
        directory.path(),
        wall(Duration::ZERO) - Duration::from_secs(25 * 60 * 60),
    );
    let path = directory.path().join("providers.toml");
    std::fs::write(&path, "[metadata]\nauto_refresh = false\n").unwrap();
    let (started, mut calls) = mpsc::unbounded_channel();
    let (refresh, mut outcomes) = observed(scheduled(directory.path(), started));
    let initial = tokio::time::Instant::now();
    let timer = refresh.start();
    assert_eq!(
        next_outcome(&mut outcomes).await,
        AutoRefresh::Idle,
        "startup trigger is skipped"
    );
    assert_eq!(
        tokio::time::Instant::now(),
        initial,
        "startup trigger runs immediately"
    );
    assert_fires_once_at(
        initial,
        Duration::from_secs(24 * 60 * 60),
        &mut outcomes,
        AutoRefresh::Idle,
    )
    .await;
    assert!(
        calls.try_recv().is_err(),
        "auto_refresh = false never fetches"
    );
    std::fs::write(&path, "[metadata]\nauto_refresh = true\n").unwrap();
    assert_fires_once_at(
        initial,
        Duration::from_secs(48 * 60 * 60),
        &mut outcomes,
        AutoRefresh::Ran,
    )
    .await;
    assert!(
        calls.try_recv().is_ok(),
        "the enabled switch refreshes a stale cache"
    );
    assert!(calls.try_recv().is_err(), "one trigger refreshes once");
    refresh.stop();
    timer.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn invalid_configuration_retries_with_bounded_backoff() {
    let directory = tempfile::tempdir().unwrap();
    write_cache(
        directory.path(),
        wall(Duration::ZERO) - Duration::from_secs(25 * 60 * 60),
    );
    let path = directory.path().join("providers.toml");
    std::fs::write(&path, "[metadata]\nauto_refresh = true\nunknown = true\n").unwrap();
    let (started, mut calls) = mpsc::unbounded_channel();
    let (refresh, mut outcomes) = observed(scheduled(directory.path(), started));
    let initial = tokio::time::Instant::now();
    let timer = refresh.start();
    assert_eq!(
        next_outcome(&mut outcomes).await,
        AutoRefresh::Failed,
        "invalid providers.toml counts as a failed trigger"
    );
    assert_fires_once_at(
        initial,
        Duration::from_secs(60),
        &mut outcomes,
        AutoRefresh::Failed,
    )
    .await;
    assert_fires_once_at(
        initial,
        Duration::from_secs(180),
        &mut outcomes,
        AutoRefresh::Failed,
    )
    .await;
    assert!(
        calls.try_recv().is_err(),
        "invalid configuration never fetches"
    );
    std::fs::write(&path, "[metadata]\nauto_refresh = true\n").unwrap();
    assert_fires_once_at(
        initial,
        Duration::from_secs(420),
        &mut outcomes,
        AutoRefresh::Ran,
    )
    .await;
    assert!(
        calls.try_recv().is_ok(),
        "the repaired configuration refreshes"
    );
    assert!(calls.try_recv().is_err(), "one trigger refreshes once");
    refresh.stop();
    timer.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn automatic_refresh_skips_a_fresh_cache_and_fetches_a_stale_one() {
    let fresh = tempfile::tempdir().unwrap();
    // 23 小时前写入，按注入的墙钟还剩 1 小时到期。
    write_cache(
        fresh.path(),
        wall(Duration::ZERO) - Duration::from_secs(23 * 60 * 60),
    );
    let stale = tempfile::tempdir().unwrap();
    write_cache(
        stale.path(),
        wall(Duration::ZERO) - Duration::from_secs(25 * 60 * 60),
    );
    let (fresh_started, mut fresh_calls) = mpsc::unbounded_channel();
    let (stale_started, mut stale_calls) = mpsc::unbounded_channel();
    let (fresh_refresh, mut fresh_outcomes) = observed(scheduled(fresh.path(), fresh_started));
    let (stale_refresh, mut stale_outcomes) = observed(scheduled(stale.path(), stale_started));
    let initial = tokio::time::Instant::now();
    let fresh_timer = fresh_refresh.start();
    let stale_timer = stale_refresh.start();
    assert_eq!(next_outcome(&mut stale_outcomes).await, AutoRefresh::Ran);
    assert_eq!(
        tokio::time::Instant::now(),
        initial,
        "a cache older than 24h must be refreshed at startup"
    );
    assert!(stale_calls.try_recv().is_ok(), "stale cache fetches once");
    assert_fires_once_at(
        initial,
        Duration::from_secs(60 * 60),
        &mut fresh_outcomes,
        AutoRefresh::Ran,
    )
    .await;
    assert!(
        fresh_calls.try_recv().is_ok(),
        "a cache newer than 24h is refreshed only when it expires"
    );
    assert!(fresh_calls.try_recv().is_err(), "expiry refreshes once");
    fresh_refresh.stop();
    stale_refresh.stop();
    fresh_timer.await.unwrap();
    stale_timer.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn failed_automatic_refresh_retries_with_bounded_backoff() {
    let (started, calls) = mpsc::unbounded_channel();
    let (refresh, mut outcomes) = observed(MetadataRefresh::with_work(None, move |_| {
        started.send(()).unwrap();
        Box::pin(async {
            Err(LocalError::new(
                LocalErrorKind::Transport,
                "metadata server returned HTTP 503",
            ))
        })
    }));
    let initial = tokio::time::Instant::now();
    let timer = refresh.start();
    assert_eq!(next_outcome(&mut outcomes).await, AutoRefresh::Failed);
    assert_eq!(
        tokio::time::Instant::now(),
        initial,
        "startup trigger runs immediately"
    );
    let mut elapsed = 0u64;
    for gap in [60_u64, 120, 240, 480, 960, 1_920, 3_600, 3_600] {
        elapsed += gap;
        assert_fires_once_at(
            initial,
            Duration::from_secs(elapsed),
            &mut outcomes,
            AutoRefresh::Failed,
        )
        .await;
    }
    assert_eq!(calls.len(), 9, "startup plus eight retries each fetch once");
    refresh.stop();
    timer.await.unwrap();
}

fn write_cache(directory: &std::path::Path, modified: std::time::SystemTime) {
    let cache = directory.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let path = cache.join("models.json");
    std::fs::write(&path, b"{}\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

fn scheduled(
    directory: &std::path::Path,
    started: mpsc::UnboundedSender<()>,
) -> Arc<MetadataRefresh> {
    let directory = directory.to_owned();
    MetadataRefresh::with_work(Some(Ok(directory)), move |_| {
        started.send(()).unwrap();
        Box::pin(async { Ok(()) })
    })
}
