// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Cell evidence ledger — records the `(Cell, Trust)` pairs produced by a
//! Tool call's result CellRef.
//!
//! In #27 (Codeberg #5) slice C, evidence is produced only from the Tool's
//! **result CellRef** at call return: the host parses the trust-free
//! 3-segment CellRef, verifies it against the descriptor's capability cells,
//! derives trust from the descriptor's scope declaration, and records the
//! 4-segment audit key. Domain host operations do not produce evidence yet
//! (#28 (Codeberg #6)).
//!
//! The ledger is backed by `Arc<Mutex<...>>` so a clone taken before the
//! call shares the same underlying records: the call moves one clone into
//! the wasmtime `Store` (dropped on return), but the other clone retains
//! the records added during execution. D.3 reads `snapshot_keys()` from
//! the surviving clone after the call returns.

use std::sync::{Arc, Mutex};

use lancet_protocol::cell::{Cell, Trust};

/// In-memory recorder for one call's Cell evidence. Clones share the
/// underlying record buffer, so the caller can clone before passing into
/// a moved context and read the records back after the call returns.
#[derive(Debug, Default, Clone)]
pub struct CellEvidenceLedger {
    records: Arc<Mutex<Vec<(Cell, Trust)>>>,
}

impl CellEvidenceLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a `(cell, trust)` pair. Called by the host after successful
    /// result validation.
    pub fn record(&self, cell: Cell, trust: Trust) {
        self.records.lock().unwrap().push((cell, trust));
    }

    /// All recorded evidence pairs.
    pub fn records(&self) -> Vec<(Cell, Trust)> {
        self.records.lock().unwrap().clone()
    }

    /// The 4-segment audit keys (`ctx:ext:scope:trust`) for all recorded
    /// evidence. D.3 merges these into durable audit metadata.
    pub fn snapshot_keys(&self) -> Vec<String> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .map(|(cell, trust)| cell.audit_key(*trust))
            .collect()
    }
}
