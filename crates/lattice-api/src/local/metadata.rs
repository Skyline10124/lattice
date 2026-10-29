// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 定时与手动刷新共享一次在途操作；停止信号独立于 Turn 排空。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{FutureExt, future::BoxFuture};
use lattice_llm::MetadataRefreshControl;
use tokio::sync::watch;

use super::{LocalError, LocalErrorKind, Result};

const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
type Work = dyn Fn(Arc<MetadataRefreshControl>) -> BoxFuture<'static, Result<()>> + Send + Sync;
type Completion = watch::Receiver<Option<Result<()>>>;

pub(super) struct MetadataRefresh {
    directory: Option<std::result::Result<PathBuf, crate::ConfigRejection>>,
    work: Box<Work>,
    flight: Mutex<Option<Completion>>,
    cancel: Arc<MetadataRefreshControl>,
    stopped: watch::Sender<bool>,
}

fn stopping() -> LocalError {
    LocalError::new(
        LocalErrorKind::Stopping,
        "backend is stopping; metadata refresh cancelled",
    )
}

impl MetadataRefresh {
    pub(super) fn new(
        gateway: std::result::Result<Arc<lattice_llm::RequestGateway>, crate::ConfigRejection>,
        directory: std::result::Result<PathBuf, crate::ConfigRejection>,
    ) -> Arc<Self> {
        Self::with_work(Some(directory), move |cancel| {
            let gateway = gateway.clone();
            Box::pin(async move {
                let gateway = gateway
                    .map_err(|error| LocalError::from(crate::ApiError::Configuration(error)))?;
                let Err(error) = gateway.refresh_metadata(cancel).await else {
                    return Ok(());
                };
                let auth = gateway.auth_store();
                Err(tokio::task::spawn_blocking(move || {
                    let kind = match &error {
                        lattice_llm::MetadataError::Configuration(_) => LocalErrorKind::BadRequest,
                        lattice_llm::MetadataError::Cancelled => LocalErrorKind::Stopping,
                        lattice_llm::MetadataError::Io(error)
                            if error.kind() == std::io::ErrorKind::PermissionDenied =>
                        {
                            LocalErrorKind::Permission
                        }
                        lattice_llm::MetadataError::Io(_)
                        | lattice_llm::MetadataError::Worker(_) => LocalErrorKind::Internal,
                        _ => LocalErrorKind::Transport,
                    };
                    LocalError::new(
                        kind,
                        &crate::ConfigRejection::new(&error.to_string(), Some(&auth)).to_string(),
                    )
                })
                .await
                .map_err(|_| {
                    LocalError::new(
                        LocalErrorKind::Internal,
                        "metadata error processing task failed",
                    )
                })?)
            })
        })
    }

    fn with_work(
        directory: Option<std::result::Result<PathBuf, crate::ConfigRejection>>,
        work: impl Fn(Arc<MetadataRefreshControl>) -> BoxFuture<'static, Result<()>>
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            directory,
            work: Box::new(work),
            flight: Mutex::new(None),
            cancel: Arc::new(MetadataRefreshControl::default()),
            stopped: watch::channel(false).0,
        })
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(super) fn without_network() -> Arc<Self> {
        Self::with_work(None, |_| Box::pin(async { Ok(()) }))
    }

    async fn automatic_refresh(self: &Arc<Self>) {
        let enabled = match self.directory.clone() {
            Some(directory) => tokio::task::spawn_blocking(move || {
                let directory = directory
                    .map_err(|error| LocalError::from(crate::ApiError::Configuration(error)))?;
                let auth = lattice_llm::auth::AuthStore::new(directory.clone());
                lattice_llm::metadata_auto_refresh_enabled(&directory).map_err(|error| {
                    LocalError::new(
                        LocalErrorKind::BadRequest,
                        &crate::ConfigRejection::new(&error.to_string(), Some(&auth)).to_string(),
                    )
                })
            })
            .await
            .unwrap_or_else(|_| {
                Err(LocalError::new(
                    LocalErrorKind::Internal,
                    "metadata configuration task failed",
                ))
            }),
            None => Ok(true),
        };
        match enabled {
            Ok(true) => {
                let _ = self.refresh().await;
            }
            Ok(false) => tracing::info!("automatic model metadata refresh disabled"),
            Err(error) => tracing::warn!(%error, "automatic model metadata refresh skipped"),
        }
    }

    pub(super) fn start(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let refresh = self.clone();
        tokio::spawn(async move {
            let mut stopped = refresh.stopped.subscribe();
            let mut timer = tokio::time::interval(REFRESH_INTERVAL);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = stopped.wait_for(|stopped| *stopped) => break,
                    _ = timer.tick() => {},
                }
                tokio::select! {
                    biased;
                    _ = stopped.wait_for(|stopped| *stopped) => break,
                    _ = refresh.automatic_refresh() => {},
                }
            }
        })
    }

    pub(super) fn stop(&self) {
        let _flight = self.flight.lock().expect("metadata refresh lock");
        self.cancel.cancel();
        self.stopped.send_replace(true);
    }

    pub(super) async fn refresh(self: &Arc<Self>) -> Result<()> {
        let mut completion = {
            let mut flight = self.flight.lock().expect("metadata refresh lock");
            if self.cancel.is_cancelled() {
                return Err(stopping());
            }
            if let Some(completion) = &*flight {
                completion.clone()
            } else {
                let (done, completion) = watch::channel(None);
                *flight = Some(completion.clone());
                let refresh = self.clone();
                tokio::spawn(async move {
                    let mut stopped = refresh.stopped.subscribe();
                    let work = std::panic::AssertUnwindSafe(async {
                        (refresh.work)(refresh.cancel.clone()).await
                    })
                    .catch_unwind();
                    let result = tokio::select! {
                        biased;
                        _ = stopped.wait_for(|stopped| *stopped) => Err(stopping()),
                        result = work => result.unwrap_or_else(|_| Err(LocalError::new(LocalErrorKind::Internal, "metadata refresh task failed"))),
                    };
                    if let Err(error) = &result
                        && error.kind != LocalErrorKind::Stopping
                    {
                        tracing::warn!(%error, "model metadata refresh failed");
                    }
                    let mut flight = refresh.flight.lock().expect("metadata refresh lock");
                    flight.take();
                    done.send_replace(Some(result));
                });
                completion
            }
        };
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result;
            }
            completion.changed().await.map_err(|_| {
                LocalError::new(
                    LocalErrorKind::Internal,
                    "metadata refresh result unavailable",
                )
            })?;
        }
    }
}

#[cfg(test)]
mod tests;
