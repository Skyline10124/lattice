//! Low-level execution helpers for native CEP handlers.
//!
//! INTERNAL — superseded by CEP catalog methods.
//!
//! This module is not part of the public LANCET surface. The public tool
//! contract is the CEP method catalog in [`lancet_protocol`] (CATALOG,
//! params/results, CepError, envelope/policy, and audit/provenance schemas).

pub(crate) mod render;
pub(crate) mod shell;
pub(crate) mod web;
