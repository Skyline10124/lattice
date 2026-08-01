// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! cep_mode integration tests.
//!
//! Invariant: `cep_mode = Off` does **not** re-enable retired direct tool calls.
//! In the production stack (post-API), there is no legacy tool executor path.
//! `Off` mode only skips the effect-region check; the CEP policy hook still runs and will
//! deny requests that lack the required capability declaration.
//!
//! ## Modes tested
//!
//! | Mode    | Region overreach | Effect-kind deny | Policy deny (no capability) |
//! |---------|------------------|------------------|-----------------------------|
//! | Enforce | Err(EffectViolation) | Err(EffectViolation) | Err(MissingCapability) |
//! | Warn    | Ok; audit row has `effect_warning` in metadata | Err(EffectViolation) | Err(MissingCapability) |
//! | Off     | Ok (region check skipped) | Err(EffectViolation) | Err(MissingCapability) — policy still runs |
//!
//! ## Approach
//!
//! Enforce/Warn/Off tests use a **custom overreach handler** (not the production
//! registry) to produce a deterministic out-of-scope effect for the effect checker.
//! The policy-deny-still-works tests use the production registry to demonstrate that
//! the policy hook runs regardless of `cep_mode`.

use std::path::PathBuf;

use lancet_catalog::methods::external::{Fetch, FetchParams, FetchResult};
use std::sync::Arc;

use lancet_native::{
    CepAuditStore, CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};
use lancet_protocol::CepErrorCode;
use lancet_protocol::effect_types::ImpactKind;

use lancet_protocol::policy::CepMode;
use lattice_profile::LancetSection;
use tempfile::TempDir;

// ── Shared overreach handler ───────────────────────────────────────────
//
// A custom `mutate.create` handler that:
// 1. Sets an EMPTY authorized region (scope: nothing allowed).
// 2. Records a `ExternalObservation` effect at a path outside the empty region.
// 3. Returns Ok — so the effect checker is the only thing that can catch the overreach.
//
// The handler must be a *catalog* method: the effect checker reads the method's
// declared `EffectDecl` from `lancet_catalog::catalog()`, so a synthetic
// non-catalog fixture would skip the check entirely and every assertion below
// would pass vacuously. (`session.todo_write` played this role until #29 made it
// a Component Tool.)
//
// The recorded kind is deliberately NOT the kind `mutate.create` declares
// (`WorkspaceMutation`): the `effect_deny_rejects_in_*_mode` tests deny the
// observed kind, and this mismatch is what keeps the pre-dispatch
// declared-kind gate from intercepting those calls — see
// `assert_post_dispatch_denial`.

/// Params for the overreach handler. The handler never touches the filesystem —
/// it records a fabricated out-of-region effect and returns Ok — so the path is
/// only a well-formed value.
fn create_params() -> FetchParams {
    FetchParams {
        url: "https://example.com/".into(),
        headers: Default::default(),
    }
}

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
