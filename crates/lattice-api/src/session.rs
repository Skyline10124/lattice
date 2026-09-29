// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 当前代会话库的短期门面；不自行打开、创建或替换数据库。
use lancet_protocol::SessionId;
use lattice_session::store::{SessionRead, SessionRecord, SessionStore, StoreError};
use std::sync::Arc;

#[derive(Clone, Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
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
mod redb_tests {
    use super::*;
    use lattice_session::store::*;
    #[tokio::test]
    async fn passed_store_prefix_resolution_and_safe_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionStore::create(&lattice_session::StateLayout::in_state_root(tmp.path())).unwrap(),
        );
        let id = lancet_protocol::SessionId::parse("aaaa0000000000000000000000000001").unwrap();
        let lock = store.try_lock_session(id).unwrap();
        let input = AcceptTurn {
            session: AcceptSession::New { id },
            turn_id: lancet_protocol::TurnId::parse("00000000-0000-4000-8000-000000000001")
                .unwrap(),
            project_store_key: lattice_session::ProjectStoreKey::from_canonical_path_bytes(
                b"/session-api",
            ),
            input: "test".into(),
            accepted_at: chrono::Utc::now(),
            agent_selection: AgentSelection::Explicit,
            config: FrozenTurnConfig {
                agent_name: "alpha".into(),
                model: "model".into(),
                provider: "openai".into(),
                system_prompt: None,
                profile_snapshot_json: "{}".into(),
                policy_snapshot_json: "{}".into(),
                catalog: ToolCatalogSnapshot::new("test".into(), Vec::new()).unwrap(),
                limits: TurnExecutionLimits {
                    timeout_secs: 300,
                    max_model_steps: 32,
                    max_model_attempts: 1,
                },
            },
        };
        store.accept_turn(&lock, input.clone()).unwrap();
        drop(lock);
        let api = SessionApi::from_store(store.clone());
        assert_eq!(
            api.resolve_session(id.to_string())
                .await
                .unwrap()
                .session_id,
            id
        );
        assert_eq!(
            api.resolve_session("aaaa".into()).await.unwrap().session_id,
            id
        );
        for invalid in ["", "   ", "aaa"] {
            assert!(matches!(
                api.resolve_session(invalid.into()).await,
                Err(ApiError::BadRequest(_))
            ));
        }
        assert!(matches!(
            api.resolve_session("bbbb".into()).await,
            Err(ApiError::NotFound(_))
        ));
        let log = api.read(move |read| read.log(id)).await.unwrap();
        assert_eq!(log.nodes.len(), 2);
        assert_eq!(log.session.current, NodeId(1));
        assert_eq!(log.current_agent.as_deref(), Some("alpha"));
        for n in 2..=7 {
            let mut next = input.clone();
            let sid = lancet_protocol::SessionId::parse(&format!("aaaa{n:028x}")).unwrap();
            next.session = AcceptSession::New { id: sid };
            next.turn_id =
                lancet_protocol::TurnId::parse(&format!("00000000-0000-4000-8000-{n:012x}"))
                    .unwrap();
            store
                .accept_turn(&store.try_lock_session(sid).unwrap(), next)
                .unwrap();
        }
        assert_eq!(
            api.resolve_session(id.to_string())
                .await
                .unwrap()
                .session_id,
            id
        );
        let Err(ApiError::BadRequest(error)) = api.resolve_session("aaaa".into()).await else {
            panic!("ambiguous prefix accepted")
        };
        assert_eq!(error.matches("aaaa").count(), 5);
        assert!(error.contains("use more characters"));
        let rows = api.read(|read| read.list(None, 100)).await.unwrap();
        assert_eq!(rows.items.len(), 7);
        assert!(rows.items.iter().all(|row| row.node_count == 2));
    }
}
