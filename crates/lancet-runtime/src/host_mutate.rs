// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Tool Host Structured Workspace Mutation operations (#31 slice A).
//!
//! The guest holds no filesystem path and no write handle. Its only reach is
//! the three `workspace-mutate` imports — `read-for-edit`, `create-if-absent`,
//! and `replace-with-expected` — each of which re-enters this host with a
//! string the host re-resolves, authorizes against `mutate:write:workspace`,
//! and either installs atomically or refuses before any Local bytes change.
//!
//! Precondition enforcement and evidence recording are host-authoritative:
//! a guest that short-circuits on a stale revision never reaches these
//! imports, so Components MUST call them for every install attempt (see the
//! #31 plan review fix for Standards #2).

use std::sync::{Arc, Mutex};

use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::validate_path_facts;
use lancet_authority::under_system_exclusion;
use lancet_protocol::cell::{Cell, ChannelKind, CtxOp, ExtImpact};
use lancet_protocol::error::{
    CepError, CepErrorCode, CepErrorData, GuardErrorData, Retriability, ValidationErrorData,
};
use lancet_protocol::evidence::{EffectSource, ObservedTarget};

use crate::admission::{AdmittedTool, authorize_operation, resolve_scope};
use crate::evidence::CellEvidenceLedger;
use crate::history::{MutationRecord, RecordedMutation, WorkspaceHistory};
use crate::workspace::{
    LocalWorkspace, LocalWorkspaceError, PathIntent, WorkspacePath, content_hash,
};

/// Maximum bytes a single mutation may write to one file (#120).
const MAX_WRITE_SIZE: usize = 1024 * 1024;

/// Call-scoped inputs for one mutate import.
pub struct MutationCallContext<'a> {
    pub call_id: u64,
    pub turn_id: &'a str,
    /// CEP method name recorded into private history (`mutate.create` / `mutate.edit`).
    pub method: &'a str,
    pub admitted: &'a AdmittedTool,
    pub evidence: &'a CellEvidenceLedger,
}

/// Bytes + host-computed Local Revision returned by `read-for-edit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBytes {
    pub content: String,
    pub local_revision: String,
}

/// Successful `create-if-absent` install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateInstalled {
    pub path: String,
    pub bytes_written: u64,
    pub local_revision: String,
    pub created_dirs: Vec<String>,
    pub history: Option<RecordedMutation>,
}

/// Successful `replace-with-expected` install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaceInstalled {
    pub path: String,
    pub bytes_written: u64,
    pub local_revision: String,
    pub history: Option<RecordedMutation>,
}

/// Host-decided mutation failure. Guest-facing messages are coarse; audit
/// labels stay on the host side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MutateError {
    NotFound { requested: String },
    AlreadyExists { requested: String },
    StaleRevision {
        requested: String,
        expected: String,
        observed: String,
    },
    PathRejected { requested: String, reason: String },
    Denied { requested: String, reason: String },
    LimitExceeded {
        requested: String,
        limit: usize,
        actual: usize,
    },
    Cancelled,
    InvalidRequest { reason: String },
    Io { requested: String, detail: String },
}

impl MutateError {
    pub fn audit_label(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not-found",
            Self::AlreadyExists { .. } => "already-exists",
            Self::StaleRevision { .. } => "stale-revision",
            Self::PathRejected { .. } => "path-rejected",
            Self::Denied { .. } => "denied",
            Self::LimitExceeded { .. } => "limit-exceeded",
            Self::Cancelled => "cancelled",
            Self::InvalidRequest { .. } => "invalid-request",
            Self::Io { .. } => "io-error",
        }
    }

    pub fn guest_message(&self) -> String {
        match self {
            Self::NotFound { requested } => format!("path not found: {requested}"),
            Self::AlreadyExists { requested } => {
                format!(
                    "mutate target '{requested}' already exists; observe.workspace and replan"
                )
            }
            Self::StaleRevision {
                requested,
                expected,
                observed,
            } => format!(
                "expected_hash for '{requested}' is stale: expected {expected}, current {observed}; observe.workspace read_file again"
            ),
            Self::PathRejected { requested, .. } => format!("path rejected: {requested}"),
            Self::Denied { requested, .. } => format!("write denied: {requested}"),
            Self::LimitExceeded {
                requested,
                limit,
                actual,
            } => format!("write size limit exceeded for '{requested}': {actual} > {limit}"),
            Self::Cancelled => "cancelled".into(),
            Self::InvalidRequest { reason } => reason.clone(),
            Self::Io { requested, .. } => format!("i/o error at {requested}"),
        }
    }
}

/// Map a host mutate refusal to a structured CEP error.
pub fn mutate_cep_error(method: &str, err: &MutateError) -> CepError {
    match err {
        MutateError::AlreadyExists { .. } | MutateError::StaleRevision { .. } => CepError::new(
            CepErrorCode::ReplanRequired,
            err.guest_message(),
            CepErrorData::Guard(GuardErrorData {
                method: method.into(),
                reason: format!("ReplanRequired: {}", err.audit_label()),
            }),
            Retriability::RequiresReplan,
        ),
        MutateError::PathRejected { .. } => CepError::new(
            CepErrorCode::PathContainment,
            err.guest_message(),
            CepErrorData::Validation(ValidationErrorData {
                field: "path".into(),
                reason: err.audit_label().into(),
            }),
            Retriability::Permanent,
        ),
        MutateError::Denied { .. } => CepError::new(
            CepErrorCode::EffectViolation,
            err.guest_message(),
            CepErrorData::Validation(ValidationErrorData {
                field: "path".into(),
                reason: err.audit_label().into(),
            }),
            Retriability::Permanent,
        ),
        MutateError::LimitExceeded { limit, actual, .. } => CepError::new(
            CepErrorCode::Reserved,
            err.guest_message(),
            CepErrorData::Validation(ValidationErrorData {
                field: "content".into(),
                reason: format!("size limit exceeded: {actual} > {limit}"),
            }),
            Retriability::Permanent,
        ),
        MutateError::Cancelled => crate::error_map::cancelled_error(method),
        MutateError::NotFound { .. } | MutateError::InvalidRequest { .. } => CepError::new(
            CepErrorCode::TypeError,
            err.guest_message(),
            CepErrorData::Validation(ValidationErrorData {
                field: "path".into(),
                reason: err.audit_label().into(),
            }),
            Retriability::Permanent,
        ),
        MutateError::Io { .. } => CepError::new(
            CepErrorCode::ExecutionFailed,
            err.guest_message(),
            CepErrorData::Validation(ValidationErrorData {
                field: "path".into(),
                reason: err.audit_label().into(),
            }),
            Retriability::Permanent,
        ),
    }
}

/// Host authority behind the `workspace-mutate` imports.
pub struct WorkspaceMutationHost {
    workspace: Arc<LocalWorkspace>,
    authorized_region: AuthorizedEffectRegion,
    history: Option<Arc<WorkspaceHistory>>,
    scope_name: String,
    max_write_size: usize,
    refusal: Mutex<Option<CepError>>,
}

impl WorkspaceMutationHost {
    pub fn new(
        workspace: Arc<LocalWorkspace>,
        authorized_region: AuthorizedEffectRegion,
        history: Option<Arc<WorkspaceHistory>>,
        scope_name: impl Into<String>,
    ) -> Self {
        Self {
            workspace,
            authorized_region,
            history,
            scope_name: scope_name.into(),
            max_write_size: MAX_WRITE_SIZE,
            refusal: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn with_max_write_size(mut self, max_write_size: usize) -> Self {
        self.max_write_size = max_write_size;
        self
    }

    pub fn take_refusal(&self) -> Option<CepError> {
        self.refusal.lock().unwrap().take()
    }

    fn remember_refusal(&self, method: &str, err: &MutateError) {
        let mut slot = self.refusal.lock().unwrap();
        if slot.is_none() {
            *slot = Some(mutate_cep_error(method, err));
        }
    }

    /// Operation cell: `mutate:write:<workspace scope>` (Spec P0).
    fn operation_cell(&self) -> Cell {
        Cell {
            ctx_op: CtxOp::Mutate,
            ext_impact: ExtImpact::Write,
            scope: self.scope_name.clone(),
        }
    }

    fn scope(&self, admitted: &AdmittedTool) -> lancet_protocol::evidence::ObservedScope {
        resolve_scope(
            &admitted.descriptor,
            &self.scope_name,
            ChannelKind::Workspace,
        )
    }

    fn check_write_size(&self, requested: &str, content: &str) -> Result<(), MutateError> {
        let actual = content.len();
        if actual > self.max_write_size {
            return Err(MutateError::LimitExceeded {
                requested: requested.to_string(),
                limit: self.max_write_size,
                actual,
            });
        }
        Ok(())
    }

    fn authorize(
        &self,
        ctx: &MutationCallContext<'_>,
        resolved: &str,
        requested: &str,
    ) -> Result<(), MutateError> {
        let cell = self.operation_cell();
        let target = ObservedTarget::Path {
            path: resolved.to_string(),
        };
        if let Err(denial) = authorize_operation(ctx.admitted, &cell, &target, ctx.turn_id) {
            let err = MutateError::Denied {
                requested: requested.to_string(),
                reason: denial.to_string(),
            };
            ctx.evidence
                .record_denied(cell, self.scope(ctx.admitted), target, denial.to_string());
            self.remember_refusal(ctx.method, &err);
            return Err(err);
        }
        Ok(())
    }

    fn record_refusal(&self, ctx: &MutationCallContext<'_>, resolved: &str, err: &MutateError) {
        ctx.evidence.record_denied(
            self.operation_cell(),
            self.scope(ctx.admitted),
            ObservedTarget::Path {
                path: resolved.to_string(),
            },
            err.audit_label(),
        );
        self.remember_refusal(ctx.method, err);
    }

    fn parse_path(&self, requested: &str) -> Result<WorkspacePath, MutateError> {
        WorkspacePath::parse(requested).map_err(|err| MutateError::PathRejected {
            requested: requested.to_string(),
            reason: err.to_string(),
        })
    }

    fn guard_path(
        &self,
        requested: &str,
        ws_path: &WorkspacePath,
        intent: PathIntent,
    ) -> Result<(), MutateError> {
        if under_system_exclusion(ws_path.as_str()) {
            return Err(MutateError::PathRejected {
                requested: requested.to_string(),
                reason: "system path exclusion".into(),
            });
        }
        let facts = self
            .workspace
            .resolve_facts_blocking(ws_path, intent)
            .map_err(|err| map_workspace_io(requested, err))?;
        let guard_path = if ws_path.is_project_root() {
            ""
        } else {
            ws_path.as_str()
        };
        validate_path_facts(guard_path, &facts.to_guard(), &self.authorized_region).map_err(
            |cep| MutateError::Denied {
                requested: requested.to_string(),
                reason: cep.message,
            },
        )?;
        Ok(())
    }

    /// Read target bytes + Local Revision for edit compile.
    pub fn read_for_edit(
        &self,
        ctx: &MutationCallContext<'_>,
        path: &str,
    ) -> Result<FileBytes, MutateError> {
        let ws_path = self.parse_path(path)?;
        self.guard_path(path, &ws_path, PathIntent::Read)?;
        let resolved = ws_path.as_str().to_string();
        self.authorize(ctx, &resolved, path)?;

        let bytes = self
            .workspace
            .read_file_blocking(&ws_path)
            .map_err(|err| match err {
                LocalWorkspaceError::NotFound { .. } => MutateError::NotFound {
                    requested: path.to_string(),
                },
                other => map_workspace_io(path, other),
            })?;
        let content = String::from_utf8(bytes).map_err(|_| MutateError::InvalidRequest {
            reason: format!("file '{path}' is not valid UTF-8"),
        })?;
        let local_revision = content_hash(content.as_bytes());
        Ok(FileBytes {
            content,
            local_revision,
        })
    }

    /// ADR 0003 decision 8: no-replace create.
    pub fn create_if_absent(
        &self,
        ctx: &MutationCallContext<'_>,
        path: &str,
        content: &str,
        create_parents: bool,
    ) -> Result<CreateInstalled, MutateError> {
        self.check_write_size(path, content)?;
        let ws_path = self.parse_path(path)?;
        ws_path
            .require_file_path()
            .map_err(|err| MutateError::PathRejected {
                requested: path.to_string(),
                reason: err.to_string(),
            })?;
        self.guard_path(path, &ws_path, PathIntent::Create)?;
        let resolved = ws_path.as_str().to_string();
        self.authorize(ctx, &resolved, path)?;

        let created_dirs = if create_parents {
            missing_parent_dirs(self.workspace.as_ref(), &ws_path)?
        } else {
            Vec::new()
        };

        let created = match self.workspace.create_excl_sync(
            &ws_path,
            content.as_bytes(),
            create_parents,
        ) {
            Ok(created) => created,
            Err(LocalWorkspaceError::TargetExists { .. }) => {
                let err = MutateError::AlreadyExists {
                    requested: path.to_string(),
                };
                self.record_refusal(ctx, &resolved, &err);
                return Err(err);
            }
            Err(other) => {
                let err = map_workspace_io(path, other);
                self.record_refusal(ctx, &resolved, &err);
                return Err(err);
            }
        };

        let history = self.record_history(
            ctx,
            &resolved,
            None,
            content.as_bytes(),
            &created.content_hash,
        );

        ctx.evidence.record_effect(
            self.operation_cell(),
            self.scope(ctx.admitted),
            ObservedTarget::Path {
                path: resolved.clone(),
            },
            EffectSource::Operation,
            Some(created.bytes_written),
        );

        Ok(CreateInstalled {
            path: path.to_string(),
            bytes_written: created.bytes_written,
            local_revision: created.content_hash,
            created_dirs,
            history,
        })
    }

    /// Recheck expected revision, then atomic replace.
    pub fn replace_with_expected(
        &self,
        ctx: &MutationCallContext<'_>,
        path: &str,
        expected_revision: &str,
        content: &str,
    ) -> Result<ReplaceInstalled, MutateError> {
        self.check_write_size(path, content)?;
        let ws_path = self.parse_path(path)?;
        ws_path
            .require_file_path()
            .map_err(|err| MutateError::PathRejected {
                requested: path.to_string(),
                reason: err.to_string(),
            })?;
        self.guard_path(path, &ws_path, PathIntent::Overwrite)?;
        let resolved = ws_path.as_str().to_string();
        self.authorize(ctx, &resolved, path)?;

        let before = self
            .workspace
            .read_file_blocking(&ws_path)
            .map_err(|err| match err {
                LocalWorkspaceError::NotFound { .. } => MutateError::NotFound {
                    requested: path.to_string(),
                },
                other => map_workspace_io(path, other),
            })?;
        let observed = content_hash(&before);
        if observed != expected_revision {
            let err = MutateError::StaleRevision {
                requested: path.to_string(),
                expected: expected_revision.to_string(),
                observed,
            };
            self.record_refusal(ctx, &resolved, &err);
            return Err(err);
        }

        let installed = self
            .workspace
            .install_atomic_sync(&ws_path, content.as_bytes())
            .map_err(|err| {
                let mapped = map_workspace_io(path, err);
                self.record_refusal(ctx, &resolved, &mapped);
                mapped
            })?;

        let history = self.record_history(
            ctx,
            &resolved,
            Some(before.as_slice()),
            content.as_bytes(),
            &installed.content_hash,
        );

        ctx.evidence.record_effect(
            self.operation_cell(),
            self.scope(ctx.admitted),
            ObservedTarget::Path {
                path: resolved.clone(),
            },
            EffectSource::Operation,
            Some(installed.bytes_written),
        );

        Ok(ReplaceInstalled {
            path: path.to_string(),
            bytes_written: installed.bytes_written,
            local_revision: installed.content_hash,
            history,
        })
    }

    fn record_history(
        &self,
        ctx: &MutationCallContext<'_>,
        path: &str,
        before: Option<&[u8]>,
        after: &[u8],
        content_hash: &str,
    ) -> Option<RecordedMutation> {
        let history = self.history.as_ref()?;
        let record = MutationRecord {
            method: ctx.method.to_string(),
            turn_id: ctx.turn_id.to_string(),
            call_id: ctx.call_id,
            path: path.to_string(),
            before: before.map(<[u8]>::to_vec),
            after: after.to_vec(),
            content_hash: content_hash.to_string(),
            recorded_at: unix_seconds_now(),
        };
        match history.record(&record) {
            Ok(recorded) => recorded,
            Err(err) => {
                // History failure must not roll back an already-installed
                // mutation (ADR 0003 / #63). Surface via stderr until this
                // crate takes a tracing dependency for host-side ops.
                eprintln!(
                    "installed mutation was not recorded in the private workspace history: method={} path={} turn_id={} call_id={} error={}",
                    ctx.method, path, ctx.turn_id, ctx.call_id, err
                );
                None
            }
        }
    }
}

fn map_workspace_io(requested: &str, err: LocalWorkspaceError) -> MutateError {
    match err {
        LocalWorkspaceError::Path(err) => MutateError::PathRejected {
            requested: requested.to_string(),
            reason: err.to_string(),
        },
        LocalWorkspaceError::SymlinkRejected { .. } | LocalWorkspaceError::RootEscape => {
            MutateError::PathRejected {
                requested: requested.to_string(),
                reason: err.to_string(),
            }
        }
        LocalWorkspaceError::NotFound { .. } => MutateError::NotFound {
            requested: requested.to_string(),
        },
        LocalWorkspaceError::TargetExists { .. } => MutateError::AlreadyExists {
            requested: requested.to_string(),
        },
        LocalWorkspaceError::Io(err) => MutateError::Io {
            requested: requested.to_string(),
            detail: err.to_string(),
        },
    }
}

fn missing_parent_dirs(
    workspace: &LocalWorkspace,
    ws_path: &WorkspacePath,
) -> Result<Vec<String>, MutateError> {
    let mut missing = Vec::new();
    let mut parts: Vec<&str> = ws_path.as_str().split('/').collect();
    if parts.len() <= 1 {
        return Ok(missing);
    }
    parts.pop();
    let mut cur = String::new();
    for part in parts {
        if !cur.is_empty() {
            cur.push('/');
        }
        cur.push_str(part);
        let parent = WorkspacePath::parse(&cur).map_err(|err| MutateError::PathRejected {
            requested: ws_path.as_str().to_string(),
            reason: err.to_string(),
        })?;
        let facts = workspace
            .resolve_facts_blocking(&parent, PathIntent::Create)
            .map_err(|err| map_workspace_io(ws_path.as_str(), err))?;
        if !facts.exists {
            missing.push(cur.clone());
        }
    }
    Ok(missing)
}

fn unix_seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The `workspace-mutate` Tool Host imports
// ---------------------------------------------------------------------------

use crate::bindings;
use crate::runtime::StoreState;

type WitMutateError = bindings::lattice::tool_host::workspace_mutate::MutateError;

impl bindings::lattice::tool_host::workspace_mutate::Host for StoreState {
    fn read_for_edit(
        &mut self,
        path: String,
    ) -> Result<bindings::lattice::tool_host::workspace_mutate::FileBytes, WitMutateError> {
        let (host, ctx) = self.mutation("mutate.edit")?;
        host.read_for_edit(&ctx, &path)
            .map(
                |outcome| bindings::lattice::tool_host::workspace_mutate::FileBytes {
                    content: outcome.content,
                    local_revision: outcome.local_revision,
                },
            )
            .map_err(wit_mutate_error)
    }

    fn create_if_absent(
        &mut self,
        path: String,
        content: String,
        create_parents: bool,
    ) -> Result<bindings::lattice::tool_host::workspace_mutate::CreateResult, WitMutateError> {
        let (host, ctx) = self.mutation("mutate.create")?;
        host.create_if_absent(&ctx, &path, &content, create_parents)
            .map(
                |outcome| bindings::lattice::tool_host::workspace_mutate::CreateResult {
                    path: outcome.path,
                    bytes_written: outcome.bytes_written,
                    local_revision: outcome.local_revision,
                    created_dirs: outcome.created_dirs,
                },
            )
            .map_err(wit_mutate_error)
    }

    fn replace_with_expected(
        &mut self,
        path: String,
        expected_revision: String,
        content: String,
    ) -> Result<bindings::lattice::tool_host::workspace_mutate::ReplaceResult, WitMutateError> {
        let (host, ctx) = self.mutation("mutate.edit")?;
        host.replace_with_expected(&ctx, &path, &expected_revision, &content)
            .map(
                |outcome| bindings::lattice::tool_host::workspace_mutate::ReplaceResult {
                    path: outcome.path,
                    bytes_written: outcome.bytes_written,
                    local_revision: outcome.local_revision,
                },
            )
            .map_err(wit_mutate_error)
    }
}

impl StoreState {
    fn mutation(
        &self,
        method: &'static str,
    ) -> Result<(Arc<WorkspaceMutationHost>, MutationCallContext<'_>), WitMutateError> {
        let host = self.call.mutate.as_ref().cloned().ok_or_else(|| {
            WitMutateError::Denied(String::from(
                "this call has no workspace mutation authority",
            ))
        })?;
        Ok((
            host,
            MutationCallContext {
                call_id: self.call.call_id,
                turn_id: &self.call.turn_id,
                method,
                admitted: &self.call.admitted,
                evidence: &self.call.evidence,
            },
        ))
    }
}

fn wit_mutate_error(err: MutateError) -> WitMutateError {
    let message = err.guest_message();
    match err {
        MutateError::NotFound { .. } => WitMutateError::NotFound(message),
        MutateError::AlreadyExists { .. } => WitMutateError::AlreadyExists(message),
        MutateError::StaleRevision { .. } => WitMutateError::StaleRevision(message),
        MutateError::PathRejected { .. } => WitMutateError::PathRejected(message),
        MutateError::Denied { .. } => WitMutateError::Denied(message),
        MutateError::LimitExceeded { .. } => WitMutateError::LimitExceeded(message),
        MutateError::Cancelled => WitMutateError::Cancelled(message),
        MutateError::InvalidRequest { .. } => WitMutateError::InvalidRequest(message),
        MutateError::Io { .. } => WitMutateError::IoError(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use lancet_protocol::cell::{ArtifactIdentity, Capability, ScopeDecl, ToolDescriptor, Trust};
    use lancet_protocol::evidence::CapabilityAdmissionPolicy;

    use crate::admission::admit_tool;
    use crate::history::WorkspaceHistoryBacking;

    fn mutate_descriptor() -> Arc<ToolDescriptor> {
        Arc::new(ToolDescriptor {
            name: "mutate.create".into(),
            version: "0.1.0".into(),
            description: "mutation fixture".into(),
            params_schema: serde_json::Value::Null,
            result_schema: serde_json::Value::Null,
            scopes: vec![ScopeDecl {
                name: "workspace".into(),
                trust: Trust::In,
                channel: ChannelKind::Workspace,
            }],
            capabilities: vec![Capability {
                name: "workspace.mutation".into(),
                cell: Cell {
                    ctx_op: CtxOp::Mutate,
                    ext_impact: ExtImpact::Write,
                    scope: "workspace".into(),
                },
            }],
            artifact: ArtifactIdentity {
                sha256: "0".repeat(64),
                toolchain: "test".into(),
            },
            abi_version: 1,
        })
    }

    fn admitted() -> AdmittedTool {
        admit_tool(
            mutate_descriptor(),
            &CapabilityAdmissionPolicy {
                enabled_tools: BTreeSet::from(["mutate.create".to_string()]),
                denied_capabilities: BTreeSet::new(),
                max_boundary: Trust::In,
            },
        )
        .expect("the mutation capability is admitted")
    }

    fn seeded_project() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("temp root");
        fs::write(dir.path().join("notes.txt"), "v1\n").expect("seed");
        dir
    }

    fn host_over(
        root: &Path,
        region: AuthorizedEffectRegion,
        history: Option<Arc<WorkspaceHistory>>,
    ) -> (WorkspaceMutationHost, CellEvidenceLedger, Arc<LocalWorkspace>) {
        let workspace = Arc::new(LocalWorkspace::new(root));
        (
            WorkspaceMutationHost::new(Arc::clone(&workspace), region, history, "workspace"),
            CellEvidenceLedger::new(),
            workspace,
        )
    }

    fn default_host(
        root: &Path,
    ) -> (WorkspaceMutationHost, CellEvidenceLedger, Arc<LocalWorkspace>) {
        host_over(root, AuthorizedEffectRegion::empty(), None)
    }

    fn ctx<'a>(
        tool: &'a AdmittedTool,
        evidence: &'a CellEvidenceLedger,
        method: &'a str,
    ) -> MutationCallContext<'a> {
        MutationCallContext {
            call_id: 7,
            turn_id: "turn-1",
            method,
            admitted: tool,
            evidence,
        }
    }

    fn deny_writes(root: &Path, patterns: &[&str]) -> AuthorizedEffectRegion {
        let mut region = AuthorizedEffectRegion::empty();
        for pattern in patterns {
            let joined = root.join(pattern);
            if pattern.ends_with("/**") {
                region
                    .denied_write_path_prefixes
                    .push(joined.parent().unwrap_or(root).to_path_buf());
            } else {
                region.denied_write_paths.push(joined);
            }
        }
        // validate_path_facts compares against facts.normalized_path which is
        // project-relative. Mirror native mutation tests by also storing the
        // relative form when the pattern is relative.
        region.denied_write_paths = patterns
            .iter()
            .filter(|p| !p.ends_with("/**"))
            .map(|p| PathBuf::from(p))
            .collect();
        region.denied_write_path_prefixes = patterns
            .iter()
            .filter_map(|p| p.strip_suffix("/**").map(PathBuf::from))
            .collect();
        region
    }

    #[test]
    fn create_if_absent_installs_new_file_and_records_effect() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");

        let installed = host
            .create_if_absent(&call, "fresh.txt", "hello\n", false)
            .expect("create");
        assert_eq!(installed.bytes_written, 6);
        assert_eq!(
            installed.local_revision,
            content_hash(b"hello\n")
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("fresh.txt")).unwrap(),
            "hello\n"
        );

        let (_, effects, denied) = evidence.snapshot();
        assert!(denied.denied.is_empty());
        assert_eq!(effects.effects.len(), 1);
        assert_eq!(effects.effects[0].cell.ctx_op, CtxOp::Mutate);
        assert_eq!(effects.effects[0].cell.ext_impact, ExtImpact::Write);
        assert_eq!(effects.effects[0].context_measure, Some(6));
    }

    #[test]
    fn create_if_absent_refuses_existing_target_without_overwrite() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");

        let err = host
            .create_if_absent(&call, "notes.txt", "nope\n", false)
            .expect_err("exists");
        assert!(matches!(err, MutateError::AlreadyExists { .. }));
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "v1\n"
        );
        assert_eq!(
            mutate_cep_error("mutate.create", &err).code,
            CepErrorCode::ReplanRequired
        );

        let (_, effects, denied) = evidence.snapshot();
        assert!(effects.effects.is_empty());
        assert_eq!(denied.denied.len(), 1);
        assert_eq!(denied.denied[0].reason, "already-exists");
    }

    #[test]
    fn replace_with_expected_installs_when_revision_matches() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");
        let expected = content_hash(b"v1\n");

        let installed = host
            .replace_with_expected(&call, "notes.txt", &expected, "v2\n")
            .expect("replace");
        assert_eq!(installed.local_revision, content_hash(b"v2\n"));
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "v2\n"
        );
    }

    #[test]
    fn replace_with_expected_refuses_stale_revision_without_write() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");

        let err = host
            .replace_with_expected(&call, "notes.txt", &content_hash(b"wrong\n"), "v2\n")
            .expect_err("stale");
        assert!(matches!(err, MutateError::StaleRevision { .. }));
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "v1\n"
        );
        assert_eq!(
            mutate_cep_error("mutate.edit", &err).code,
            CepErrorCode::ReplanRequired
        );
        let (_, effects, denied) = evidence.snapshot();
        assert!(effects.effects.is_empty());
        assert_eq!(denied.denied[0].reason, "stale-revision");
    }

    #[test]
    fn path_traversal_is_rejected_before_authorize() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");

        let err = host
            .create_if_absent(&call, "../escape.txt", "x\n", false)
            .expect_err("escape");
        assert!(matches!(err, MutateError::PathRejected { .. }));
    }

    #[test]
    fn write_deny_pattern_blocks_install() {
        let dir = seeded_project();
        let region = deny_writes(dir.path(), &["locked.txt"]);
        let (host, evidence, _) = host_over(dir.path(), region, None);
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");

        let err = host
            .create_if_absent(&call, "locked.txt", "no\n", false)
            .expect_err("denied");
        assert!(matches!(err, MutateError::Denied { .. }));
        assert!(!dir.path().join("locked.txt").exists());
    }

    #[test]
    fn write_size_limit_is_enforced_before_install() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let host = host.with_max_write_size(4);
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");

        let err = host
            .create_if_absent(&call, "big.txt", "12345", false)
            .expect_err("limit");
        assert!(matches!(
            err,
            MutateError::LimitExceeded {
                limit: 4,
                actual: 5,
                ..
            }
        ));
        assert!(!dir.path().join("big.txt").exists());
    }

    #[test]
    fn fault_injection_leaves_target_unchanged_and_records_denial() {
        let dir = seeded_project();
        let (host, evidence, workspace) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");
        workspace.fail_next_install();

        let err = host
            .replace_with_expected(&call, "notes.txt", &content_hash(b"v1\n"), "v2\n")
            .expect_err("injected");
        assert!(matches!(err, MutateError::Io { .. }));
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "v1\n"
        );
        let (_, effects, denied) = evidence.snapshot();
        assert!(effects.effects.is_empty());
        assert_eq!(denied.denied[0].reason, "io-error");
    }

    #[test]
    fn successful_create_appends_private_history() {
        let dir = seeded_project();
        let history = Arc::new(
            WorkspaceHistory::open(WorkspaceHistoryBacking::Repo(
                dir.path().join(".lattice-history"),
            ))
            .expect("history"),
        );
        let (host, evidence, _) = host_over(dir.path(), AuthorizedEffectRegion::empty(), Some(history.clone()));
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");

        let installed = host
            .create_if_absent(&call, "tracked.txt", "keep\n", false)
            .expect("create");
        assert!(installed.history.is_some());
        assert!(history.current().unwrap().is_some());
    }

    #[test]
    fn read_for_edit_returns_host_computed_revision() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");

        let bytes = host.read_for_edit(&call, "notes.txt").expect("read");
        assert_eq!(bytes.content, "v1\n");
        assert_eq!(bytes.local_revision, content_hash(b"v1\n"));
    }

    #[test]
    fn create_parents_reports_missing_dirs() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");

        let installed = host
            .create_if_absent(&call, "a/b/c.txt", "nested\n", true)
            .expect("create");
        assert_eq!(installed.created_dirs, vec!["a".to_string(), "a/b".to_string()]);
        assert!(dir.path().join("a/b/c.txt").is_file());
    }
}
