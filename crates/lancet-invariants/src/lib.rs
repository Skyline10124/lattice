// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET invariant card system.
//!
//! Loads `.lancet/invariants/*.toml` cards, indexes them by anchor, exposes opaque
//! review helpers, and performs shallow Rust AST category checks for Guard L2.

pub mod guard_l2;
pub mod invariants;

pub use guard_l2::{
    categories_in_byte_range, check_l2, l2_status_for_method, node_kind_to_category, AstCategory,
    L2CheckResult, L2Violation,
};
pub use invariants::{
    default_invariants_dir, load_cards_from_dir, load_invariant_index_from_dir,
    opaque_human_review_error, opaque_human_review_message, opaque_notes, resolve_invariants_dir,
    CardAnchor, ForbiddenEntry, InvariantCard, InvariantCardIndex, OpaqueEntry,
};
