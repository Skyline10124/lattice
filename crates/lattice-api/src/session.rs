// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 当前代会话库的短期门面；不自行打开、创建或替换数据库。
use lancet_protocol::SessionId;
use lattice_session::store::{SessionRead, SessionRecord, SessionStore, StoreError};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestConflict {
    TurnIdUsed,
    SessionExists,
    BindingMismatch,
}

impl std::fmt::Display for RequestConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TurnIdUsed => "TurnId is already in use",
            Self::SessionExists => "session already exists",
            Self::BindingMismatch => "session is bound to another workspace",
        })
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("{0}")]
    Configuration(crate::ConfigRejection),
    #[error("conflict: {0}")]
    Conflict(RequestConflict),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("busy: {0}")]
    Busy(String),
    #[error("store unavailable: {0}")]
    StoreUnavailable(String),
    #[error("session store: {0}")]
    Store(Arc<StoreError>),
    #[error("store worker failed; result unknown")]
    WorkerFailed,
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        Self::Store(Arc::new(error))
    }
}
impl From<crate::turn::coordinator::CoordinatorIoError> for ApiError {
    fn from(error: crate::turn::coordinator::CoordinatorIoError) -> Self {
        match error {
            crate::turn::coordinator::CoordinatorIoError::Store(error) => error.into(),
            crate::turn::coordinator::CoordinatorIoError::Worker(_) => Self::WorkerFailed,
        }
    }
}
impl ApiError {
    pub(crate) fn requires_reopen(&self) -> bool {
        matches!(self, Self::WorkerFailed)
            || matches!(self, Self::Store(error) if matches!(error.as_ref(),
            StoreError::CommitFailed { .. } | StoreError::WriteDisabled | StoreError::CreationDurabilityUnknown { .. }))
    }
}
pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Clone)]
pub struct SessionApi {
    pub(crate) store: Arc<SessionStore>,
    // store 必须先于租约释放，恢复取得排他租约时最后一个旧数据库句柄已退场。
    lease: Option<Arc<crate::stores::StoreLease>>,
}

impl SessionApi {
    #[cfg(any(test, feature = "test-utils"))]
    pub fn from_store(store: Arc<SessionStore>) -> Self {
        Self { store, lease: None }
    }

    pub(crate) fn leased(store: Arc<SessionStore>, lease: Arc<crate::stores::StoreLease>) -> Self {
        Self {
            store,
            lease: Some(lease),
        }
    }

    pub(crate) fn shutdown(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.lease
            .as_ref()
            .map_or_else(Arc::default, |lease| lease.shutdown.clone())
    }

    pub(crate) fn report_failure(&self, error: &ApiError) {
        if error.requires_reopen()
            && let Some(lease) = &self.lease
            && let Some(manager) = lease.manager.upgrade()
        {
            manager.fail(lease.generation);
        }
    }

    pub(crate) fn with_execution_start<T>(
        &self,
        start: impl FnOnce() -> T,
    ) -> lattice_session::store::Result<T> {
        self.store.with_execution_start(|| {
            if self
                .lease
                .as_ref()
                .is_some_and(|lease| lease.shutdown.load(std::sync::atomic::Ordering::Acquire))
            {
                Err(StoreError::WriteDisabled)
            } else {
                Ok(start())
            }
        })?
    }

    pub(crate) async fn blocking<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let api = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&api)))
                .unwrap_or(Err(ApiError::WorkerFailed));
            if let Err(error) = &result {
                api.report_failure(error);
            }
            result
        })
        .await
        .map_err(|_| ApiError::WorkerFailed)
        .and_then(|result| result);
        if let Err(error) = &result {
            self.report_failure(error);
        }
        result
    }

    /// 整个读事务在阻塞线程内完成，只返回已物化数据。
    pub async fn read<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&SessionRead) -> lattice_session::store::Result<T> + Send + 'static,
    ) -> Result<T> {
        self.blocking(move |api| Ok(operation(&api.store.begin_read()?)?))
            .await
    }

    pub async fn resolve_session(&self, prefix: String) -> Result<SessionRecord> {
        self.blocking(move |api| resolve_session(&api.store.begin_read()?, &prefix))
            .await
    }
}

/// 精确身份优先；同一读事务分页查全，歧义诊断最多五项。
fn resolve_session(read: &SessionRead, prefix: &str) -> Result<SessionRecord> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Err(ApiError::BadRequest("session ID must not be empty".into()));
    }
    if let Ok(id) = SessionId::parse(prefix) {
        match read.session(id) {
            Ok(record) => return Ok(record),
            Err(StoreError::NotFound { .. }) => {}
            Err(error) => return Err(error.into()),
        }
    }
    if prefix.len() < 4 {
        return Err(ApiError::BadRequest(
            "session prefix requires at least four characters".into(),
        ));
    }
    let mut after = None;
    let mut matches = Vec::new();
    let mut count = 0;
    loop {
        let page = read.list(after, 256)?;
        for row in page.items {
            if row.session_id.to_string().starts_with(prefix) {
                count += 1;
                if matches.len() < 5 {
                    matches.push(row.session_id);
                }
            }
        }
        let Some(next) = page.next else {
            break;
        };
        after = Some(next);
    }
    match count {
        0 => Err(ApiError::NotFound(prefix.into())),
        1 => Ok(read.session(matches[0])?),
        _ => Err(ApiError::BadRequest(format!(
            "ambiguous session prefix: {}; use more characters",
            matches
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_conflicts_have_readable_english_messages() {
        for (reason, text) in [
            (RequestConflict::TurnIdUsed, "TurnId is already in use"),
            (RequestConflict::SessionExists, "session already exists"),
            (
                RequestConflict::BindingMismatch,
                "session is bound to another workspace",
            ),
        ] {
            assert_eq!(
                ApiError::Conflict(reason).to_string(),
                format!("conflict: {text}")
            );
        }
    }
}
