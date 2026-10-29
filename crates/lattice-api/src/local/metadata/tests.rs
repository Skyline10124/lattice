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
