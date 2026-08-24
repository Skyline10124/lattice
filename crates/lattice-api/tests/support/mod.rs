// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
#![allow(dead_code)]

//! Shared helpers for lattice-api integration tests.
//!
//! #59 (Codeberg #37) removed the Upper workspace override surface; these
//! helpers now only provide the git-repo bootstrap used by Direct-Local test
//! fixtures.
use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lancet_audit::invocation_store::InvocationStore;
use lancet_protocol::{
    BindingIdentity, CepError, InvocationBinding, InvocationId, PlanKey, SessionId, TurnId,
    arguments_digest,
};
use lattice_session::test_utils::init_fixture_repo;

pub struct ReservedRuntime {
    runtime: lancet_api::LancetRuntime,
    client: ReservedClient,
    _state: tempfile::TempDir,
}

impl ReservedRuntime {
    pub fn build(
        mut config: lancet_api::LancetRuntimeConfig,
    ) -> Result<Self, lancet_api::LancetError> {
        let state = tempfile::TempDir::new()
            .map_err(|err| lancet_api::LancetError::Internal(err.to_string()))?;
        let backing = lancet_api::WorkspaceHistoryBacking::for_project_in(
            state.path(),
            &config.policy.project_root,
        )
        .map_err(|err| lancet_api::LancetError::Internal(err.to_string()))?;
        let (layout, key) = match &backing {
            lancet_api::WorkspaceHistoryBacking::Project { layout, key } => (layout.clone(), *key),
            lancet_api::WorkspaceHistoryBacking::Disabled => unreachable!(),
        };
        InvocationStore::create(&layout, &key)
            .map_err(|err| lancet_api::LancetError::Internal(err.to_string()))?;
        config.invocations = backing;
        let runtime = lancet_api::LancetRuntime::build(config)?;
        let descriptors = runtime
            .tool_definitions()
            .iter()
            .map(|tool| (tool.name.clone(), tool.descriptor_identity.clone()))
            .collect();
        let client = ReservedClient {
            inner: runtime.client(),
            invocations: Arc::new(
                InvocationStore::open(&layout, &key)
                    .map_err(|err| lancet_api::LancetError::Internal(err.to_string()))?,
            ),
            descriptors: Arc::new(descriptors),
            next: Arc::new(AtomicU32::new(0)),
        };
        Ok(Self {
            runtime,
            client,
            _state: state,
        })
    }

    pub fn client(&self) -> ReservedClient {
        self.client.clone()
    }
}

impl Deref for ReservedRuntime {
    type Target = lancet_api::LancetRuntime;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

#[derive(Clone)]
pub struct ReservedClient {
    inner: lancet_api::CepClient,
    invocations: Arc<InvocationStore>,
    descriptors: Arc<BTreeMap<String, String>>,
    next: Arc<AtomicU32>,
}

impl ReservedClient {
    fn binding(&self, method: &str, params: &serde_json::Value) -> InvocationBinding {
        let ordinal = self.next.fetch_add(1, Ordering::Relaxed);
        let turn_id = TurnId::parse("22222222-2222-4222-8222-222222222222").unwrap();
        let binding = InvocationBinding {
            invocation_id: InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}",))
                .unwrap(),
            identity: BindingIdentity {
                session_id: SessionId::parse("11111111111111111111111111111111").unwrap(),
                turn_id,
                model_step_ordinal: ordinal,
                part_ordinal: 0,
                method: method.to_string(),
                descriptor_identity: self
                    .descriptors
                    .get(method)
                    .cloned()
                    .unwrap_or_else(|| "0".repeat(64)),
                arguments_digest: arguments_digest(params),
            },
        };
        self.invocations
            .reserve_batch(
                PlanKey {
                    turn_id,
                    model_step_ordinal: ordinal,
                },
                std::slice::from_ref(&binding),
            )
            .expect("reserve integration-test invocation");
        binding
    }

    pub async fn call_receipt(
        &self,
        method: impl Into<String>,
        params: serde_json::Value,
    ) -> Result<lancet_api::ExecutionReceipt, CepError> {
        let method = method.into();
        self.inner
            .call_raw_with_receipt(self.binding(&method, &params), method, params)
            .await
    }

    pub async fn call_raw(
        &self,
        method: impl Into<String>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CepError> {
        self.call_receipt(method, params).await?.result
    }

    pub fn start_call_raw(
        &self,
        method: String,
        params: serde_json::Value,
    ) -> (
        impl std::future::Future<Output = Result<serde_json::Value, CepError>> + Send + 'static,
        lancet_api::CallControl,
    ) {
        self.inner
            .start_call_raw(self.binding(&method, &params), method, params)
    }
}
pub fn init_git_repo(root: &Path) {
    // Capture any pre-written project files into the initial commit so HEAD is
    // born and the worktree baseline matches the fixture.
    let mut files: Vec<(String, String)> = Vec::new();
    collect_files(root, root, &mut files);
    let owned: Vec<(String, String)> = files;
    let refs: Vec<(&str, &str)> = owned
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let _ = init_fixture_repo(root, &refs);
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, out);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .into_owned();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            out.push((rel, content));
        }
    }
}
