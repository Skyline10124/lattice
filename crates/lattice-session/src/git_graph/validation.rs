// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Derived per-session validation projection for strict open (L-GH139A-11).
//!
//! Strict open must fail closed on malformed session refs, but re-parsing
//! every immutable object on every process start is O(global store content).
//! After a session mutation, this module records that session's exact
//! `(suffix, oid)` set at `refs/lattice/session-validation/<SessionId>`.
//!
//! The projection is derived and non-authoritative:
//! - open always enumerates live `refs/lattice/sessions/<id>/**`;
//! - a projection is used only when it names exactly that live set;
//! - a missing, unreadable, or mismatched projection forces a full
//!   per-session object validation before the handle is returned;
//! - open never writes a projection, so the read path has no global write.
//!
//! The projection ref is updated in the same kernel ref transaction as the
//! session's domain refs, so a crash cannot publish a domain ref change
//! while still claiming the previous set is validated.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use lattice_store::{RefExpectation, RefTxnEdit, StoreError, StoreRepo};

use super::state;
use crate::Result;

pub(crate) const PROJECTION_SCHEMA_VERSION: u32 = 1;
pub(crate) const PROJECTION_REF_PREFIX: &str = "refs/lattice/session-validation/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionValidationProjection {
    schema_version: u32,
    session_id: String,
    /// Sorted `(suffix, oid-hex)` pairs covering every live domain ref.
    refs: Vec<(String, String)>,
}

pub(crate) fn projection_ref_name(session_id: &str) -> Result<String> {
    state::validate_session_ref_id(session_id)?;
    Ok(format!("{PROJECTION_REF_PREFIX}{session_id}"))
}

/// True when the stored projection names exactly `live` for `session_id`.
///
/// Any defect in the projection is treated as a cache miss, never as
/// corruption: the caller must then validate the live refs themselves.
pub(crate) fn projection_matches(
    store: &StoreRepo,
    session_id: &str,
    live: &[(String, gix::ObjectId)],
) -> Result<bool> {
    let Some(oid) = store.ref_target(&projection_ref_name(session_id)?)? else {
        return Ok(false);
    };
    let Ok(bytes) = store.read_blob(oid) else {
        return Ok(false);
    };
    let Ok(parsed) = serde_json::from_slice::<SessionValidationProjection>(&bytes) else {
        return Ok(false);
    };
    if parsed.schema_version != PROJECTION_SCHEMA_VERSION || parsed.session_id != session_id {
        return Ok(false);
    }
    let mut expected: Vec<(String, String)> = live
        .iter()
        .map(|(suffix, oid)| (suffix.clone(), oid.to_string()))
        .collect();
    expected.sort();
    Ok(parsed.refs == expected)
}

/// Publish `edits` and refresh `session_id`'s validation projection in the
/// same kernel transaction. Callers map [`StoreError::CasConflict`].
pub(crate) fn publish_session_edits(
    store: &StoreRepo,
    session_id: &str,
    mut edits: Vec<RefTxnEdit>,
) -> std::result::Result<(), StoreError> {
    let prefix = format!("refs/lattice/sessions/{session_id}/");
    let mut refs: BTreeMap<String, gix::ObjectId> = store
        .enumerate_refs(&prefix)?
        .into_iter()
        .filter_map(|(name, oid)| {
            name.strip_prefix(&prefix)
                .map(|suffix| (suffix.to_string(), oid))
        })
        .collect();
    for edit in &edits {
        let Some(suffix) = edit.name.strip_prefix(&prefix) else {
            continue;
        };
        match edit.new {
            Some(oid) => {
                refs.insert(suffix.to_string(), oid);
            }
            None => {
                refs.remove(suffix);
            }
        }
    }
    let projection = SessionValidationProjection {
        schema_version: PROJECTION_SCHEMA_VERSION,
        session_id: session_id.to_string(),
        refs: refs
            .into_iter()
            .map(|(suffix, oid)| (suffix, oid.to_string()))
            .collect(),
    };
    let bytes = lattice_store::canon::canonical_json_bytes(&projection)?;
    let blob = store.write_blob(&bytes)?;
    let proj_ref = format!("{PROJECTION_REF_PREFIX}{session_id}");
    let current = store.ref_target(&proj_ref)?;
    edits.push(RefTxnEdit {
        name: proj_ref,
        expect: match current {
            Some(existing) => RefExpectation::MustMatch(existing),
            None => RefExpectation::MustNotExist,
        },
        new: Some(blob),
    });
    store.ref_transaction(&edits)
}
