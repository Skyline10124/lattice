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

#[tokio::test(start_paused = true)]
async fn automatic_refresh_skips_a_fresh_cache_and_fetches_a_stale_one() {
    let fresh = tempfile::tempdir().unwrap();
    write_cache(fresh.path(), std::time::SystemTime::now());
    let stale = tempfile::tempdir().unwrap();
    write_cache(
        stale.path(),
        std::time::SystemTime::now() - Duration::from_secs(25 * 60 * 60),
    );
    let (fresh_started, mut fresh_calls) = mpsc::unbounded_channel();
    let (stale_started, mut stale_calls) = mpsc::unbounded_channel();
    let fresh_refresh = scheduled(fresh.path(), fresh_started);
    let stale_refresh = scheduled(stale.path(), stale_started);
    let initial = tokio::time::Instant::now();
    let fresh_timer = fresh_refresh.start();
    let stale_timer = stale_refresh.start();
    stale_calls.recv().await.unwrap();
    assert_eq!(
        tokio::time::Instant::now(),
        initial,
        "a cache older than 24h must be refreshed at startup"
    );
    tokio::time::advance(Duration::from_secs(60 * 60)).await;
    tokio::task::yield_now().await;
    assert!(
        fresh_calls.try_recv().is_err(),
        "a cache newer than 24h must not be refreshed"
    );
    fresh_refresh.stop();
    stale_refresh.stop();
    fresh_timer.await.unwrap();
    stale_timer.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn failed_automatic_refresh_retries_with_bounded_backoff() {
    let (started, mut calls) = mpsc::unbounded_channel();
    let refresh = MetadataRefresh::with_work(None, move |_| {
        started.send(()).unwrap();
        Box::pin(async {
            Err(LocalError::new(
                LocalErrorKind::Transport,
                "metadata server returned HTTP 503",
            ))
        })
    });
    let initial = tokio::time::Instant::now();
    let timer = refresh.start();
    calls.recv().await.unwrap();
    assert_eq!(tokio::time::Instant::now(), initial);
    let mut elapsed = 0u64;
    for gap in [60_u64, 120, 240, 480, 960, 1_920, 3_600, 3_600] {
        tokio::time::advance(Duration::from_secs(gap)).await;
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
        assert!(
            calls.try_recv().is_ok(),
            "failed refresh must retry after {gap}s (elapsed {elapsed}s)"
        );
        assert!(
            calls.try_recv().is_err(),
            "failed refresh retried more than once at the {gap}s mark"
        );
        elapsed += gap;
        assert_eq!(
            tokio::time::Instant::now(),
            initial + Duration::from_secs(elapsed),
            "backoff stays on the paused clock"
        );
    }
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
