// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn attestation persistence built from recorded audit rows.

use lancet_native::{
    AttestationRecord, AuditQueryFilters, CepAuditContext, CepAuditStore, build_in_toto_statement,
    source_audit_range_hash,
};

use crate::error::LancetError;

/// Persist an unsigned in-toto attestation for `turn_id` after audit flush.
///
/// No-ops when `turn_id` is empty, when an attestation already exists for the
/// turn, or when the turn recorded no audit rows.
pub fn store_turn_attestation(
    audit: &CepAuditStore,
    ctx: &CepAuditContext,
    turn_id: &str,
) -> Result<(), LancetError> {
    if turn_id.is_empty() {
        return Ok(());
    }
    if audit
        .attestation_for_turn(turn_id)
        .map_err(|err| LancetError::Internal(format!("attestation lookup: {err}")))?
        .is_some()
    {
        return Ok(());
    }

    let filters = AuditQueryFilters {
        session_id: Some(ctx.session_id.clone()),
        turn_id: Some(turn_id.to_string()),
        ..Default::default()
    };
    let rows = audit
        .query_rows(&filters)
        .map_err(|err| LancetError::Internal(format!("attestation query rows: {err}")))?;
    if rows.is_empty() {
        return Ok(());
    }

    // Legacy default builder id when the caller leaves the runtime version unset.
    let runtime_version = if ctx.runtime_version.is_empty() {
        "lattice"
    } else {
        ctx.runtime_version.as_str()
    };
    let statement = build_in_toto_statement(turn_id, &rows, runtime_version);
    let statement_json = serde_json::to_string(&statement)
        .map_err(|err| LancetError::Internal(format!("attestation statement serialize: {err}")))?;
    let range_hash = source_audit_range_hash(&rows);
    let session_id = rows
        .first()
        .map(|r| r.session_id.clone())
        .unwrap_or_else(|| ctx.session_id.clone());
    let generated_at = rows
        .iter()
        .filter_map(|r| r.ended_at.as_deref())
        .next_back()
        .unwrap_or_else(|| rows[0].started_at.as_str())
        .to_string();

    audit
        .insert_attestation(&AttestationRecord {
            turn_id: turn_id.to_string(),
            session_id,
            statement_json,
            predicate_type: statement.predicate_type.clone(),
            generated_at,
            source_audit_range_or_hash: range_hash,
        })
        .map_err(|err| LancetError::Internal(format!("attestation insert: {err}")))?;

    Ok(())
}
