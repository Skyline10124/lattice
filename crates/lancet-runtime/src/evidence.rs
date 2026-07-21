// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Cell evidence ledger — records the `(Cell, Trust)` pairs produced by a
//! Tool call's result CellRef.
//!
//! In #5 slice C, evidence is produced only from the Tool's **result CellRef**
//! at call return: the host parses the trust-free 3-segment CellRef, verifies
//! it against the descriptor's capability cells, derives trust from the
//! descriptor's scope declaration, and records the 4-segment audit key.
//! Domain host operations do not produce evidence yet (#6).

use lancet_protocol::cell::{Cell, Trust};

/// In-memory recorder for one call's Cell evidence. Slice D connects this
/// to the durable audit lifecycle.
#[derive(Debug, Default)]
pub struct CellEvidenceLedger {
    records: Vec<(Cell, Trust)>,
}

impl CellEvidenceLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a `(cell, trust)` pair. Called by the host after successful
    /// result validation.
    pub fn record(&mut self, cell: Cell, trust: Trust) {
        self.records.push((cell, trust));
    }

    /// All recorded evidence pairs.
    pub fn records(&self) -> &[(Cell, Trust)] {
        &self.records
    }
}
