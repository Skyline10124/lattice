//! Low-level execution helpers for native CEP handlers.
//!
//! INTERNAL — superseded by CEP catalog methods.
//!
//! This module is not part of the public CEP surface. The public tool
//! contract is the CEP method catalog in `lattice_cep_catalog` (CATALOG and
//! params/results), plus protocol errors, envelopes, policy, and provenance
//! wire types in `lattice_cep_protocol`.

pub(crate) mod render;
pub(crate) mod shell;
pub(crate) mod web;
