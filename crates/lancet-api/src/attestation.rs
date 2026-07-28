// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn attestation persistence built from recorded audit rows.

use lancet_native::{
    AttestationRecord, AuditQueryFilters, CepAuditContext, CepAuditStore, EvidenceLookup,
    build_in_toto_statement_with_evidence, source_audit_range_hash, statement_bytes,
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
    // The builder picks the predicate version per turn (#28 D8); the byte
    // writer canonicalizes v2 only, so a re-projected statement compares byte
    // for byte while stored v1 bytes keep their serde order.
    // #112: the Cell evidence records live in the evidence table, so the
    // builder resolves each Component row's `evidence_id` through the same
    // store the rows came from.
    let resolve = |id: lancet_native::EvidenceId| audit.call_evidence(id).ok().flatten();
    let statement = build_in_toto_statement_with_evidence(
        turn_id,
        &rows,
        runtime_version,
        &EvidenceLookup::new(&resolve),
    );
    let statement_json = statement_bytes(&statement)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use lancet_native::{
        AuditQueryFilters, ServerRunConfig, rebuild_in_toto_statement_with_evidence,
        statement_bytes,
    };
    use lancet_protocol::provenance::CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2;
    use lancet_protocol::{CapabilityAdmissionPolicy, Trust};
    use lancet_runtime::ComponentRuntime;

    use super::*;
    use crate::runtime::{DEFAULT_CHANNEL_CAPACITY, LancetServer};

    fn noop_admission(deny: &[&str]) -> CapabilityAdmissionPolicy {
        CapabilityAdmissionPolicy {
            enabled_tools: BTreeSet::from(["noop.echo".to_string()]),
            denied_capabilities: deny.iter().map(|name| (*name).to_string()).collect(),
            max_boundary: Trust::In,
        }
    }

    /// #28 D8/B6: a turn whose rows carry versioned Cell evidence attests as
    /// v2, and re-projecting it under the stored predicate type reproduces the
    /// stored bytes exactly — the canonical writer is the only difference from
    /// the v1 path, and it is deterministic.
    #[tokio::test]
    async fn component_turn_attests_v2_and_replays_byte_identically() {
        let components =
            Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
        let config =
            ServerRunConfig::in_memory_for_tests().with_capability_admission(noop_admission(&[]));
        let audit = Arc::clone(&config.audit);
        let ctx = config.audit_context.read().await.clone();

        let (client, _lifecycle) =
            LancetServer::spawn_composed(config, DEFAULT_CHANNEL_CAPACITY, components)
                .expect("spawn composed runtime");
        let value: serde_json::Value = client
            .call_named("noop.echo", serde_json::json!({ "message": "attest" }))
            .await
            .expect("component call");
        assert_eq!(value, serde_json::json!({ "echoed": "attest" }));
        audit.await_pending().await;

        store_turn_attestation(&audit, &ctx, &ctx.turn_id).expect("store attestation");
        let stored = audit
            .attestation_for_turn(&ctx.turn_id)
            .expect("attestation lookup")
            .expect("attestation stored");
        assert_eq!(
            stored.predicate_type, CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2,
            "a turn carrying Cell evidence attests as v2"
        );

        let rows = audit
            .query_rows(&AuditQueryFilters {
                session_id: Some(ctx.session_id.clone()),
                turn_id: Some(ctx.turn_id.clone()),
                ..Default::default()
            })
            .expect("query rows");
        // #112: the replay resolves Cell evidence through the same store, the
        // way `replay_audit` does in production.
        let resolve = |id: lancet_native::EvidenceId| audit.call_evidence(id).ok().flatten();
        let replayed = rebuild_in_toto_statement_with_evidence(
            &ctx.turn_id,
            &rows,
            &ctx.runtime_version,
            Some(stored.predicate_type.as_str()),
            &lancet_native::EvidenceLookup::new(&resolve),
        );
        assert_eq!(
            statement_bytes(&replayed).expect("replay bytes"),
            stored.statement_json,
            "stored and re-projected v2 statements must be byte identical"
        );
        // B6: the predicate keeps "why authorized" and "what happened" as
        // separate per-call fields.
        let statement: serde_json::Value =
            serde_json::from_str(&stored.statement_json).expect("statement is JSON");
        let evidence = &statement["predicate"]["evidence"][0];
        assert_eq!(
            evidence["admission"]["approved_capabilities"],
            serde_json::json!(["session.events", "session.result"])
        );
        assert_eq!(
            evidence["observed_effects"]["effects"]
                .as_array()
                .expect("observed effects array")
                .len(),
            2,
            "the emit-event operation and the result are both recorded"
        );
    }

    /// #28 C2: composition refuses a deny entry that names no composed
    /// capability, so the caller never receives a client for a policy the
    /// operator mistyped.
    #[tokio::test]
    async fn unknown_deny_name_refuses_composition() {
        let components =
            Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
        let config = ServerRunConfig::in_memory_for_tests()
            .with_capability_admission(noop_admission(&["session.tyop"]));

        let Err(err) = LancetServer::spawn_composed(config, DEFAULT_CHANNEL_CAPACITY, components)
        else {
            panic!("an unknown deny name must refuse composition");
        };
        assert!(
            format!("{err}").contains("session.tyop"),
            "the error must name the unknown deny entry: {err}"
        );
    }
}
