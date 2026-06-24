// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Equivalence verification (D-11, D-12) — stub.
//!
//! Filled in by Plan 39-04. The verifier compares topology (commit set,
//! parent edges under the idmap, branch ref tips) and transcripts (per-branch
//! message-by-message equivalence) between the SQLite source and the Git
//! target. It uses a tip-OID-direct reader for non-HEAD branches to avoid
//! mutating `meta.json` during verification.

use std::path::Path;

use crate::Result;

use super::extract::Idmap;

/// Verify equivalence between the SQLite source and the Git target.
///
/// Returns `Ok(true)` if topology and transcripts match, `Ok(false)` on any
/// mismatch (the caller fails the migration and does NOT archive the source).
/// Plan 04 implements the full comparison; this stub always passes so the
/// orchestration in Plan 02 compiles and the write path can be exercised.
pub(crate) fn verify_equivalence(
    _source_db: &Path,
    _repo: &git2::Repository,
    _idmap: &Idmap,
) -> Result<bool> {
    // TODO(39-04): topology + transcript equivalence.
    Ok(true)
}
