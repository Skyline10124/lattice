// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Tool Host Structured Workspace Mutation operations (#31 slice A).
//!
//! The guest holds no filesystem path and no write handle. Its only reach is
//! the four `workspace-mutate` imports — `read-for-edit`, `create-if-absent`,
//! `replace-with-expected`, and `refuse-edit` — each of which re-enters this host with a
//! string the host re-resolves, authorizes against `mutate:write:workspace`,
//! and either installs atomically or refuses before any Local bytes change.
//!
//! Precondition enforcement and evidence recording are host-authoritative:
//! `read-for-edit` rejects a stale planning revision before guest compilation;
//! `replace-with-expected` rechecks at the final install operation; and
//! semantic guest diagnostics return through `refuse-edit` without installing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::validate_path_facts;
use lancet_authority::under_system_exclusion;
use lancet_dispatch::Cancellation;
use lancet_protocol::cell::{Cell, ChannelKind, CtxOp, ExtImpact};
use lancet_protocol::error::{
    CepError, CepErrorCode, CepErrorData, ExecutionErrorData, GuardErrorData, Retriability,
    ValidationErrorData,
};
use lancet_protocol::evidence::{
    MutationEvidence, MutationInstallation, MutationPrecondition, ObservedTarget,
};
use lancet_protocol::{InvocationId, TurnId};

use lattice_store::{Blocking, LockMode};

use crate::admission::{AdmittedTool, authorize_operation, resolve_scope};
use crate::evidence::CellEvidenceLedger;
use crate::history::{MutationRecord, RecordedMutation, WorkspaceHistory};
use crate::workspace::{
    ConditionalCreateError, ConditionalReplaceError, LocalWorkspace, LocalWorkspaceError,
    PathIntent, WorkspacePath, content_hash,
};

/// Maximum bytes a single mutation may write to one file (#120).
// Singularity: same byte cap as every workspace write (#120 / #31 review).
const MAX_WRITE_SIZE: usize = crate::workspace::MAX_WORKSPACE_WRITE_BYTES;

/// Call-scoped inputs for one mutate import.
pub struct MutationCallContext<'a> {
    pub invocation_id: InvocationId,
    pub turn_id: &'a str,
    /// CEP method name recorded into private history (`mutate.create` / `mutate.edit`).
    pub method: &'a str,
    pub admitted: &'a AdmittedTool,
    pub evidence: &'a CellEvidenceLedger,
    pub cancellation: Cancellation,
}

/// Guest-owned semantic edit diagnostic class, authenticated and projected by
/// the host so it cannot collapse into an untyped guest error string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditRefusalKind {
    Validation,
    Replan,
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
    NotFound {
        requested: String,
    },
    AlreadyExists {
        requested: String,
    },
    StaleRevision {
        requested: String,
        expected: String,
        observed: String,
    },
    SemanticReplan {
        requested: String,
        reason: String,
    },
    Validation {
        requested: String,
        field: String,
        reason: String,
    },
    PathRejected {
        requested: String,
        reason: String,
    },
    Denied {
        requested: String,
        reason: String,
    },
    LimitExceeded {
        requested: String,
        limit: usize,
        actual: usize,
    },
    Cancelled,
    InvalidRequest {
        reason: String,
    },
    Io {
        requested: String,
        detail: String,
    },
    History {
        requested: String,
        detail: String,
    },
}

impl MutateError {
    pub fn audit_label(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not-found",
            Self::AlreadyExists { .. } => "already-exists",
            Self::StaleRevision { .. } => "stale-revision",
            Self::SemanticReplan { .. } => "semantic-replan",
            Self::Validation { .. } => "validation",
            Self::PathRejected { .. } => "path-rejected",
            Self::Denied { .. } => "denied",
            Self::LimitExceeded { .. } => "limit-exceeded",
            Self::Cancelled => "cancelled",
            Self::InvalidRequest { .. } => "invalid-request",
            Self::Io { .. } => "io-error",
            Self::History { .. } => "history-error",
        }
    }

    pub fn guest_message(&self) -> String {
        match self {
            Self::NotFound { requested } => format!("path not found: {requested}"),
            Self::AlreadyExists { requested } => {
                format!("mutate target '{requested}' already exists; observe.workspace and replan")
            }
            Self::StaleRevision {
                requested,
                expected,
                observed,
            } => format!(
                "expected_hash for '{requested}' is stale: expected {expected}, current {observed}; observe.workspace read_file again"
            ),
            Self::SemanticReplan { reason, .. } => reason.clone(),
            Self::Validation { reason, .. } => reason.clone(),
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
            Self::History { requested, .. } => {
                format!("mutation installed at {requested}, but private history recording failed")
            }
        }
    }
}

/// Map a host mutate refusal to a structured CEP error.
pub fn mutate_cep_error(method: &str, err: &MutateError) -> CepError {
    match err {
        MutateError::AlreadyExists { .. }
        | MutateError::StaleRevision { .. }
        | MutateError::SemanticReplan { .. } => CepError::new(
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
            CepErrorCode::ToolResourceExhausted,
            err.guest_message(),
            CepErrorData::Execution(ExecutionErrorData {
                command: method.into(),
                exit_code: Some(1),
                stderr: Some(format!("size limit exceeded: {actual} > {limit}")),
            }),
            Retriability::Permanent,
        ),
        MutateError::Cancelled => crate::error_map::cancelled_error(method),
        MutateError::Validation { field, reason, .. } => CepError::new(
            CepErrorCode::SchemaMismatch,
            reason.clone(),
            CepErrorData::Validation(ValidationErrorData {
                field: field.clone(),
                reason: reason.clone(),
            }),
            Retriability::Permanent,
        ),
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
        MutateError::History { .. } => CepError::new(
            CepErrorCode::OutcomeIndeterminate,
            err.guest_message(),
            CepErrorData::Validation(ValidationErrorData {
                field: "workspace_history".into(),
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
    mutation_gate: Arc<Mutex<()>>,
    refusals: Mutex<HashMap<InvocationId, CepError>>,
}

impl WorkspaceMutationHost {
    pub fn new(
        workspace: Arc<LocalWorkspace>,
        authorized_region: AuthorizedEffectRegion,
        history: Option<Arc<WorkspaceHistory>>,
        scope_name: impl Into<String>,
    ) -> Self {
        let mutation_gate = workspace.mutation_sequence_gate();
        Self {
            workspace,
            authorized_region,
            history,
            scope_name: scope_name.into(),
            max_write_size: MAX_WRITE_SIZE,
            mutation_gate,
            refusals: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn with_max_write_size(mut self, max_write_size: usize) -> Self {
        self.max_write_size = max_write_size;
        self
    }

    pub fn take_refusal(&self, invocation_id: InvocationId) -> Option<CepError> {
        self.refusals.lock().unwrap().remove(&invocation_id)
    }

    fn remember_refusal(&self, invocation_id: InvocationId, method: &str, err: &MutateError) {
        self.refusals
            .lock()
            .unwrap()
            .entry(invocation_id)
            .or_insert_with(|| mutate_cep_error(method, err));
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

    fn check_cancelled(
        &self,
        ctx: &MutationCallContext<'_>,
        path_for_evidence: &str,
    ) -> Result<(), MutateError> {
        if ctx.cancellation.is_cancelled() {
            let err = MutateError::Cancelled;
            self.record_refusal(ctx, path_for_evidence, &err);
            return Err(err);
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
            ctx.evidence.record_denied_with_mutation(
                cell,
                self.scope(ctx.admitted),
                target,
                denial.to_string(),
                Some(MutationEvidence {
                    before_revision: None,
                    after_revision: None,
                    precondition: MutationPrecondition::Denied,
                    installation: MutationInstallation::NotAttempted,
                }),
            );
            self.remember_refusal(ctx.invocation_id, ctx.method, &err);
            return Err(err);
        }
        Ok(())
    }

    fn record_refusal(&self, ctx: &MutationCallContext<'_>, resolved: &str, err: &MutateError) {
        ctx.evidence.record_denied_with_mutation(
            self.operation_cell(),
            self.scope(ctx.admitted),
            ObservedTarget::Path {
                path: resolved.to_string(),
            },
            err.audit_label(),
            mutation_evidence_for(err),
        );
        self.remember_refusal(ctx.invocation_id, ctx.method, err);
    }

    /// Early refusal (parse / guard / size) before install: still records
    /// MutationEvidence denial + typed CEP so guest `Err(String)` cannot
    /// collapse the outcome to generic `ExecutionFailed`.
    fn refuse_early(
        &self,
        ctx: &MutationCallContext<'_>,
        path_for_evidence: &str,
        err: MutateError,
    ) -> MutateError {
        self.record_refusal(ctx, path_for_evidence, &err);
        err
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
        expected_revision: &str,
    ) -> Result<FileBytes, MutateError> {
        self.check_cancelled(ctx, path)?;
        let ws_path = self
            .parse_path(path)
            .map_err(|err| self.refuse_early(ctx, path, err))?;
        self.guard_path(path, &ws_path, PathIntent::Read)
            .map_err(|err| self.refuse_early(ctx, ws_path.as_str(), err))?;
        let resolved = ws_path.as_str().to_string();
        self.authorize(ctx, &resolved, path)?;

        let bytes = match self.workspace.read_file_blocking(&ws_path) {
            Ok(bytes) => bytes,
            Err(err) => {
                let mapped = match err {
                    LocalWorkspaceError::NotFound { .. } => MutateError::NotFound {
                        requested: path.to_string(),
                    },
                    other => map_workspace_io(path, other),
                };
                self.record_refusal(ctx, &resolved, &mapped);
                return Err(mapped);
            }
        };
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                let err = MutateError::InvalidRequest {
                    reason: format!("file '{path}' is not valid UTF-8"),
                };
                self.record_refusal(ctx, &resolved, &err);
                return Err(err);
            }
        };
        let local_revision = content_hash(content.as_bytes());
        if local_revision != expected_revision {
            let err = MutateError::StaleRevision {
                requested: path.to_string(),
                expected: expected_revision.to_string(),
                observed: local_revision,
            };
            self.record_refusal(ctx, &resolved, &err);
            return Err(err);
        }
        Ok(FileBytes {
            content,
            local_revision,
        })
    }

    /// Preserve a semantic edit diagnostic as a typed host refusal without
    /// attempting any installation. The path is re-resolved and authorized so
    /// a guest cannot attach a diagnostic to authority it was never granted.
    pub fn refuse_edit(
        &self,
        ctx: &MutationCallContext<'_>,
        path: &str,
        kind: EditRefusalKind,
        field: &str,
        reason: &str,
    ) -> MutateError {
        if let Err(err) = self.check_cancelled(ctx, path) {
            return err;
        }
        let ws_path = match self.parse_path(path) {
            Ok(path) => path,
            Err(err) => return self.refuse_early(ctx, path, err),
        };
        if let Err(err) = self.guard_path(path, &ws_path, PathIntent::Read) {
            return self.refuse_early(ctx, ws_path.as_str(), err);
        }
        let resolved = ws_path.as_str().to_string();
        if let Err(err) = self.authorize(ctx, &resolved, path) {
            return err;
        }
        let err = match kind {
            EditRefusalKind::Validation => MutateError::Validation {
                requested: path.to_string(),
                field: field.to_string(),
                reason: reason.to_string(),
            },
            EditRefusalKind::Replan => MutateError::SemanticReplan {
                requested: path.to_string(),
                reason: reason.to_string(),
            },
        };
        self.record_refusal(ctx, &resolved, &err);
        err
    }

    /// ADR 0003 decision 8: no-replace create.
    pub fn create_if_absent(
        &self,
        ctx: &MutationCallContext<'_>,
        path: &str,
        content: &str,
        create_parents: bool,
    ) -> Result<CreateInstalled, MutateError> {
        self.check_write_size(path, content)
            .map_err(|err| self.refuse_early(ctx, path, err))?;
        let ws_path = self
            .parse_path(path)
            .map_err(|err| self.refuse_early(ctx, path, err))?;
        ws_path.require_file_path().map_err(|err| {
            self.refuse_early(
                ctx,
                path,
                MutateError::PathRejected {
                    requested: path.to_string(),
                    reason: err.to_string(),
                },
            )
        })?;
        self.guard_path(path, &ws_path, PathIntent::Create)
            .map_err(|err| self.refuse_early(ctx, ws_path.as_str(), err))?;
        let resolved = ws_path.as_str().to_string();
        self.authorize(ctx, &resolved, path)?;
        // ADR 0005 §5 lock order: the per-project `WorkspaceOperationLock`
        // (exclusive for a mutation) is taken first and held from here
        // through install and history publication; the private history takes
        // the repository write lock underneath it. The in-process gate keeps
        // the two Host instances over one workspace serialized as well.
        let _operation_guard = self
            .workspace
            .lock_operation(LockMode::Exclusive, Blocking::Block)
            .map_err(|err| MutateError::History {
                requested: path.to_string(),
                detail: err.to_string(),
            })
            .inspect_err(|err| self.record_refusal(ctx, &resolved, err))?;
        let _mutation_guard = self
            .mutation_gate
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        let created_dirs = if create_parents {
            missing_parent_dirs(self.workspace.as_ref(), &ws_path).inspect_err(|err| {
                self.record_refusal(ctx, &resolved, err);
            })?
        } else {
            Vec::new()
        };

        let (created, post_install_error) = match self.workspace.create_excl_sync_cancellable(
            &ws_path,
            content.as_bytes(),
            create_parents,
            || ctx.cancellation.is_cancelled(),
        ) {
            Ok(created) => (created, None),
            Err(ConditionalCreateError::InstalledButNotDurable { created, error }) => {
                (created, Some(map_workspace_io(path, error)))
            }
            Err(ConditionalCreateError::Cancelled) => {
                let err = MutateError::Cancelled;
                self.record_refusal(ctx, &resolved, &err);
                return Err(err);
            }
            Err(ConditionalCreateError::Workspace(LocalWorkspaceError::TargetExists {
                ..
            })) => {
                let err = MutateError::AlreadyExists {
                    requested: path.to_string(),
                };
                self.record_refusal(ctx, &resolved, &err);
                return Err(err);
            }
            Err(ConditionalCreateError::Workspace(other)) => {
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

        ctx.evidence.record_mutation_effect(
            self.operation_cell(),
            self.scope(ctx.admitted),
            ObservedTarget::Path {
                path: resolved.clone(),
            },
            MutationEvidence {
                before_revision: None,
                after_revision: Some(created.content_hash.clone()),
                precondition: MutationPrecondition::Satisfied,
                installation: MutationInstallation::Installed,
            },
        );

        let history = match history {
            Ok(history) => history,
            Err(err) => {
                self.remember_refusal(ctx.invocation_id, ctx.method, &err);
                return Err(err);
            }
        };

        if let Some(err) = post_install_error {
            self.remember_refusal(ctx.invocation_id, ctx.method, &err);
            return Err(err);
        }

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
        self.check_write_size(path, content)
            .map_err(|err| self.refuse_early(ctx, path, err))?;
        let ws_path = self
            .parse_path(path)
            .map_err(|err| self.refuse_early(ctx, path, err))?;
        ws_path.require_file_path().map_err(|err| {
            self.refuse_early(
                ctx,
                path,
                MutateError::PathRejected {
                    requested: path.to_string(),
                    reason: err.to_string(),
                },
            )
        })?;
        self.guard_path(path, &ws_path, PathIntent::Overwrite)
            .map_err(|err| self.refuse_early(ctx, ws_path.as_str(), err))?;
        let resolved = ws_path.as_str().to_string();
        self.authorize(ctx, &resolved, path)?;
        // ADR 0005 §5 lock order: the per-project `WorkspaceOperationLock`
        // (exclusive for a mutation) is taken first and held from here
        // through install and history publication; the private history takes
        // the repository write lock underneath it. The in-process gate keeps
        // the two Host instances over one workspace serialized as well.
        let _operation_guard = self
            .workspace
            .lock_operation(LockMode::Exclusive, Blocking::Block)
            .map_err(|err| MutateError::History {
                requested: path.to_string(),
                detail: err.to_string(),
            })
            .inspect_err(|err| self.record_refusal(ctx, &resolved, err))?;
        let _mutation_guard = self
            .mutation_gate
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        let (replacement, post_install_error) = match self.workspace.replace_if_revision_sync(
            &ws_path,
            expected_revision,
            content.as_bytes(),
            || ctx.cancellation.is_cancelled(),
        ) {
            Ok(replacement) => (replacement, None),
            Err(ConditionalReplaceError::InstalledButNotDurable { replacement, error }) => {
                (replacement, Some(map_workspace_io(path, error)))
            }
            Err(ConditionalReplaceError::StaleRevision { observed }) => {
                let err = MutateError::StaleRevision {
                    requested: path.to_string(),
                    expected: expected_revision.to_string(),
                    observed,
                };
                self.record_refusal(ctx, &resolved, &err);
                return Err(err);
            }
            Err(ConditionalReplaceError::Cancelled) => {
                let err = MutateError::Cancelled;
                self.record_refusal(ctx, &resolved, &err);
                return Err(err);
            }
            Err(ConditionalReplaceError::Workspace(err)) => {
                let mapped = map_workspace_io(path, err);
                self.record_refusal(ctx, &resolved, &mapped);
                return Err(mapped);
            }
        };
        let before = replacement.before;
        let installed = replacement.installed;
        let observed = content_hash(&before);

        let history = self.record_history(
            ctx,
            &resolved,
            Some(before.as_slice()),
            content.as_bytes(),
            &installed.content_hash,
        );

        ctx.evidence.record_mutation_effect(
            self.operation_cell(),
            self.scope(ctx.admitted),
            ObservedTarget::Path {
                path: resolved.clone(),
            },
            MutationEvidence {
                before_revision: Some(observed),
                after_revision: Some(installed.content_hash.clone()),
                precondition: MutationPrecondition::Satisfied,
                installation: MutationInstallation::Installed,
            },
        );

        let history = match history {
            Ok(history) => history,
            Err(err) => {
                self.remember_refusal(ctx.invocation_id, ctx.method, &err);
                return Err(err);
            }
        };

        if let Some(err) = post_install_error {
            self.remember_refusal(ctx.invocation_id, ctx.method, &err);
            return Err(err);
        }

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
    ) -> Result<Option<RecordedMutation>, MutateError> {
        let Some(history) = self.history.as_ref() else {
            return Ok(None);
        };
        let record = MutationRecord {
            method: ctx.method.to_string(),
            turn_id: TurnId::parse(ctx.turn_id).map_err(|err| MutateError::History {
                requested: path.to_string(),
                detail: format!("invalid turn id: {err}"),
            })?,
            invocation_id: ctx.invocation_id,
            path: path.to_string(),
            before: before.map(<[u8]>::to_vec),
            after: after.to_vec(),
            content_hash: content_hash.to_string(),
            recorded_at: unix_seconds_now(),
        };
        history.record(&record).map_err(|err| MutateError::History {
            requested: path.to_string(),
            detail: err.to_string(),
        })
    }
}

fn mutation_evidence_for(err: &MutateError) -> Option<MutationEvidence> {
    match err {
        MutateError::StaleRevision { observed, .. } => Some(MutationEvidence {
            before_revision: Some(observed.clone()),
            after_revision: None,
            precondition: MutationPrecondition::StaleRevision,
            installation: MutationInstallation::NotAttempted,
        }),
        MutateError::AlreadyExists { .. } => Some(MutationEvidence {
            before_revision: None,
            after_revision: None,
            precondition: MutationPrecondition::TargetExists,
            installation: MutationInstallation::NotAttempted,
        }),
        MutateError::Denied { .. } => Some(MutationEvidence {
            before_revision: None,
            after_revision: None,
            precondition: MutationPrecondition::Denied,
            installation: MutationInstallation::NotAttempted,
        }),
        MutateError::Io { .. } => Some(MutationEvidence {
            before_revision: None,
            after_revision: None,
            precondition: MutationPrecondition::Satisfied,
            installation: MutationInstallation::Failed,
        }),
        MutateError::History { .. } => None,
        _ => None,
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
        expected_revision: String,
    ) -> Result<bindings::lattice::tool_host::workspace_mutate::FileBytes, WitMutateError> {
        let (host, ctx) = self.mutation("mutate.edit")?;
        host.read_for_edit(&ctx, &path, &expected_revision)
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

    fn refuse_edit(
        &mut self,
        path: String,
        kind: bindings::lattice::tool_host::workspace_mutate::EditRefusalKind,
        field: String,
        reason: String,
    ) -> WitMutateError {
        let (host, ctx) = match self.mutation("mutate.edit") {
            Ok(authority) => authority,
            Err(err) => return err,
        };
        let kind = match kind {
            bindings::lattice::tool_host::workspace_mutate::EditRefusalKind::Validation => {
                EditRefusalKind::Validation
            }
            bindings::lattice::tool_host::workspace_mutate::EditRefusalKind::Replan => {
                EditRefusalKind::Replan
            }
        };
        wit_mutate_error(host.refuse_edit(&ctx, &path, kind, &field, &reason))
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
                invocation_id: self.call.invocation_id,
                turn_id: &self.call.turn_id,
                method,
                admitted: &self.call.admitted,
                evidence: &self.call.evidence,
                cancellation: self.call.cancellation.clone(),
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
        MutateError::SemanticReplan { .. } => WitMutateError::SemanticReplan(message),
        MutateError::Validation { .. } => WitMutateError::Validation(message),
        MutateError::PathRejected { .. } => WitMutateError::PathRejected(message),
        MutateError::Denied { .. } => WitMutateError::Denied(message),
        MutateError::LimitExceeded { .. } => WitMutateError::LimitExceeded(message),
        MutateError::Cancelled => WitMutateError::Cancelled(message),
        MutateError::InvalidRequest { .. } => WitMutateError::InvalidRequest(message),
        MutateError::Io { .. } => WitMutateError::IoError(message),
        MutateError::History { .. } => WitMutateError::IoError(message),
    }
}

#[cfg(test)]
mod tests {
    fn test_invocation_id(ordinal: u64) -> InvocationId {
        InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}"))
            .expect("invocation id")
    }

    use super::*;

    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use lancet_protocol::cell::{ArtifactIdentity, Capability, ScopeDecl, ToolDescriptor, Trust};
    use lancet_protocol::evidence::CapabilityAdmissionPolicy;
    use tokio::sync::watch;

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

    fn private_history() -> (tempfile::TempDir, Arc<WorkspaceHistory>) {
        let state = tempfile::TempDir::new().expect("private state root");
        let workspace_root = state.path().join("workspace");
        fs::create_dir_all(&workspace_root).expect("workspace root");
        let history = Arc::new(
            WorkspaceHistory::create(
                WorkspaceHistoryBacking::for_project_in(
                    &state.path().join("state"),
                    &workspace_root,
                )
                .expect("derive backing"),
            )
            .expect("history"),
        );
        (state, history)
    }

    fn host_over(
        root: &Path,
        region: AuthorizedEffectRegion,
        history: Option<Arc<WorkspaceHistory>>,
    ) -> (
        WorkspaceMutationHost,
        CellEvidenceLedger,
        Arc<LocalWorkspace>,
    ) {
        let workspace = Arc::new(LocalWorkspace::new(root));
        (
            WorkspaceMutationHost::new(Arc::clone(&workspace), region, history, "workspace"),
            CellEvidenceLedger::new(),
            workspace,
        )
    }

    fn default_host(
        root: &Path,
    ) -> (
        WorkspaceMutationHost,
        CellEvidenceLedger,
        Arc<LocalWorkspace>,
    ) {
        host_over(root, AuthorizedEffectRegion::empty(), None)
    }

    fn ctx<'a>(
        tool: &'a AdmittedTool,
        evidence: &'a CellEvidenceLedger,
        method: &'a str,
    ) -> MutationCallContext<'a> {
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        MutationCallContext {
            invocation_id: test_invocation_id(7),
            turn_id: "10000000-0000-4000-8000-000000000007",
            method,
            admitted: tool,
            evidence,
            cancellation: Cancellation::new(cancel_rx),
        }
    }

    fn cancelled_ctx<'a>(
        tool: &'a AdmittedTool,
        evidence: &'a CellEvidenceLedger,
        method: &'a str,
    ) -> MutationCallContext<'a> {
        let (_cancel_tx, cancel_rx) = watch::channel(true);
        MutationCallContext {
            invocation_id: test_invocation_id(7),
            turn_id: "10000000-0000-4000-8000-000000000007",
            method,
            admitted: tool,
            evidence,
            cancellation: Cancellation::new(cancel_rx),
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
            .map(PathBuf::from)
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
        assert_eq!(installed.local_revision, content_hash(b"hello\n"));
        assert_eq!(
            fs::read_to_string(dir.path().join("fresh.txt")).unwrap(),
            "hello\n"
        );

        let (_, effects, denied) = evidence.snapshot();
        assert!(denied.denied.is_empty());
        assert_eq!(effects.effects.len(), 1);
        assert_eq!(effects.effects[0].cell.ctx_op, CtxOp::Mutate);
        assert_eq!(effects.effects[0].cell.ext_impact, ExtImpact::Write);
        assert_eq!(effects.effects[0].context_measure, None);
        let mutation = effects.effects[0]
            .mutation
            .as_ref()
            .expect("mutation evidence");
        assert_eq!(mutation.before_revision, None);
        assert_eq!(
            mutation.after_revision.as_deref(),
            Some(installed.local_revision.as_str())
        );
        assert_eq!(mutation.precondition, MutationPrecondition::Satisfied);
        assert_eq!(mutation.installation, MutationInstallation::Installed);
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
        let mutation = denied.denied[0].mutation.as_ref().expect("mutation denial");
        assert_eq!(mutation.precondition, MutationPrecondition::TargetExists);
        assert_eq!(mutation.installation, MutationInstallation::NotAttempted);
    }

    #[test]
    fn hosts_for_same_workspace_share_install_history_gate() {
        let dir = seeded_project();
        let workspace = Arc::new(LocalWorkspace::new(dir.path()));
        let host_a = WorkspaceMutationHost::new(
            Arc::clone(&workspace),
            AuthorizedEffectRegion::empty(),
            None,
            "workspace",
        );
        let host_b = WorkspaceMutationHost::new(
            workspace,
            AuthorizedEffectRegion::empty(),
            None,
            "workspace",
        );

        let _guard = host_a.mutation_gate.lock().unwrap();
        assert!(
            host_b.mutation_gate.try_lock().is_err(),
            "all hosts over one workspace must serialize install through history recording"
        );
    }

    #[test]
    fn concurrent_create_same_target_has_one_winner_and_one_replan() {
        let dir = seeded_project();
        let workspace = Arc::new(LocalWorkspace::new(dir.path()));
        let host_a = WorkspaceMutationHost::new(
            Arc::clone(&workspace),
            AuthorizedEffectRegion::empty(),
            None,
            "workspace",
        );
        let host_b = WorkspaceMutationHost::new(
            workspace,
            AuthorizedEffectRegion::empty(),
            None,
            "workspace",
        );
        let tool = admitted();
        let evidence_a = CellEvidenceLedger::new();
        let evidence_b = CellEvidenceLedger::new();
        let barrier = Arc::new(std::sync::Barrier::new(3));

        let (a, b) = std::thread::scope(|scope| {
            let barrier_a = Arc::clone(&barrier);
            let host_a = &host_a;
            let tool_a = &tool;
            let evidence_a = &evidence_a;
            let a = scope.spawn(move || {
                let call = ctx(tool_a, evidence_a, "mutate.create");
                barrier_a.wait();
                host_a.create_if_absent(&call, "race.txt", "a\n", false)
            });
            let barrier_b = Arc::clone(&barrier);
            let host_b = &host_b;
            let tool_b = &tool;
            let evidence_b = &evidence_b;
            let b = scope.spawn(move || {
                let call = ctx(tool_b, evidence_b, "mutate.create");
                barrier_b.wait();
                host_b.create_if_absent(&call, "race.txt", "b\n", false)
            });
            barrier.wait();
            (a.join().unwrap(), b.join().unwrap())
        });

        let outcomes = [&a, &b];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Err(MutateError::AlreadyExists { .. })))
                .count(),
            1
        );
        let bytes = fs::read(dir.path().join("race.txt")).unwrap();
        assert!(bytes == b"a\n" || bytes == b"b\n");
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
        let mutation = denied.denied[0].mutation.as_ref().expect("mutation denial");
        assert_eq!(
            mutation.before_revision.as_deref(),
            Some(content_hash(b"v1\n").as_str())
        );
        assert_eq!(mutation.precondition, MutationPrecondition::StaleRevision);
        assert_eq!(mutation.installation, MutationInstallation::NotAttempted);
    }

    #[test]
    fn replace_with_expected_detects_external_writer_at_final_precondition_check() {
        let dir = seeded_project();
        let (host, evidence, workspace) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");
        workspace.replace_target_before_next_revision_check(
            WorkspacePath::parse("notes.txt").unwrap(),
            b"external\n".to_vec(),
        );

        let err = host
            .replace_with_expected(&call, "notes.txt", &content_hash(b"v1\n"), "agent\n")
            .expect_err("the final precondition must observe the external write");
        assert!(matches!(err, MutateError::StaleRevision { .. }));
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "external\n"
        );
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
        assert_eq!(
            host.take_refusal(test_invocation_id(7))
                .expect("structured refusal")
                .code,
            CepErrorCode::PathContainment
        );
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
        assert_eq!(
            host.take_refusal(test_invocation_id(7))
                .expect("structured refusal")
                .code,
            CepErrorCode::EffectViolation
        );
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
        assert_eq!(
            host.take_refusal(test_invocation_id(7))
                .expect("structured refusal")
                .code,
            CepErrorCode::ToolResourceExhausted
        );
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
    fn cancelled_create_is_refused_at_host_install_boundary() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = cancelled_ctx(&tool, &evidence, "mutate.create");

        let err = host
            .create_if_absent(&call, "cancelled.txt", "no\n", false)
            .expect_err("cancelled create");
        assert_eq!(err, MutateError::Cancelled);
        assert!(!dir.path().join("cancelled.txt").exists());
        assert_eq!(
            host.take_refusal(test_invocation_id(7))
                .expect("structured refusal")
                .code,
            CepErrorCode::CallCancelled
        );
    }

    #[test]
    fn cancelled_replace_is_refused_at_host_install_boundary() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = cancelled_ctx(&tool, &evidence, "mutate.edit");

        let err = host
            .replace_with_expected(&call, "notes.txt", &content_hash(b"v1\n"), "v2\n")
            .expect_err("cancelled replace");
        assert_eq!(err, MutateError::Cancelled);
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "v1\n"
        );
        assert_eq!(
            host.take_refusal(test_invocation_id(7))
                .expect("structured refusal")
                .code,
            CepErrorCode::CallCancelled
        );
    }

    #[test]
    fn successful_create_appends_private_history() {
        let dir = seeded_project();
        let (_state, history) = private_history();
        let (host, evidence, _) = host_over(
            dir.path(),
            AuthorizedEffectRegion::empty(),
            Some(history.clone()),
        );
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");

        let installed = host
            .create_if_absent(&call, "tracked.txt", "keep\n", false)
            .expect("create");
        assert!(installed.history.is_some());
        assert!(history.current().unwrap().is_some());
    }

    #[test]
    fn history_failure_after_install_is_returned_with_installed_effect() {
        let dir = seeded_project();
        let (_state, history) = private_history();
        history.fail_next_record();
        let (host, evidence, _) = host_over(
            dir.path(),
            AuthorizedEffectRegion::empty(),
            Some(history.clone()),
        );
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");

        let err = host
            .create_if_absent(&call, "installed.txt", "keep\n", false)
            .expect_err("history failure must not be reported as success");
        assert!(matches!(err, MutateError::History { .. }));
        assert_eq!(
            fs::read_to_string(dir.path().join("installed.txt")).unwrap(),
            "keep\n",
            "post-install failure must not fake rollback"
        );
        assert_eq!(history.current().unwrap(), None);
        let (_, effects, denied) = evidence.snapshot();
        assert_eq!(
            effects.effects.len(),
            1,
            "the installed write remains an effect"
        );
        assert_eq!(
            effects.effects[0]
                .mutation
                .as_ref()
                .expect("mutation evidence")
                .installation,
            MutationInstallation::Installed
        );
        assert!(
            denied.denied.is_empty(),
            "an installed write is not a denial"
        );
        assert_eq!(
            host.take_refusal(test_invocation_id(7))
                .expect("structured failure")
                .code,
            CepErrorCode::OutcomeIndeterminate
        );
    }

    #[test]
    fn create_parent_fsync_failure_keeps_installed_history_and_effect() {
        let dir = seeded_project();
        let (_state, history) = private_history();
        let (host, evidence, workspace) = host_over(
            dir.path(),
            AuthorizedEffectRegion::empty(),
            Some(history.clone()),
        );
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.create");
        workspace.fail_next_parent_fsync();

        let err = host
            .create_if_absent(&call, "durability.txt", "installed\n", false)
            .expect_err("parent fsync failure must remain visible to the caller");
        assert!(matches!(err, MutateError::Io { .. }));
        assert_eq!(
            fs::read_to_string(dir.path().join("durability.txt")).unwrap(),
            "installed\n"
        );
        assert!(history.current().unwrap().is_some());
        let (_, effects, denied) = evidence.snapshot();
        assert_eq!(effects.effects.len(), 1);
        assert_eq!(
            effects.effects[0]
                .mutation
                .as_ref()
                .expect("mutation evidence")
                .installation,
            MutationInstallation::Installed
        );
        assert!(denied.denied.is_empty());
    }

    #[test]
    fn replace_parent_fsync_failure_keeps_installed_history_and_effect() {
        let dir = seeded_project();
        let (_state, history) = private_history();
        let (host, evidence, workspace) = host_over(
            dir.path(),
            AuthorizedEffectRegion::empty(),
            Some(history.clone()),
        );
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");
        workspace.fail_next_parent_fsync();

        let err = host
            .replace_with_expected(&call, "notes.txt", &content_hash(b"v1\n"), "v2\n")
            .expect_err("parent fsync failure must remain visible to the caller");
        assert!(matches!(err, MutateError::Io { .. }));
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "v2\n"
        );
        assert!(history.current().unwrap().is_some());
        let (_, effects, denied) = evidence.snapshot();
        assert_eq!(effects.effects.len(), 1);
        assert_eq!(
            effects.effects[0]
                .mutation
                .as_ref()
                .expect("mutation evidence")
                .installation,
            MutationInstallation::Installed
        );
        assert!(denied.denied.is_empty());
    }

    #[test]
    fn read_for_edit_returns_host_computed_revision() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");

        let expected = content_hash(b"v1\n");
        let bytes = host
            .read_for_edit(&call, "notes.txt", &expected)
            .expect("read");
        assert_eq!(bytes.content, "v1\n");
        assert_eq!(bytes.local_revision, expected);
    }

    #[test]
    fn read_for_edit_refuses_stale_revision_before_guest_compilation() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");

        let err = host
            .read_for_edit(&call, "notes.txt", &content_hash(b"old\n"))
            .expect_err("stale read");
        assert!(matches!(err, MutateError::StaleRevision { .. }));
        assert_eq!(
            host.take_refusal(test_invocation_id(7))
                .expect("structured refusal")
                .code,
            CepErrorCode::ReplanRequired
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "v1\n"
        );
    }

    #[test]
    fn semantic_edit_refusal_preserves_replan_required() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");

        let err = host.refuse_edit(
            &call,
            "notes.txt",
            EditRefusalKind::Replan,
            "old",
            "old text matched 2 occurrences; provide occurrence",
        );
        assert!(matches!(err, MutateError::SemanticReplan { .. }));
        assert_eq!(
            host.take_refusal(test_invocation_id(7))
                .expect("structured refusal")
                .code,
            CepErrorCode::ReplanRequired
        );
    }

    #[test]
    fn semantic_edit_validation_preserves_schema_mismatch() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let call = ctx(&tool, &evidence, "mutate.edit");

        let err = host.refuse_edit(
            &call,
            "notes.txt",
            EditRefusalKind::Validation,
            "edits",
            "semantic edit produced no changes",
        );
        assert!(matches!(err, MutateError::Validation { .. }));
        let refusal = host
            .take_refusal(test_invocation_id(7))
            .expect("structured refusal");
        assert_eq!(refusal.code, CepErrorCode::SchemaMismatch);
        let CepErrorData::Validation(data) = refusal.data else {
            panic!("validation details");
        };
        assert_eq!(data.field, "edits");
    }

    #[test]
    fn structured_refusals_are_isolated_by_call_id() {
        let dir = seeded_project();
        let (host, evidence, _) = default_host(dir.path());
        let tool = admitted();
        let first = ctx(&tool, &evidence, "mutate.edit");
        let mut second = ctx(&tool, &evidence, "mutate.edit");
        second.invocation_id = test_invocation_id(8);

        host.refuse_edit(
            &first,
            "notes.txt",
            EditRefusalKind::Validation,
            "edits",
            "semantic edit produced no changes",
        );
        host.refuse_edit(
            &second,
            "notes.txt",
            EditRefusalKind::Replan,
            "old",
            "old text was not found",
        );

        assert_eq!(
            host.take_refusal(test_invocation_id(8))
                .expect("second refusal")
                .code,
            CepErrorCode::ReplanRequired
        );
        assert_eq!(
            host.take_refusal(test_invocation_id(7))
                .expect("first refusal")
                .code,
            CepErrorCode::SchemaMismatch
        );
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
        assert_eq!(
            installed.created_dirs,
            vec!["a".to_string(), "a/b".to_string()]
        );
        assert!(dir.path().join("a/b/c.txt").is_file());
    }
}
