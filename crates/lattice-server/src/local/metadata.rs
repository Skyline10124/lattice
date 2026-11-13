// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 两种模型目录各自共享在途操作，由同一个定时任务调度。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::{FutureExt, future::BoxFuture};
use lattice_llm::{ChatgptCatalogState, MetadataRefreshControl};
use tokio::sync::watch;
use tokio::time::Instant;

use super::{LocalError, LocalErrorKind, Result};

const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const INITIAL_BACKOFF: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    ModelsDev,
    Chatgpt,
}

impl Source {
    fn index(self) -> usize {
        match self {
            Self::ModelsDev => 0,
            Self::Chatgpt => 1,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::ModelsDev => "models.dev",
            Self::Chatgpt => "chatgpt",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoRefresh {
    Ran,
    Fresh,
    Idle,
    Failed,
    Stopped,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimerEvent {
    Armed(Instant),
    Fired(AutoRefresh),
}

fn backoff(failures: u32) -> Duration {
    INITIAL_BACKOFF
        .saturating_mul(1_u32 << failures.saturating_sub(1).min(16))
        .min(MAX_BACKOFF)
}

fn freshness(fetched_at: SystemTime, now: SystemTime) -> Option<Duration> {
    REFRESH_INTERVAL
        .checked_sub(now.duration_since(fetched_at).unwrap_or(Duration::ZERO))
        .filter(|remaining| !remaining.is_zero())
}

fn cache_freshness(directory: &Path, now: SystemTime) -> Option<Duration> {
    freshness(
        std::fs::metadata(directory.join("cache/models.json"))
            .ok()?
            .modified()
            .ok()?,
        now,
    )
}

type Work =
    dyn Fn(Source, Arc<MetadataRefreshControl>) -> BoxFuture<'static, Result<usize>> + Send + Sync;
type Catalog = dyn Fn() -> BoxFuture<'static, Result<ChatgptCatalogState>> + Send + Sync;
type Completion = watch::Receiver<Option<Result<usize>>>;
type Clock = dyn Fn() -> SystemTime + Send + Sync;

#[derive(Default)]
struct SourceState {
    flight: Option<Completion>,
    failures: u32,
    next_attempt: Option<Instant>,
}

#[derive(Default)]
struct State {
    sources: [SourceState; 2],
    configuration_failures: u32,
    configuration_next: Option<Instant>,
}

pub(super) struct MetadataRefresh {
    directory: Option<std::result::Result<PathBuf, crate::config_rejection::ConfigRejection>>,
    work: Box<Work>,
    catalog: Box<Catalog>,
    state: Mutex<State>,
    cancel: Arc<MetadataRefreshControl>,
    stopped: watch::Sender<bool>,
    changed: watch::Sender<()>,
    clock: Box<Clock>,
    #[cfg(test)]
    outcomes: Mutex<Option<tokio::sync::mpsc::UnboundedSender<TimerEvent>>>,
}

fn stopping() -> LocalError {
    LocalError::new(
        LocalErrorKind::Stopping,
        "backend is stopping; metadata refresh cancelled",
    )
}

async fn metadata_error(
    error: lattice_llm::MetadataError,
    auth: lattice_llm::auth::AuthStore,
) -> LocalError {
    tokio::task::spawn_blocking(move || {
        let kind = match &error {
            lattice_llm::MetadataError::Configuration(_) => LocalErrorKind::BadRequest,
            lattice_llm::MetadataError::Chatgpt(
                lattice_llm::LlmError::Credential { .. } | lattice_llm::LlmError::Config { .. },
            ) => LocalErrorKind::BadRequest,
            lattice_llm::MetadataError::Cancelled => LocalErrorKind::Stopping,
            lattice_llm::MetadataError::Io(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                LocalErrorKind::Permission
            }
            lattice_llm::MetadataError::Io(_) | lattice_llm::MetadataError::Worker(_) => {
                LocalErrorKind::Internal
            }
            _ => LocalErrorKind::Transport,
        };
        LocalError::new(
            kind,
            &crate::config_rejection::ConfigRejection::new(&error.to_string(), Some(&auth))
                .to_string(),
        )
    })
    .await
    .unwrap_or_else(|_| {
        LocalError::new(
            LocalErrorKind::Internal,
            "metadata error processing task failed",
        )
    })
}

impl MetadataRefresh {
    pub(super) fn new(
        gateway: std::result::Result<
            Arc<lattice_llm::RequestGateway>,
            crate::config_rejection::ConfigRejection,
        >,
        directory: std::result::Result<PathBuf, crate::config_rejection::ConfigRejection>,
    ) -> Arc<Self> {
        let inspect_gateway = gateway.clone();
        Self::with_sources(
            Some(directory),
            move |source, cancel| {
                let gateway = gateway.clone();
                Box::pin(async move {
                    let gateway = gateway.map_err(|error| {
                        LocalError::from(crate::session::ApiError::Configuration(error))
                    })?;
                    let result = match source {
                        Source::ModelsDev => gateway.refresh_metadata(cancel).await.map(|()| 0),
                        Source::Chatgpt => gateway.refresh_chatgpt_models(cancel).await,
                    };
                    match result {
                        Ok(count) => Ok(count),
                        Err(error) => Err(metadata_error(error, gateway.auth_store()).await),
                    }
                })
            },
            move || {
                let gateway = inspect_gateway.clone();
                Box::pin(async move {
                    let gateway = gateway.map_err(|error| {
                        LocalError::from(crate::session::ApiError::Configuration(error))
                    })?;
                    let reader = gateway.clone();
                    match tokio::task::spawn_blocking(move || reader.chatgpt_catalog_state()).await
                    {
                        Ok(Ok(state)) => Ok(state),
                        Ok(Err(error)) => Err(metadata_error(error, gateway.auth_store()).await),
                        Err(_) => Err(LocalError::new(
                            LocalErrorKind::Internal,
                            "model directory state task failed",
                        )),
                    }
                })
            },
        )
    }

    pub(super) fn with_sources(
        directory: Option<std::result::Result<PathBuf, crate::config_rejection::ConfigRejection>>,
        work: impl Fn(Source, Arc<MetadataRefreshControl>) -> BoxFuture<'static, Result<usize>>
        + Send
        + Sync
        + 'static,
        catalog: impl Fn() -> BoxFuture<'static, Result<ChatgptCatalogState>> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            directory,
            work: Box::new(work),
            catalog: Box::new(catalog),
            state: Mutex::default(),
            cancel: Arc::default(),
            stopped: watch::channel(false).0,
            changed: watch::channel(()).0,
            clock: Box::new(SystemTime::now),
            #[cfg(test)]
            outcomes: Mutex::new(None),
        })
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn with_work(
        directory: Option<std::result::Result<PathBuf, crate::config_rejection::ConfigRejection>>,
        work: impl Fn(Arc<MetadataRefreshControl>) -> BoxFuture<'static, Result<()>>
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Self::with_sources(
            directory,
            move |_, cancel| {
                let result = work(cancel);
                Box::pin(async move { result.await.map(|()| 0) })
            },
            || {
                Box::pin(async {
                    Ok(ChatgptCatalogState {
                        logged_in: false,
                        fetched_at: None,
                        model_count: None,
                    })
                })
            },
        )
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(super) fn without_network() -> Arc<Self> {
        Self::with_work(None, |_| Box::pin(async { Ok(()) }))
    }

    async fn enabled(&self) -> Result<bool> {
        let Some(directory) = self.directory.clone() else {
            return Ok(true);
        };
        tokio::task::spawn_blocking(move || {
            let directory = directory.map_err(|error| {
                LocalError::from(crate::session::ApiError::Configuration(error))
            })?;
            let auth = lattice_llm::auth::AuthStore::new(directory.clone());
            lattice_llm::metadata_auto_refresh_enabled(&directory).map_err(|error| {
                LocalError::new(
                    LocalErrorKind::BadRequest,
                    &crate::config_rejection::ConfigRejection::new(&error.to_string(), Some(&auth))
                        .to_string(),
                )
            })
        })
        .await
        .unwrap_or_else(|_| {
            Err(LocalError::new(
                LocalErrorKind::Internal,
                "metadata configuration task failed",
            ))
        })
    }

    async fn deadlines(&self) -> [Option<Instant>; 2] {
        let checked = Instant::now();
        let now = (self.clock)();
        let directory = self.directory.clone();
        let models_freshness =
            tokio::task::spawn_blocking(move || cache_freshness(directory?.ok()?.as_path(), now))
                .await
                .ok()
                .flatten();
        let catalog = (self.catalog)().await;
        let state = self.state.lock().expect("metadata refresh lock");
        let due = |source: Source, remaining: Option<Duration>| {
            state.sources[source.index()]
                .next_attempt
                .unwrap_or(checked + remaining.unwrap_or_default())
        };
        [
            Some(due(Source::ModelsDev, models_freshness)),
            match catalog {
                Ok(catalog) if !catalog.logged_in => None,
                Ok(catalog) => Some(due(
                    Source::Chatgpt,
                    catalog.fetched_at.and_then(|time| freshness(time, now)),
                )),
                Err(_) => Some(due(Source::Chatgpt, None)),
            },
        ]
    }

    async fn automatic_refresh(self: &Arc<Self>) -> AutoRefresh {
        match self.enabled().await {
            Ok(true) => {
                let mut state = self.state.lock().expect("metadata refresh lock");
                state.configuration_failures = 0;
                state.configuration_next = None;
            }
            Ok(false) => {
                tracing::info!("automatic model metadata refresh disabled");
                let mut state = self.state.lock().expect("metadata refresh lock");
                state.configuration_failures = 0;
                state.configuration_next = Some(Instant::now() + REFRESH_INTERVAL);
                return AutoRefresh::Idle;
            }
            Err(error) => {
                tracing::warn!(%error, "automatic model metadata refresh failed: invalid configuration");
                let mut state = self.state.lock().expect("metadata refresh lock");
                state.configuration_failures = state.configuration_failures.saturating_add(1);
                state.configuration_next =
                    Some(Instant::now() + backoff(state.configuration_failures));
                return AutoRefresh::Failed;
            }
        }
        let deadlines = self.deadlines().await;
        let mut ran = false;
        let mut failed = false;
        // 两个来源都必须完成；一个失败不阻止另一个发布。
        let results =
            futures::future::join_all([Source::ModelsDev, Source::Chatgpt].into_iter().map(
                |source| async move {
                    if deadlines[source.index()].is_some_and(|deadline| deadline <= Instant::now())
                    {
                        Some(self.refresh_source(source, false).await)
                    } else {
                        None
                    }
                },
            ))
            .await;
        for result in results.into_iter().flatten() {
            ran = true;
            match result {
                Err(error) if error.kind == LocalErrorKind::Stopping => {
                    return AutoRefresh::Stopped;
                }
                Err(_) => failed = true,
                Ok(_) => {}
            }
        }
        if failed {
            AutoRefresh::Failed
        } else if ran {
            AutoRefresh::Ran
        } else {
            AutoRefresh::Fresh
        }
    }

    #[cfg(test)]
    fn report(&self, event: TimerEvent) {
        if let Some(outcomes) = &*self.outcomes.lock().expect("metadata outcomes lock") {
            let _ = outcomes.send(event);
        }
    }

    pub(super) fn start(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let refresh = self.clone();
        tokio::spawn(async move {
            let mut stopped = refresh.stopped.subscribe();
            let mut changed = refresh.changed.subscribe();
            loop {
                // 先承接已经完成的工作通知，再读取它们更新后的下次时间。
                changed.borrow_and_update();
                if *stopped.borrow() {
                    break;
                }
                let deadline = refresh
                    .state
                    .lock()
                    .expect("metadata refresh lock")
                    .configuration_next;
                let deadline = match deadline {
                    Some(deadline) => deadline,
                    None => refresh
                        .deadlines()
                        .await
                        .into_iter()
                        .flatten()
                        .min()
                        .unwrap_or_else(|| Instant::now() + REFRESH_INTERVAL),
                };
                let sleep = tokio::time::sleep_until(deadline);
                #[cfg(test)]
                refresh.report(TimerEvent::Armed(sleep.deadline()));
                tokio::select! {
                    biased;
                    _ = stopped.wait_for(|stopped| *stopped) => break,
                    _ = changed.changed() => continue,
                    _ = sleep => {},
                }
                if *stopped.borrow() {
                    break;
                }
                let outcome = refresh.automatic_refresh().await;
                #[cfg(test)]
                refresh.report(TimerEvent::Fired(outcome));
                if outcome == AutoRefresh::Stopped {
                    break;
                }
            }
        })
    }

    pub(super) fn stop(&self) {
        let _state = self.state.lock().expect("metadata refresh lock");
        self.cancel.cancel();
        self.stopped.send_replace(true);
    }

    pub(super) async fn drain(&self) {
        let completions: Vec<_> = self
            .state
            .lock()
            .expect("metadata refresh lock")
            .sources
            .iter()
            .filter_map(|source| source.flight.clone())
            .collect();
        for completion in completions {
            let _ = wait(completion).await;
        }
        self.cancel.drain().await;
    }

    pub(super) async fn refresh(self: &Arc<Self>) -> Result<()> {
        let (models, chatgpt) =
            tokio::join!(self.refresh_source(Source::ModelsDev, false), async {
                let state = (self.catalog)().await?;
                if state.logged_in {
                    self.refresh_source(Source::Chatgpt, false).await.map(Some)
                } else {
                    Ok(None)
                }
            });
        let mut errors = Vec::new();
        let mut updated = Vec::new();
        let mut kind = LocalErrorKind::Transport;
        for (source, result) in [
            (Source::ModelsDev, models.map(Some)),
            (Source::Chatgpt, chatgpt),
        ] {
            match result {
                Ok(Some(_)) => updated.push(source.name()),
                Ok(None) => {}
                Err(error) => {
                    kind = error.kind;
                    errors.push(format!("{}: {}", source.name(), error.message));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            let mut message = errors.join("; ");
            if !updated.is_empty() {
                message.push_str(&format!("; updated: {}", updated.join(", ")));
            }
            Err(LocalError::new(kind, &message))
        }
    }

    pub(super) async fn refresh_chatgpt(self: &Arc<Self>) -> Result<usize> {
        self.refresh_source(Source::Chatgpt, false).await
    }

    pub(super) async fn ensure_chatgpt(self: &Arc<Self>) -> Result<bool> {
        let catalog = (self.catalog)().await?;
        if !catalog.logged_in || catalog.model_count.is_some() {
            return Ok(false);
        }
        self.refresh_source(Source::Chatgpt, true).await?;
        Ok(true)
    }

    async fn refresh_source(self: &Arc<Self>, source: Source, only_missing: bool) -> Result<usize> {
        let completion = {
            let mut state = self.state.lock().expect("metadata refresh lock");
            if self.cancel.is_cancelled() {
                return Err(stopping());
            }
            let state = &mut state.sources[source.index()];
            if let Some(completion) = &state.flight {
                completion.clone()
            } else {
                let (done, completion) = watch::channel(None);
                state.flight = Some(completion.clone());
                let refresh = self.clone();
                tokio::spawn(async move {
                    let mut stopped = refresh.stopped.subscribe();
                    let work = std::panic::AssertUnwindSafe(async {
                        if only_missing {
                            let catalog = (refresh.catalog)().await?;
                            if let Some(count) = catalog.model_count {
                                return Ok(count);
                            }
                            if !catalog.logged_in {
                                return Err(LocalError::new(
                                    LocalErrorKind::BadRequest,
                                    "ChatGPT is not signed in",
                                ));
                            }
                        }
                        (refresh.work)(source, refresh.cancel.clone()).await
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
                        tracing::warn!(%error, source = source.name(), "model metadata refresh failed");
                    }
                    let mut state = refresh.state.lock().expect("metadata refresh lock");
                    let state = &mut state.sources[source.index()];
                    state.flight = None;
                    let delay = match &result {
                        Ok(_) => {
                            state.failures = 0;
                            REFRESH_INTERVAL
                        }
                        Err(_) => {
                            state.failures = state.failures.saturating_add(1);
                            backoff(state.failures)
                        }
                    };
                    state.next_attempt = Some(Instant::now() + delay);
                    done.send_replace(Some(result));
                    refresh.changed.send_replace(());
                });
                completion
            }
        };
        wait(completion).await
    }
}

async fn wait(mut completion: Completion) -> Result<usize> {
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

#[cfg(test)]
mod tests;
