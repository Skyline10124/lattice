// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public LANCET status over the authoritative InvocationStore.

use std::path::Path;

use lancet_audit::{
    OperatorAuditRow, invocation_store::InvocationStore, operator::invocation_state_label,
};
use serde::{Deserialize, Serialize};

use crate::error::{LancetError, LancetResult};

const RECENT_CALL_LIMIT: usize = 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LancetStatusOutput {
    pub ledger_oid: Option<String>,
    pub recent_calls: Vec<OperatorAuditRow>,
    pub last_verdict: Option<String>,
}

/// Build status from one pinned ledger view without composing a Tool runtime.
pub fn collect_status(project_root: &Path) -> LancetResult<LancetStatusOutput> {
    let store_exists = InvocationStore::project_store_exists(project_root)
        .map_err(|err| LancetError::Config(format!("resolve invocation store: {err}")))?;
    if !store_exists {
        return Ok(LancetStatusOutput {
            ledger_oid: None,
            recent_calls: Vec::new(),
            last_verdict: None,
        });
    }

    let store = InvocationStore::open_project(project_root)
        .map_err(|err| LancetError::Internal(format!("open invocation store: {err}")))?;
    let view = store
        .pinned_view()
        .map_err(|err| LancetError::Internal(format!("pin invocation ledger: {err}")))?;
    let ledger_oid = Some(view.commit_oid.to_string());
    let mut recent_calls = view
        .snapshot
        .invocations
        .into_iter()
        .map(|(invocation_id, record)| OperatorAuditRow {
            invocation_id,
            record,
        })
        .collect::<Vec<_>>();
    recent_calls.sort_by_key(|row| {
        (
            row.record.binding.turn_id,
            row.record.binding.model_step_ordinal,
            row.record.binding.part_ordinal,
            row.invocation_id,
        )
    });
    recent_calls.reverse();
    recent_calls.truncate(RECENT_CALL_LIMIT);
    let last_verdict = recent_calls
        .first()
        .map(|row| invocation_state_label(&row.record.state).to_string());

    Ok(LancetStatusOutput {
        ledger_oid,
        recent_calls,
        last_verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_audit::{
        ScopedXdgStateHome,
        invocation_store::{InvocationStore, ReservationBinding},
    };
    use lancet_protocol::{BindingIdentity, InvocationId, PlanKey, SessionId, TurnId};
    use tempfile::TempDir;

    fn binding(index: u32) -> ReservationBinding {
        let turn_id = TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap();
        ReservationBinding {
            invocation_id: InvocationId::parse(&format!("00000000-0000-4000-8000-{index:012x}"))
                .unwrap(),
            identity: BindingIdentity {
                session_id: SessionId::parse("aaaaaaaa111144448888cccccccccccc").unwrap(),
                turn_id,
                model_step_ordinal: index,
                part_ordinal: 0,
                method: format!("tool.{index}"),
                descriptor_identity: "d".repeat(64),
                arguments_digest: [index as u8; 32],
            },
        }
    }

    #[test]
    fn status_is_empty_before_the_project_store_exists() {
        let _xdg = ScopedXdgStateHome::new();
        let project = TempDir::new().unwrap();
        assert_eq!(
            collect_status(project.path()).unwrap(),
            LancetStatusOutput {
                ledger_oid: None,
                recent_calls: Vec::new(),
                last_verdict: None,
            }
        );
    }

    #[test]
    fn status_returns_the_newest_twenty_current_records_from_one_pin() {
        let _xdg = ScopedXdgStateHome::new();
        let project = TempDir::new().unwrap();
        let store = InvocationStore::create_project(project.path()).unwrap();
        for index in 0..21 {
            let binding = binding(index);
            store
                .reserve_batch(
                    PlanKey {
                        turn_id: binding.identity.turn_id,
                        model_step_ordinal: index,
                    },
                    &[binding],
                )
                .unwrap();
        }

        let status = collect_status(project.path()).unwrap();
        assert_eq!(status.recent_calls.len(), 20);
        assert_eq!(status.recent_calls[0].record.binding.model_step_ordinal, 20);
        assert_eq!(status.recent_calls[19].record.binding.model_step_ordinal, 1);
        assert_eq!(status.last_verdict.as_deref(), Some("reserved"));
        assert_eq!(status.ledger_oid.unwrap().len(), 40);
    }
}
