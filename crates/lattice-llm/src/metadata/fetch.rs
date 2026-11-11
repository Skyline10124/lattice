// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use super::{Catalog, snapshot_from_json};
use crate::provider::config::ProviderRegistry;

pub(super) const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// 取消与最终改名共用一把锁；取消返回后不再开始新的缓存发布。
#[derive(Default)]
pub struct MetadataRefreshControl {
    cancelled: Mutex<bool>,
    workers: AtomicUsize,
    changed: tokio::sync::Notify,
}

struct PublicationWorker(Arc<MetadataRefreshControl>);

impl Drop for PublicationWorker {
    fn drop(&mut self) {
        self.0.workers.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }
}

impl MetadataRefreshControl {
    pub fn cancel(&self) {
        *self.cancelled.lock().expect("metadata publication lock") = true;
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.lock().expect("metadata publication lock")
    }

    fn worker(self: &Arc<Self>) -> Result<PublicationWorker, MetadataError> {
        let cancelled = self.cancelled.lock().expect("metadata publication lock");
        if *cancelled {
            return Err(MetadataError::Cancelled);
        }
        self.workers.fetch_add(1, Ordering::SeqCst);
        Ok(PublicationWorker(self.clone()))
    }

    /// 网络 future 可以立即取消，已经交给 blocking 线程的文件工作仍须排空。
    pub async fn drain(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.workers.load(Ordering::SeqCst) == 0 {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("{0}")]
    Configuration(String),
    #[error("ChatGPT model directory: {0}")]
    Chatgpt(#[from] crate::LlmError),
    #[error("invalid ChatGPT model directory: {0}")]
    InvalidCatalog(&'static str),
    #[error("metadata refresh was cancelled")]
    Cancelled,
    #[error("metadata I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid model metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("metadata network policy rejected the URL: {0}")]
    Policy(#[from] net_guard::NetGuardError),
    #[error("metadata requires an HTTPS URL")]
    HttpsRequired,
    #[error("metadata HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("metadata server returned HTTP {0}")]
    Status(reqwest::StatusCode),
    #[error("metadata response exceeds 32 MiB")]
    TooLarge,
    #[error("metadata fetch did not finish within 30 seconds")]
    Timeout,
    #[error("metadata worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
}

fn client(url: &url::Url, roots_der: &[Vec<u8>]) -> Result<reqwest::Client, MetadataError> {
    if url.scheme() != "https" {
        return Err(MetadataError::HttpsRequired);
    }
    net_guard::validate_base_url_static(url.as_str())?;
    let roots = roots_der
        .iter()
        .map(|der| reqwest::Certificate::from_der(der))
        .collect::<Result<Vec<_>, _>>()?;
    let builder = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .tls_certs_only(roots)
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(FETCH_TIMEOUT)
        .read_timeout(FETCH_TIMEOUT)
        .user_agent(crate::transport::user_agent(None));
    Ok(crate::client::pin_http_client(url.as_str(), builder)?.build()?)
}

/// 拉取完整响应后按快照白名单过滤；TLS 根完全由调用方提供。
pub async fn fetch_snapshot(url: &str, roots_der: &[Vec<u8>]) -> Result<Vec<u8>, MetadataError> {
    let source = fetch_source(url, roots_der).await?;
    snapshot_from_json(&source)
}

async fn fetch_source(url: &str, roots_der: &[Vec<u8>]) -> Result<Vec<u8>, MetadataError> {
    fetch_authorized_source(url, roots_der, None).await
}

pub(super) async fn fetch_authorized_source(
    url: &str,
    roots_der: &[Vec<u8>],
    authorization: Option<reqwest::header::HeaderValue>,
) -> Result<Vec<u8>, MetadataError> {
    let url = url::Url::parse(url).map_err(|error| net_guard::NetGuardError::Config {
        message: format!("invalid metadata URL: {error}"),
    })?;
    tokio::time::timeout(FETCH_TIMEOUT, async {
        let worker_url = url.clone();
        let roots_der = roots_der.to_vec();
        let client = tokio::task::spawn_blocking(move || client(&worker_url, &roots_der)).await??;
        let mut request = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(authorization) = authorization {
            request = request.header(reqwest::header::AUTHORIZATION, authorization);
        }
        let mut response = request.send().await?;
        if !response.status().is_success() {
            return Err(MetadataError::Status(response.status()));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(MetadataError::TooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                return Err(MetadataError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| MetadataError::Timeout)?
}

/// 按内置表与当前注册表的 catalog 过滤；同目录写完临时文件后替换，失败保留旧缓存。
pub(crate) async fn refresh_cancellable(
    config_dir: &Path,
    registry: &ProviderRegistry,
    url: &str,
    roots_der: &[Vec<u8>],
    cancel: Arc<MetadataRefreshControl>,
) -> Result<(), MetadataError> {
    let source = fetch_source(url, roots_der).await?;
    let snapshot = Catalog::filtered(&source, registry)?.to_json()?;
    publish_cancellable(config_dir, "models.json", snapshot, cancel, persist).await
}

pub(super) fn persist(
    temporary: tempfile::NamedTempFile,
    destination: &Path,
) -> Result<(), MetadataError> {
    temporary
        .persist(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

/// 两种目录共用写入与取消门闩；发布者可以在最终替换时核对凭据生命周期。
pub(super) async fn publish_cancellable(
    config_dir: &Path,
    filename: &str,
    snapshot: Vec<u8>,
    cancel: Arc<MetadataRefreshControl>,
    publish: impl FnOnce(tempfile::NamedTempFile, &Path) -> Result<(), MetadataError> + Send + 'static,
) -> Result<(), MetadataError> {
    let config_dir = config_dir.to_owned();
    let filename = filename.to_owned();
    let cache_dir = config_dir.join("cache");
    let worker = cancel.worker()?;
    tokio::task::spawn_blocking(move || {
        let _worker = worker;
        if cancel.is_cancelled() {
            return Err(MetadataError::Cancelled);
        }
        crate::auth::ensure_configuration_directory(&config_dir)
            .map_err(|error| MetadataError::Configuration(error.to_string()))?;
        std::fs::create_dir_all(&cache_dir)?;
        let temporary = tempfile::NamedTempFile::new_in(&cache_dir)?;
        write_and_publish_cancellable(
            temporary,
            &cache_dir.join(filename),
            &snapshot,
            &cancel,
            publish,
            #[cfg(test)]
            None,
        )
    })
    .await??;
    Ok(())
}

#[cfg(test)]
fn write_and_replace_cancellable(
    temporary: tempfile::NamedTempFile,
    destination: &Path,
    snapshot: &[u8],
    cancel: &MetadataRefreshControl,
    before_rename: Option<&dyn Fn()>,
) -> Result<(), MetadataError> {
    write_and_publish_cancellable(
        temporary,
        destination,
        snapshot,
        cancel,
        persist,
        before_rename,
    )
}

fn write_and_publish_cancellable(
    mut temporary: tempfile::NamedTempFile,
    destination: &Path,
    snapshot: &[u8],
    cancel: &MetadataRefreshControl,
    publish: impl FnOnce(tempfile::NamedTempFile, &Path) -> Result<(), MetadataError>,
    #[cfg(test)] before_rename: Option<&dyn Fn()>,
) -> Result<(), MetadataError> {
    temporary.write_all(snapshot)?;
    temporary.as_file().sync_all()?;
    let cancelled = cancel.cancelled.lock().expect("metadata publication lock");
    if *cancelled {
        return Err(MetadataError::Cancelled);
    }
    #[cfg(test)]
    if let Some(before_rename) = before_rename {
        before_rename();
    }
    publish(temporary, destination)
}

#[cfg(test)]
mod tests;
