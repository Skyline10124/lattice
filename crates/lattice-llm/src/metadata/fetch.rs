// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{io::Write, path::Path, time::Duration};

use super::{Catalog, snapshot_from_json};
use crate::provider::config::ProviderRegistry;

const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
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
        .user_agent(crate::transport::user_agent(None));
    Ok(crate::client::pin_http_client(url.as_str(), builder)?.build()?)
}

/// 拉取完整响应后按快照白名单过滤；TLS 根完全由调用方提供。
pub async fn fetch_snapshot(url: &str, roots_der: &[Vec<u8>]) -> Result<Vec<u8>, MetadataError> {
    let source = fetch_source(url, roots_der).await?;
    snapshot_from_json(&source)
}

async fn fetch_source(url: &str, roots_der: &[Vec<u8>]) -> Result<Vec<u8>, MetadataError> {
    let url = url::Url::parse(url).map_err(|error| net_guard::NetGuardError::Config {
        message: format!("invalid metadata URL: {error}"),
    })?;
    tokio::time::timeout(FETCH_TIMEOUT, async {
        let worker_url = url.clone();
        let roots_der = roots_der.to_vec();
        let client = tokio::task::spawn_blocking(move || client(&worker_url, &roots_der)).await??;
        let mut response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "#315 L2 元数据刷新尚未接入后台")
)]
pub(crate) async fn refresh(
    config_dir: &Path,
    registry: &ProviderRegistry,
    url: &str,
    roots_der: &[Vec<u8>],
) -> Result<(), MetadataError> {
    let source = fetch_source(url, roots_der).await?;
    let snapshot = Catalog::filtered(&source, registry)?.to_json()?;
    let cache_dir = config_dir.join("cache");
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&cache_dir)?;
        let temporary = tempfile::NamedTempFile::new_in(&cache_dir)?;
        write_and_replace(temporary, &cache_dir.join("models.json"), &snapshot)
    })
    .await??;
    Ok(())
}

fn write_and_replace(
    mut temporary: tempfile::NamedTempFile,
    destination: &Path,
    snapshot: &[u8],
) -> std::io::Result<()> {
    temporary.write_all(snapshot)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests;
