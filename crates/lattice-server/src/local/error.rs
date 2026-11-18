// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `ApiError` 到本地 IPC `LocalError` 的错误分类与安全存储诊断映射。

use lattice_ipc::{ConflictReason, LocalError, LocalErrorKind, RecoveryCause, StoreDiagnostic};
use lattice_session::store::StoreError;

use crate::session::ApiError;

impl From<ApiError> for LocalError {
    fn from(error: ApiError) -> Self {
        use LocalErrorKind::*;
        let (kind, message) = match &error {
            ApiError::NotFound(_) => (NotFound, "session or Turn not found"),
            ApiError::Configuration(reason) => return Self::new(BadRequest, &reason.to_string()),
            ApiError::RequestBudgetExceeded(message) => return Self::new(BadRequest, message),
            ApiError::BadRequest(_) => (BadRequest, "request or frozen configuration is invalid"),
            ApiError::Conflict(reason) => {
                return Self {
                    conflict: Some(*reason),
                    ..Self::new(Conflict, "request identity or Session binding conflicts")
                };
            }
            ApiError::Busy(_) => (Busy, "session is busy"),
            ApiError::StoreUnavailable(_) => (StoreUnavailable, "store unavailable"),
            ApiError::Internal(_) => (Internal, "operation failed; inspect durable state"),
            ApiError::WorkerFailed => return Self::recovery(RecoveryCause::WorkerFailed),
            ApiError::Store(error) => {
                let mut result = Self::new(StoreUnavailable, "session store unavailable");
                result.store = (match error.as_ref() {
                    StoreError::NotInitialized { path } => {
                        Some(StoreDiagnostic::NotInitialized { path: path.clone() })
                    }
                    StoreError::LegacyStores { paths } => Some(StoreDiagnostic::LegacyStores {
                        paths: paths.clone(),
                    }),
                    StoreError::UnsupportedVersion {
                        path,
                        table,
                        key,
                        found,
                        supported,
                    } => Some(StoreDiagnostic::UnsupportedVersion {
                        path: path.clone(),
                        table: table.clone(),
                        key: key.clone(),
                        found: *found,
                        supported: *supported,
                    }),
                    StoreError::Corruption { table, key, .. } => {
                        Some(StoreDiagnostic::Corruption {
                            table: table.clone(),
                            key: key.clone(),
                        })
                    }
                    StoreError::CommitFailed { .. }
                    | StoreError::CreationDurabilityUnknown { .. } => {
                        return Self::recovery(RecoveryCause::CommitFailed);
                    }
                    StoreError::WriteDisabled => {
                        return Self::recovery(RecoveryCause::WriteDisabled);
                    }
                    StoreError::NotFound { .. } => {
                        result.kind = NotFound;
                        None
                    }
                    StoreError::Conflict { .. } => {
                        result.kind = Conflict;
                        result.conflict = Some(ConflictReason::TurnIdUsed);
                        None
                    }
                    StoreError::SessionCollision { .. } => {
                        result.kind = Conflict;
                        result.conflict = Some(ConflictReason::SessionExists);
                        None
                    }
                    StoreError::BindingMismatch { .. } => {
                        result.kind = Conflict;
                        result.conflict = Some(ConflictReason::BindingMismatch);
                        None
                    }
                    StoreError::SettingsChanged { .. } => {
                        result.kind = Conflict;
                        result.conflict = Some(ConflictReason::SettingsChanged);
                        result.message = ConflictReason::SettingsChanged.to_string();
                        None
                    }
                    StoreError::Busy { .. } => {
                        result.kind = Busy;
                        None
                    }
                    StoreError::InvalidInput { .. } => {
                        result.kind = BadRequest;
                        None
                    }
                    _ => None,
                })
                .map(Box::new);
                return result;
            }
        };
        Self::new(kind, message)
    }
}

#[cfg(test)]
mod tests {
    use lattice_conversation::{SessionId, TurnId};
    use lattice_session::store::StoreError;

    use super::*;

    #[test]
    fn store_conflict_errors_map_to_distinct_local_conflict_reasons() {
        let sid = SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap();
        let tid = TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap();
        let settings_conflict = LocalError::from(ApiError::from(StoreError::SettingsChanged {
            session_id: sid,
        }));
        assert_eq!(settings_conflict.kind, LocalErrorKind::Conflict);
        assert_eq!(
            settings_conflict.conflict,
            Some(ConflictReason::SettingsChanged),
            "StoreError::SettingsChanged 必须独立映射为 ConflictReason::SettingsChanged"
        );
        let turn_conflict = LocalError::from(ApiError::from(StoreError::Conflict { turn_id: tid }));
        assert_eq!(turn_conflict.conflict, Some(ConflictReason::TurnIdUsed));
    }

    #[test]
    fn safe_store_diagnostics_preserve_path_and_never_expose_corruption_detail() {
        let error = LocalError::from(ApiError::from(StoreError::UnsupportedVersion {
            path: "/tmp/old state/lattice.redb".into(),
            table: "meta".into(),
            key: "format_version".into(),
            found: 1,
            supported: 2,
        }));
        let wire = serde_json::to_value(&error).unwrap();
        assert_eq!(wire["store"]["path"], "/tmp/old state/lattice.redb");
        assert_eq!(serde_json::from_value::<LocalError>(wire).unwrap(), error);
        let error = LocalError::from(ApiError::from(StoreError::LegacyStores {
            paths: vec!["/tmp/state/sessions.git".into()],
        }));
        assert!(
            matches!(error.store.as_deref(), Some(StoreDiagnostic::LegacyStores { paths }) if paths.len() == 1)
        );
        let error = LocalError::from(ApiError::from(StoreError::Corruption {
            table: "nodes".into(),
            key: "1".into(),
            detail: "secret-body".into(),
        }));
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("secret-body")
        );
    }

    #[test]
    fn configuration_errors_preserve_reason_while_bad_request_redacts_input() {
        let profile_missing = LocalError::from(ApiError::Configuration(
            crate::ConfigRejection::new(
                "agent profile 'reader' not found at project path /project/.lattice/agents/reader/agent.toml or global path /home/user/.lattice/agents/reader/agent.toml",
                None,
            ),
        ));
        assert_eq!(profile_missing.kind, LocalErrorKind::BadRequest);
        assert_eq!(
            profile_missing.message,
            "agent profile 'reader' not found at project path /project/.lattice/agents/reader/agent.toml or global path /home/user/.lattice/agents/reader/agent.toml"
        );

        let model_invalid = LocalError::from(ApiError::Configuration(crate::ConfigRejection::new(
            "model reference must be <provider>/<model>",
            None,
        )));
        assert_eq!(model_invalid.kind, LocalErrorKind::BadRequest);
        assert_eq!(
            model_invalid.message,
            "model reference must be <provider>/<model>"
        );

        let bad_request = LocalError::from(ApiError::BadRequest("sk-private-input".into()));
        assert_eq!(bad_request.kind, LocalErrorKind::BadRequest);
        assert_eq!(
            bad_request.message,
            "request or frozen configuration is invalid"
        );
        assert!(!bad_request.message.contains("sk-private-input"));
    }

    #[test]
    fn request_budget_exceeded_preserves_message_and_bad_request_stays_generic() {
        let budget_msg = "编码后的模型请求超过 LATTICE 本地请求体预算，请减少图片";
        let api_err = ApiError::RequestBudgetExceeded(budget_msg.into());
        assert!(!api_err.requires_reopen(), "请求预算超限不应触发会话库重开");
        assert!(!api_err.outcome_unknown(), "请求预算超限在写库前确定未生效");
        assert_eq!(api_err.to_string(), budget_msg, "Display 应原样输出文案");
        let mapped = LocalError::from(api_err);
        assert_eq!(
            mapped.kind,
            LocalErrorKind::BadRequest,
            "请求预算超限应映射为 BadRequest"
        );
        assert_eq!(
            mapped.message, budget_msg,
            "请求预算超限映射后的错误文案必须原样保留"
        );

        let generic = LocalError::from(ApiError::BadRequest("包含内部上下文的字符串".into()));
        assert_eq!(
            generic.kind,
            LocalErrorKind::BadRequest,
            "普通请求错误应映射为 BadRequest"
        );
        assert_eq!(
            generic.message, "request or frozen configuration is invalid",
            "普通 BadRequest 必须继续使用通用固定文案"
        );
        assert!(
            !generic.message.contains("包含内部上下文的字符串"),
            "普通 BadRequest 不得透传内部字符串"
        );
    }
}
