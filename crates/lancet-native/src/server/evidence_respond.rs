// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Build a CellEvidenceTriple from a ledger snapshot (#139-B recovery).

use lancet_protocol::CellEvidenceTriple;
use lancet_runtime::CellEvidenceLedger;

/// Component ledger snapshot, or typed native non-secret evidence when absent.
pub(crate) fn triple_from_ledger(ledger: &CellEvidenceLedger, method: &str) -> CellEvidenceTriple {
    let (admission, effects, denied) = ledger.snapshot();
    match admission {
        Some(admission) => CellEvidenceTriple::new(admission, effects, denied),
        None => CellEvidenceTriple::native_method(method),
    }
}
