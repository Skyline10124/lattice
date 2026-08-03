// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! cep_mode integration tests.
//!
//! These tests pin the profile-level default. Runtime mode behavior is covered
//! below the public facade, where native and Component dispatch can be exercised
//! without reintroducing LANCET server composition into this crate.

use lancet_protocol::policy::CepMode;
use lattice_profile::LancetSection;

// ── Default enforce test ───────────────────────────────────────────────

/// Profile default: `LancetSection::default()` must yield `CepMode::Enforce`.
///
/// Documents that the effective default mode is Enforce
/// when no `[lancet]` section is present in agent.toml.
#[test]
fn cep_mode_default_enforce_from_profile_schema() {
    let section = LancetSection::default();
    assert_eq!(
        section.mode,
        CepMode::Enforce,
        "LancetSection::default() must produce CepMode::Enforce, got {:?}",
        section.mode
    );
}

/// LancetSection deserialization: omitting `mode` from `[lancet]` table preserves Enforce default.
///
/// Documents the serialization guarantee: the TOML `[lancet]` section
/// without a `mode` key deserializes to `CepMode::Enforce`.
#[test]
fn cep_mode_enforce_when_cep_section_mode_absent() {
    // Deserialize a bare [lancet] table (no mode field) → Enforce.
    let cep_toml = r#"
# No mode key — must default to Enforce
"#;
    let section: LancetSection = toml::from_str(cep_toml).expect("parse LancetSection toml");
    assert_eq!(
        section.mode,
        CepMode::Enforce,
        "LancetSection without mode must default to Enforce, got {:?}",
        section.mode
    );
}
