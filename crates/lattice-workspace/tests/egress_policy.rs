// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public-surface smoke tests for `EgressPolicy` / `AllowlistEntry`.
//!
//! These tests verify that public types are constructible from downstream
//! crates.  Mapping-correctness tests live in `src/lib.rs`
//! under `#[cfg(test)] mod egress_tests` where `plan_egress` (pub(crate))
//! and the inspectable `NetPlan` are in scope.
//!
//! # What this file must NOT assert
//!
//! Do NOT test `plan_egress` or `NetPlan` here — those are `pub(crate)` and
//! not visible from an integration test crate.  Do NOT claim that a particular
//! `EgressPolicy` produces a particular SDK `NetworkPolicy` state — that is
//! live on the KVM integration lane.

use lattice_workspace::{AllowlistEntry, EgressPolicy};

/// Composite egress public-surface test — matches the `test_egress` CI filter.
///
/// Verifies that all three `EgressPolicy` variants are publicly constructible
/// from a downstream crate.  Mapping correctness is tested in
/// `src/lib.rs#egress_tests` where `plan_egress` + `NetPlan` are in scope.
#[test]
fn test_egress_variants_constructible() {
    // public-surface variant: Blocked is constructible.
    let blocked = EgressPolicy::Blocked;
    assert!(matches!(blocked, EgressPolicy::Blocked));

    // public-surface variant: Allowlist with entries is constructible.
    let allowlist = EgressPolicy::Allowlist(vec![
        AllowlistEntry {
            target: "api.openai.com".into(),
        },
        AllowlistEntry {
            target: "pypi.org".into(),
        },
    ]);
    assert!(matches!(allowlist, EgressPolicy::Allowlist(_)));

    // Open is constructible.
    let open = EgressPolicy::Open;
    assert!(matches!(open, EgressPolicy::Open));
}

/// Blocked debug format is printable (smoke).
#[test]
fn test_egress_blocked_debug_is_printable() {
    let s = format!("{:?}", EgressPolicy::Blocked);
    assert!(!s.is_empty());
}

/// Allowlist entry target is preserved (smoke).
#[test]
fn test_egress_allowlist_entry_target() {
    let entry = AllowlistEntry {
        target: "crates.io".into(),
    };
    assert_eq!(entry.target, "crates.io");
}

#[test]
fn allowlist_entries_are_publicly_constructible() {
    let entry = AllowlistEntry {
        target: "pypi.org".into(),
    };
    let p = EgressPolicy::Allowlist(vec![entry]);
    assert!(
        matches!(p, EgressPolicy::Allowlist(_)),
        "EgressPolicy::Allowlist must be publicly constructible"
    );
}

#[test]
fn blocked_variant_is_publicly_constructible() {
    let p = EgressPolicy::Blocked;
    assert!(
        matches!(p, EgressPolicy::Blocked),
        "EgressPolicy::Blocked must be publicly constructible"
    );
}

#[test]
fn open_variant_is_publicly_constructible() {
    let p = EgressPolicy::Open;
    assert!(
        matches!(p, EgressPolicy::Open),
        "EgressPolicy::Open must be publicly constructible"
    );
}

#[test]
fn allowlist_entry_target_field_is_readable() {
    let entry = AllowlistEntry {
        target: "api.openai.com".into(),
    };
    assert_eq!(entry.target, "api.openai.com");
}
