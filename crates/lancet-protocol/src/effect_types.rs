// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP effect type system.
//!
//! Defines the static effect vocabulary used by catalog method contracts:
//! impact kinds, effect kind sets, scope kinds, overlay behavior, failure
//! semantics, and the static effect declaration ceiling.

use crate::capability::RequiredCapability;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeSet;

/// The kind of impact an operation can have.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum ImpactKind {
    WorkspaceObservation,
    WorkspaceMutation,
    CommandExecution,
    ExternalObservation,
    SessionStateMutation,
    AuditObservation,
}

/// A const-constructible, order-canonical set of [`ImpactKind`] values.
///
/// Used in [`EffectDecl`] to declare the static effect ceiling for a method.
/// Always declare members in [`ImpactKind`] enum declaration order.
/// Serializes as a JSON array of snake_case strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EffectKindSet(pub &'static [ImpactKind]);

impl EffectKindSet {
    pub const fn from_static(kinds: &'static [ImpactKind]) -> Self {
        Self(kinds)
    }

    pub fn contains(&self, kind: &ImpactKind) -> bool {
        self.0.iter().any(|k| k == kind)
    }

    pub fn is_subset_of(&self, other: &EffectKindSet) -> bool {
        self.0.iter().all(|k| other.contains(k))
    }
}

impl Serialize for EffectKindSet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl JsonSchema for EffectKindSet {
    fn schema_name() -> Cow<'static, str> {
        "EffectKindSet".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <Vec<ImpactKind>>::json_schema(generator)
    }
}

impl From<EffectKindSet> for BTreeSet<ImpactKind> {
    fn from(kinds: EffectKindSet) -> Self {
        kinds.0.iter().cloned().collect()
    }
}

/// Static scope-region kind descriptor for effect declarations.
///
/// Describes the *class* of scope a method operates in, not concrete
/// per-invocation bounds (those are executor-owned).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectScopeKind {
    PathRead,
    ObservationRoot,
    WorkspaceWrite,
    CommandExecution,
    ExternalHost,
    SessionState,
}

/// Static overlay behavior declaration for a method.
///
/// v1.3.1 does not add overlay isolation; this field exists so every tool
/// contract explicitly states how it is expected to compose with a future
/// lower/upper filesystem boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlayBehavior {
    NotApplicable,
    ReadWorkspace,
    WriteWorkspace,
    CommandOpaque,
    ExternalOpaque,
    SessionState,
    AuditRead,
}

/// Static failure semantics declaration for a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureSemantics {
    NoBackendEffectOnFailure,
    AllOrNothing,
    PartialEffectsAudited,
    RetainDiffOnVerifyFailure,
}

/// Static method-level effect declaration (the permission ceiling).
///
/// Catalog methods carry one of these as `const EFFECT`. The `kinds` field is a
/// set (not kind + also).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EffectDecl {
    pub kinds: EffectKindSet,
    pub scope: EffectScopeKind,
    #[serde(serialize_with = "serialize_static_slice")]
    #[schemars(with = "Vec<RequiredCapability>")]
    pub capabilities: &'static [RequiredCapability],
    pub overlay: OverlayBehavior,
    pub failure: FailureSemantics,
}

impl EffectDecl {
    /// Registry-time completeness check for method contracts.
    pub fn validate_complete(&self, method: &str) -> Result<(), String> {
        if self.kinds.0.is_empty() {
            return Err(format!("{method} has an empty effect kind set"));
        }
        if self.capabilities.is_empty() {
            return Err(format!("{method} has no required capability declaration"));
        }
        if matches!(self.failure, FailureSemantics::PartialEffectsAudited)
            && !self.kinds.contains(&ImpactKind::CommandExecution)
        {
            return Err(format!(
                "{method} declares partial-effects failure semantics without command execution"
            ));
        }
        Ok(())
    }
}

fn serialize_static_slice<T: Serialize, S: serde::Serializer>(
    slice: &'static [T],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    slice.serialize(serializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_impact_kind_fails_deserialization() {
        let bad = serde_json::json!("unknown_impact");
        let result: Result<ImpactKind, _> = serde_json::from_value(bad);
        assert!(result.is_err());
    }
}
