// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap, future::Future, io::Read, path::Path, sync::Arc, time::SystemTime,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::{MetadataError, MetadataRefreshControl, Object, fetch};
use crate::auth::ChatgptAuth;

const MODELS_URL: &str = "https://api.openai.com/v1/models";
pub(super) const CACHE_FILE: &str = "chatgpt-models.json";

/// 只有本地目录状态跨过网关，账号材料与 token 留在凭据模块。
#[derive(Clone, Copy, Debug)]
pub struct ChatgptCatalogState {
    pub logged_in: bool,
    pub fetched_at: Option<SystemTime>,
    pub model_count: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountModel {
    pub(crate) slug: String,
    pub(crate) display_name: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    fetched_at: String,
    #[serde(deserialize_with = "model_objects")]
    models: Vec<AccountModel>,
}

fn model_objects<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<AccountModel>, D::Error> {
    Vec::<Object<AccountModel>>::deserialize(deserializer)
        .map(|models| models.into_iter().map(|model| model.0).collect())
}

pub(crate) struct AccountCatalog {
    pub(crate) fetched_at: SystemTime,
    pub(crate) models: BTreeMap<String, String>,
}

fn invalid(reason: &'static str) -> MetadataError {
    MetadataError::InvalidCatalog(reason)
}

fn validate(models: &[AccountModel]) -> Result<BTreeMap<String, String>, MetadataError> {
    let mut result = BTreeMap::new();
    for model in models {
        lattice_conversation::ModelId::parse(&model.slug)
            .map_err(|_| invalid("invalid model ID"))?;
        if model.display_name.is_empty() || model.display_name.chars().any(char::is_control) {
            return Err(invalid("invalid display name"));
        }
        if result
            .insert(model.slug.clone(), model.display_name.clone())
            .is_some()
        {
            return Err(invalid("duplicate model ID"));
        }
    }
    Ok(result)
}

pub(super) fn parse_response(source: &[u8]) -> Result<Vec<AccountModel>, MetadataError> {
    let source: serde_json::Value =
        serde_json::from_slice(source).map_err(|_| invalid("invalid JSON"))?;
    let models = source
        .as_object()
        .and_then(|object| object.get("models"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid("models must be an array"))?;
    let mut listed = Vec::new();
    for value in models {
        let model = value
            .as_object()
            .ok_or_else(|| invalid("model must be an object"))?;
        let visibility = model
            .get("visibility")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid("visibility must be a string"))?;
        if visibility != "list" {
            continue;
        }
        let field = |name| {
            model
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| invalid("listed model requires slug and display_name"))
        };
        listed.push(AccountModel {
            slug: field("slug")?,
            display_name: field("display_name")?,
        });
    }
    validate(&listed)?;
    Ok(listed)
}

pub(crate) fn load(directory: &Path) -> Result<AccountCatalog, MetadataError> {
    let file = std::fs::File::open(directory.join("cache").join(CACHE_FILE))?;
    let mut bytes = Vec::new();
    file.take(fetch::MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > fetch::MAX_RESPONSE_BYTES {
        return Err(MetadataError::TooLarge);
    }
    let Object(document): Object<Document> =
        serde_json::from_slice(&bytes).map_err(|_| invalid("invalid cache structure"))?;
    if document.version != 1 {
        return Err(invalid("unsupported cache version"));
    }
    let fetched_at = chrono::DateTime::parse_from_rfc3339(&document.fetched_at)
        .map_err(|_| invalid("invalid fetched_at"))?;
    Ok(AccountCatalog {
        fetched_at: fetched_at.into(),
        models: validate(&document.models)?,
    })
}

pub(crate) fn state(directory: &Path) -> Result<ChatgptCatalogState, MetadataError> {
    let logged_in = crate::auth::AuthStore::new(directory.into())
        .load()
        .map_err(|error| MetadataError::Configuration(error.to_string()))?
        .chatgpt_logged_in();
    let catalog = load(directory).ok();
    Ok(ChatgptCatalogState {
        logged_in,
        fetched_at: catalog.as_ref().map(|catalog| catalog.fetched_at),
        model_count: catalog.as_ref().map(|catalog| catalog.models.len()),
    })
}

pub(crate) async fn refresh(
    directory: &Path,
    auth: &ChatgptAuth,
    roots_der: &[Vec<u8>],
    cancel: Arc<MetadataRefreshControl>,
) -> Result<usize, MetadataError> {
    refresh_with(directory, auth, cancel, |authorization| async move {
        let bytes =
            fetch::fetch_authorized_source(MODELS_URL, roots_der, Some(authorization)).await?;
        parse_response(&bytes)
    })
    .await
}

/// 远端依赖只返回筛选后的目录；本地测试不模拟 OpenAI HTTP 服务。
pub(crate) async fn refresh_with<F, Fut>(
    directory: &Path,
    auth: &ChatgptAuth,
    cancel: Arc<MetadataRefreshControl>,
    fetch: F,
) -> Result<usize, MetadataError>
where
    F: FnOnce(reqwest::header::HeaderValue) -> Fut,
    Fut: Future<Output = Result<Vec<AccountModel>, MetadataError>>,
{
    if cancel.is_cancelled() {
        return Err(MetadataError::Cancelled);
    }
    let token = auth.access_token().await?;
    let models = fetch(token.authorization()?).await?;
    validate(&models)?;
    let model_count = models.len();
    let document = Document {
        version: 1,
        fetched_at: Utc::now().to_rfc3339(),
        models,
    };
    let mut snapshot = serde_json::to_vec_pretty(&document)?;
    snapshot.push(b'\n');
    if snapshot.len() > fetch::MAX_RESPONSE_BYTES {
        return Err(MetadataError::TooLarge);
    }
    let auth = auth.clone();
    fetch::publish_cancellable(
        directory,
        CACHE_FILE,
        snapshot,
        cancel,
        move |temporary, destination| {
            auth.with_current_token(&token, || fetch::persist(temporary, destination))?
        },
    )
    .await?;
    Ok(model_count)
}

#[cfg(test)]
mod tests;
