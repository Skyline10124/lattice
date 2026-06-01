//! Turn-scoped active envelope store for CEP.
//!
//! `ActiveEnvelopeStore` holds the currently active `ExecutionEnvelope` for a
//! single turn. It is created by `lattice-api` during turn composition, injected
//! into `ServerRunConfig`, and cloned into every `HandlerContext`. The store is
//! dropped when the turn ends, enforcing turn-scoped lifetime.

use crate::guard::normalize_write_path_pattern;
use lancet_protocol::envelope::{Capability, ExecutionEnvelope};
use lancet_protocol::error::{CepErrorData, GuardErrorData, ValidationErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use std::fmt;
use tokio::sync::RwLock;

/// Outcome of a successful envelope declaration.
#[derive(Clone, Debug)]
pub struct DeclareOutcome {
    pub envelope_id: String,
    pub envelope_hash: String,
    pub accepted: bool,
    pub superseded_envelope_id: Option<String>,
}

/// Record of an active envelope in the store.
#[derive(Clone, Debug)]
pub struct EnvelopeRecord {
    pub envelope_id: String,
    pub envelope_hash: String,
    pub envelope: ExecutionEnvelope,
    pub declared_at_call_id: u64,
    pub supersedes: Option<String>,
    pub reason: Option<String>,
}

/// Internal mutable state behind the store.
struct ActiveEnvelopeState {
    active: Option<EnvelopeRecord>,
    next_seq: u64,
}

/// Turn-scoped active envelope store.
///
/// Thread-safe via `RwLock`. Send + Sync.
pub struct ActiveEnvelopeStore {
    inner: RwLock<ActiveEnvelopeState>,
}

impl fmt::Debug for ActiveEnvelopeStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActiveEnvelopeStore")
            .finish_non_exhaustive()
    }
}

impl Default for ActiveEnvelopeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ActiveEnvelopeStore {
    /// Create a new empty store.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(ActiveEnvelopeState {
                active: None,
                next_seq: 1,
            }),
        }
    }

    /// Declare an envelope without supersede.
    ///
    /// Returns `EnvelopeAlreadyActive` if an active envelope already exists.
    pub async fn declare(
        &self,
        envelope: ExecutionEnvelope,
        call_id: u64,
        turn_id: &str,
    ) -> Result<DeclareOutcome, CepError> {
        let mut state = self.inner.write().await;
        if state.active.is_some() {
            return Err(CepError::new(
                CepErrorCode::EnvelopeAlreadyActive,
                "an active envelope already exists; use supersede=true to replace it",
                CepErrorData::Guard(GuardErrorData {
                    method: "envelope.declare".into(),
                    reason: "envelope already active".into(),
                }),
                Retriability::Permanent,
            ));
        }
        validate_envelope(&envelope)?;

        let envelope_hash = compute_envelope_hash(&envelope);
        let envelope_id = format!("env-{}-{}", turn_id, state.next_seq);
        state.next_seq += 1;

        let record = EnvelopeRecord {
            envelope_id: envelope_id.clone(),
            envelope_hash: envelope_hash.clone(),
            envelope,
            declared_at_call_id: call_id,
            supersedes: None,
            reason: None,
        };
        state.active = Some(record);

        Ok(DeclareOutcome {
            envelope_id,
            envelope_hash,
            accepted: true,
            superseded_envelope_id: None,
        })
    }

    /// Declare an envelope with supersede.
    ///
    /// Atomically replaces the active envelope if one exists.
    pub async fn declare_with_supersede(
        &self,
        envelope: ExecutionEnvelope,
        call_id: u64,
        turn_id: &str,
        reason: Option<String>,
    ) -> Result<DeclareOutcome, CepError> {
        validate_envelope(&envelope)?;

        let mut state = self.inner.write().await;
        let superseded_envelope_id = state.active.as_ref().map(|r| r.envelope_id.clone());

        let envelope_hash = compute_envelope_hash(&envelope);
        let envelope_id = format!("env-{}-{}", turn_id, state.next_seq);
        state.next_seq += 1;

        let record = EnvelopeRecord {
            envelope_id: envelope_id.clone(),
            envelope_hash: envelope_hash.clone(),
            envelope,
            declared_at_call_id: call_id,
            supersedes: superseded_envelope_id.clone(),
            reason,
        };
        state.active = Some(record);

        Ok(DeclareOutcome {
            envelope_id,
            envelope_hash,
            accepted: true,
            superseded_envelope_id,
        })
    }

    /// Get the active envelope id and a clone of the envelope, if any.
    pub async fn active_envelope(&self) -> Option<(String, ExecutionEnvelope)> {
        let state = self.inner.read().await;
        state
            .active
            .as_ref()
            .map(|r| (r.envelope_id.clone(), r.envelope.clone()))
    }

    /// Get a clone of the active envelope record, if any.
    pub async fn active_record(&self) -> Option<EnvelopeRecord> {
        let state = self.inner.read().await;
        state.active.clone()
    }

    /// Get just the active envelope id, if any.
    pub async fn active_envelope_id(&self) -> Option<String> {
        let state = self.inner.read().await;
        state.active.as_ref().map(|r| r.envelope_id.clone())
    }
}

/// Compute a deterministic hash of the canonical envelope JSON.
fn compute_envelope_hash(envelope: &ExecutionEnvelope) -> String {
    let canonical = serde_json::to_string(envelope).unwrap_or_default();
    blake3::hash(canonical.as_bytes()).to_hex().to_string()
}

/// Validate envelope structure before storing.
fn validate_envelope(envelope: &ExecutionEnvelope) -> Result<(), CepError> {
    // Non-empty intent description
    if envelope.intent.description.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "envelope intent.description must not be empty",
            CepErrorData::Validation(ValidationErrorData {
                field: "intent.description".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Non-empty capabilities
    if envelope.capabilities.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "envelope capabilities must not be empty",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Validate WritePath patterns
    for cap in &envelope.capabilities {
        match cap {
            Capability::WritePath { path_pattern } => {
                validate_write_path_pattern(path_pattern)?;
            }
            Capability::RunCommand { argv_allowlist } => {
                if argv_allowlist.is_empty() {
                    return Err(CepError::new(
                        CepErrorCode::SchemaMismatch,
                        "RunCommand argv_allowlist must not be empty if present",
                        CepErrorData::Validation(ValidationErrorData {
                            field: "capabilities.RunCommand.argv_allowlist".into(),
                            reason: "must not be empty".into(),
                        }),
                        Retriability::Permanent,
                    ));
                }
                for entry in argv_allowlist {
                    if entry.is_empty() {
                        return Err(CepError::new(
                            CepErrorCode::SchemaMismatch,
                            "RunCommand argv_allowlist entries must not be empty",
                            CepErrorData::Validation(ValidationErrorData {
                                field: "capabilities.RunCommand.argv_allowlist".into(),
                                reason: "entry must not be empty".into(),
                            }),
                            Retriability::Permanent,
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    // Validate verify command if present
    if let Some(verify) = &envelope.verify {
        if verify.command.is_empty() {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                "verify command must not be empty",
                CepErrorData::Validation(ValidationErrorData {
                    field: "verify.command".into(),
                    reason: "must not be empty".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }

    Ok(())
}

/// Validate a WritePath pattern.
fn validate_write_path_pattern(pattern: &str) -> Result<(), CepError> {
    normalize_write_path_pattern(pattern).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::envelope::{
        Constraints, Impact, ImpactConfidence, ImpactKind, Intent, Scope,
    };
    use std::collections::BTreeSet;

    fn sample_envelope(description: &str) -> ExecutionEnvelope {
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::WritePath {
            path_pattern: "src/**".into(),
        });
        ExecutionEnvelope {
            intent: Intent {
                description: description.into(),
            },
            capabilities,
            scope: Scope {
                workspace_root: Some("/tmp/test".into()),
                extensions: serde_json::Map::new(),
            },
            impact: vec![Impact {
                kind: ImpactKind::WorkspaceMutation,
                target: "src/**".into(),
                scope: None,
                confidence: ImpactConfidence::Conservative,
                unknowns: vec![],
            }],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        }
    }

    fn read_only_envelope(description: &str) -> ExecutionEnvelope {
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::ReadPath {
            path_pattern: "src/**".into(),
        });
        capabilities.insert(Capability::Inspect);
        ExecutionEnvelope {
            intent: Intent {
                description: description.into(),
            },
            capabilities,
            scope: Scope {
                workspace_root: Some("/tmp/test".into()),
                extensions: serde_json::Map::new(),
            },
            impact: vec![Impact {
                kind: ImpactKind::WorkspaceObservation,
                target: "src/**".into(),
                scope: None,
                confidence: ImpactConfidence::Conservative,
                unknowns: vec![],
            }],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn store_new_returns_none_for_active_envelope() {
        let store = ActiveEnvelopeStore::new();
        assert!(store.active_envelope().await.is_none());
        assert!(store.active_envelope_id().await.is_none());
    }

    #[tokio::test]
    async fn store_declare_returns_success_when_no_active() {
        let store = ActiveEnvelopeStore::new();
        let env = sample_envelope("test declare");
        let outcome = store.declare(env, 1, "turn-1").await.unwrap();
        assert!(outcome.accepted);
        assert!(outcome.envelope_id.starts_with("env-turn-1-"));
        assert!(!outcome.envelope_hash.is_empty());
        assert!(outcome.superseded_envelope_id.is_none());
    }

    #[tokio::test]
    async fn store_declare_returns_already_active_on_double_declare() {
        let store = ActiveEnvelopeStore::new();
        let env1 = sample_envelope("first declare");
        store.declare(env1, 1, "turn-1").await.unwrap();

        let env2 = sample_envelope("second declare");
        let err = store.declare(env2, 2, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::EnvelopeAlreadyActive);
    }

    #[tokio::test]
    async fn store_declare_returns_already_active_before_validating_second_envelope() {
        let store = ActiveEnvelopeStore::new();
        let env1 = sample_envelope("first declare");
        store.declare(env1, 1, "turn-1").await.unwrap();

        let mut env2 = sample_envelope("invalid second declare");
        env2.intent.description = String::new();
        let err = store.declare(env2, 2, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::EnvelopeAlreadyActive);
    }

    #[tokio::test]
    async fn store_supersede_atomically_replaces_active() {
        let store = ActiveEnvelopeStore::new();
        let env1 = sample_envelope("original");
        let outcome1 = store.declare(env1, 1, "turn-1").await.unwrap();

        let env2 = sample_envelope("replacement");
        let outcome2 = store
            .declare_with_supersede(env2, 2, "turn-1", Some("upgrade".into()))
            .await
            .unwrap();

        assert!(outcome2.accepted);
        assert_eq!(
            outcome2.superseded_envelope_id,
            Some(outcome1.envelope_id.clone())
        );
        assert_ne!(outcome2.envelope_id, outcome1.envelope_id);

        // Active envelope is now the new one
        let (active_id, active_env) = store.active_envelope().await.unwrap();
        assert_eq!(active_id, outcome2.envelope_id);
        assert_eq!(active_env.intent.description, "replacement");
    }

    #[tokio::test]
    async fn store_supersede_with_no_active_just_declares() {
        let store = ActiveEnvelopeStore::new();
        let env = sample_envelope("first with supersede");
        let outcome = store
            .declare_with_supersede(env, 1, "turn-1", None)
            .await
            .unwrap();
        assert!(outcome.accepted);
        assert!(outcome.superseded_envelope_id.is_none());
    }

    #[tokio::test]
    async fn store_envelope_id_is_turn_scoped() {
        let store = ActiveEnvelopeStore::new();
        let env = sample_envelope("turn scoped");
        let outcome = store.declare(env, 1, "my-turn-42").await.unwrap();
        assert_eq!(outcome.envelope_id, "env-my-turn-42-1");
    }

    #[tokio::test]
    async fn store_envelope_id_monotonically_increases() {
        let store = ActiveEnvelopeStore::new();
        let env1 = sample_envelope("first");
        let o1 = store.declare(env1, 1, "turn-1").await.unwrap();

        let env2 = sample_envelope("second");
        let o2 = store
            .declare_with_supersede(env2, 2, "turn-1", None)
            .await
            .unwrap();

        assert_eq!(o1.envelope_id, "env-turn-1-1");
        assert_eq!(o2.envelope_id, "env-turn-1-2");
    }

    #[tokio::test]
    async fn store_rejects_empty_intent_description() {
        let store = ActiveEnvelopeStore::new();
        let mut env = sample_envelope("valid");
        env.intent.description = String::new();
        let err = store.declare(env, 1, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn store_rejects_empty_capabilities() {
        let store = ActiveEnvelopeStore::new();
        let mut env = sample_envelope("valid");
        env.capabilities = BTreeSet::new();
        let err = store.declare(env, 1, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn store_rejects_write_path_with_dot_dot() {
        let store = ActiveEnvelopeStore::new();
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::WritePath {
            path_pattern: "../etc/passwd".into(),
        });
        let env = ExecutionEnvelope {
            intent: Intent {
                description: "bad pattern".into(),
            },
            capabilities,
            scope: Scope::default(),
            impact: vec![],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        };
        let err = store.declare(env, 1, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[tokio::test]
    async fn store_rejects_absolute_write_path_pattern() {
        let store = ActiveEnvelopeStore::new();
        let mut env = sample_envelope("absolute pattern");
        env.capabilities = BTreeSet::new();
        env.capabilities.insert(Capability::WritePath {
            path_pattern: "/etc/passwd".into(),
        });

        let err = store.declare(env, 1, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[tokio::test]
    async fn store_rejects_unsupported_write_path_glob() {
        let store = ActiveEnvelopeStore::new();
        let mut env = sample_envelope("unsupported glob");
        env.capabilities = BTreeSet::new();
        env.capabilities.insert(Capability::WritePath {
            path_pattern: "**/*.rs".into(),
        });

        let err = store.declare(env, 1, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn store_rejects_write_path_with_nul() {
        let store = ActiveEnvelopeStore::new();
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::WritePath {
            path_pattern: "src/\0file".into(),
        });
        let env = ExecutionEnvelope {
            intent: Intent {
                description: "nul pattern".into(),
            },
            capabilities,
            scope: Scope::default(),
            impact: vec![],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        };
        let err = store.declare(env, 1, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[tokio::test]
    async fn store_rejects_empty_run_command_argv() {
        let store = ActiveEnvelopeStore::new();
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::RunCommand {
            argv_allowlist: vec![],
        });
        let env = ExecutionEnvelope {
            intent: Intent {
                description: "empty argv".into(),
            },
            capabilities,
            scope: Scope::default(),
            impact: vec![],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        };
        let err = store.declare(env, 1, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn store_rejects_empty_verify_command() {
        let store = ActiveEnvelopeStore::new();
        let mut env = sample_envelope("with verify");
        env.verify = Some(lancet_protocol::envelope::VerifyCommand {
            command: String::new(),
            expected_exit_code: Some(0),
        });
        let err = store.declare(env, 1, "turn-1").await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn store_read_only_envelope_declares_successfully() {
        let store = ActiveEnvelopeStore::new();
        let env = read_only_envelope("read only");
        let outcome = store.declare(env, 1, "turn-1").await.unwrap();
        assert!(outcome.accepted);
    }

    #[tokio::test]
    async fn store_envelope_hash_differs_for_different_content() {
        let store = ActiveEnvelopeStore::new();
        let env1 = sample_envelope("content A");
        let o1 = store.declare(env1, 1, "turn-1").await.unwrap();

        let store2 = ActiveEnvelopeStore::new();
        let env2 = sample_envelope("content B");
        let o2 = store2.declare(env2, 1, "turn-1").await.unwrap();

        assert_ne!(o1.envelope_hash, o2.envelope_hash);
    }

    #[tokio::test]
    async fn store_envelope_hash_same_for_identical_content() {
        let store = ActiveEnvelopeStore::new();
        let env1 = sample_envelope("same content");
        let o1 = store.declare(env1, 1, "turn-1").await.unwrap();

        let store2 = ActiveEnvelopeStore::new();
        let env2 = sample_envelope("same content");
        let o2 = store2.declare(env2, 1, "turn-1").await.unwrap();

        assert_eq!(o1.envelope_hash, o2.envelope_hash);
    }
}
