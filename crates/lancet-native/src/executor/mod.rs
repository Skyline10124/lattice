// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Low-level execution helpers for native CEP handlers.
//!
//! INTERNAL — superseded by CEP catalog methods.
//!
//! This module is not part of the public CEP surface. The public tool
//! contract is the CEP method catalog in `lancet_catalog` (CATALOG and
//! params/results), plus protocol errors, envelopes, policy, and provenance
//! wire types in `lancet_protocol`.

pub(crate) mod error;
pub(crate) mod limits;
pub(crate) mod render;
pub(crate) mod shell;
pub(crate) mod supervisor;
pub(crate) mod web;
