// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Cell effect model (ADR 0004 / spec 2026-07-01 §5).
//!
//! Closed core: `CtxOp` × `ExtImpact` over tool-declared scopes with
//! fail-closed trust. This is the sole effect vocabulary for Tool descriptors.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Effect of an operation on the agent context, managed like memory.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CtxOp {
    /// Result enters context (charged against the context budget by #28).
    Alloc,
    /// Context release. Vocabulary only this cycle (PRD R10): no ToolHost seam.
    Free,
    /// Mutates existing context state (not charged).
    Mutate,
}

/// Effect of an operation on the external world.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExtImpact {
    None,
    Read,
    Write,
}

/// Trust boundary. Absence of an explicit `in` declaration means `ex`
/// (fail-closed): a deny-`ex` turn rejects the capability.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    In,
    #[default]
    Ex,
}

/// Physical data-flow channel a scope is served by.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Workspace,
    Network,
    Session,
}

/// A tool-declared scope: open vocabulary, closed attributes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopeDecl {
    pub name: String,
    #[serde(default)]
    pub trust: Trust,
    pub channel: ChannelKind,
}

/// One dense effect cell: `ctx_op × ext_impact × scope-name`.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub ctx_op: CtxOp,
    pub ext_impact: ExtImpact,
    pub scope: String,
}

/// A capability names one cell; one cell may carry several capability
/// aliases (admission is per-capability, enforcement per-cell).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub name: String,
    pub cell: Cell,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CellKeyError {
    #[error("cell reference must have 3 segments, got {0}")]
    SegmentCount(usize),
    #[error("unknown segment value: {0}")]
    UnknownSegment(String),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    #[error("tool name {0} must be 1-64 characters of [A-Za-z0-9_-]")]
    InvalidToolName(String),
    #[error("capability name {0} must be lowercase dotted with at least two [a-z0-9_-] segments")]
    InvalidCapabilityName(String),
    #[error("capability {capability} references undeclared scope {scope}")]
    UndeclaredScope { capability: String, scope: String },
    #[error("duplicate capability name {0}")]
    DuplicateCapability(String),
    #[error("duplicate scope name {0}")]
    DuplicateScope(String),
}

/// Sole host-side representation of a Source Tool manifest (ADR 0004 §2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolDescriptor {
    pub name: String,
    pub version: String,
    pub description: String,
    /// JSON Schema for params/result, generated from the manifest.
    pub params_schema: serde_json::Value,
    pub result_schema: serde_json::Value,
    pub scopes: Vec<ScopeDecl>,
    pub capabilities: Vec<Capability>,
    pub artifact: ArtifactIdentity,
    pub abi_version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactIdentity {
    pub sha256: String,
    /// Toolchain fingerprint recorded at build time (rustc + wit-bindgen).
    pub toolchain: String,
}

/// A tool call's reported output cell + value (budget seam lands in #28).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolOutput {
    pub result_cell: Cell,
    pub value: serde_json::Value,
}

impl Cell {
    /// Trust-free 3-segment CellRef `ctx:ext:scope`; the host appends the
    /// `:in|ex` trust segment via `audit_key`.
    pub fn reference(&self) -> String {
        let ctx = match self.ctx_op {
            CtxOp::Alloc => "alloc",
            CtxOp::Free => "free",
            CtxOp::Mutate => "mutate",
        };
        let ext = match self.ext_impact {
            ExtImpact::None => "none",
            ExtImpact::Read => "read",
            ExtImpact::Write => "write",
        };
        format!("{ctx}:{ext}:{}", self.scope)
    }

    /// Only the host may add trust, after descriptor lookup.
    pub fn audit_key(&self, trust: Trust) -> String {
        let trust = match trust {
            Trust::In => "in",
            Trust::Ex => "ex",
        };
        format!("{}:{trust}", self.reference())
    }
}

/// Used by manifest parsing and guest result decoding. The scope segment
/// must not contain `:` — the format is `ctx:ext:scope`, so a `:`-bearing
/// scope shifts the segment count and is rejected.
pub fn parse_cell_ref(key: &str) -> Result<Cell, CellKeyError> {
    let parts: Vec<&str> = key.split(':').collect();
    let [ctx, ext, scope] = parts.as_slice() else {
        return Err(CellKeyError::SegmentCount(parts.len()));
    };
    let ctx_op = match *ctx {
        "alloc" => CtxOp::Alloc,
        "free" => CtxOp::Free,
        "mutate" => CtxOp::Mutate,
        other => return Err(CellKeyError::UnknownSegment(format!("ctx={other}"))),
    };
    let ext_impact = match *ext {
        "none" => ExtImpact::None,
        "read" => ExtImpact::Read,
        "write" => ExtImpact::Write,
        other => return Err(CellKeyError::UnknownSegment(format!("ext={other}"))),
    };
    if scope.is_empty() {
        return Err(CellKeyError::UnknownSegment("scope=<empty>".into()));
    }
    Ok(Cell {
        ctx_op,
        ext_impact,
        scope: (*scope).to_owned(),
    })
}

/// Capability-name grammar `^[a-z0-9_-]+(\.[a-z0-9_-]+)+$`: lowercase
/// dotted, at least two segments, so every capability carries a stable
/// namespace across tools and the cross-tool conflict rule compares
/// like with like.
fn is_valid_capability_name(name: &str) -> bool {
    let mut segments = 0usize;
    for segment in name.split('.') {
        if segment.is_empty()
            || !segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

/// Tool-name grammar `^[A-Za-z0-9_-]{1,64}$`. The name reaches every
/// provider unchanged as the model-visible function name, so it must satisfy
/// the strictest rule: OpenAI allows these characters up to 64, Anthropic up
/// to 128, and Gemini additionally accepts dots and colons. Namespaces are
/// separated with `__` (`mutate__create`).
fn is_valid_tool_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl ToolDescriptor {
    /// Registration-time consistency: the tool name follows the provider-safe
    /// grammar; every capability cell must root in a declared scope; names
    /// must be unique. Channels derive from scopes —
    /// there is no independent channels field (spec §11.3).
    ///
    /// Per-capability check order is fixed: grammar, then duplicate name,
    /// then undeclared scope — an ill-formed name is reported as such and
    /// never as a downstream scope or duplication problem.
    pub fn validate(&self) -> Result<(), DescriptorError> {
        if !is_valid_tool_name(&self.name) {
            return Err(DescriptorError::InvalidToolName(self.name.clone()));
        }
        // Duplicate scope names first, so undeclared-scope lookups below
        // never silently match the wrong copy of a duplicated name.
        let mut seen_scopes = BTreeSet::new();
        for s in &self.scopes {
            if !seen_scopes.insert(s.name.as_str()) {
                return Err(DescriptorError::DuplicateScope(s.name.clone()));
            }
        }
        let mut seen_caps = BTreeSet::new();
        for c in &self.capabilities {
            if !is_valid_capability_name(&c.name) {
                return Err(DescriptorError::InvalidCapabilityName(c.name.clone()));
            }
            if !seen_caps.insert(c.name.as_str()) {
                return Err(DescriptorError::DuplicateCapability(c.name.clone()));
            }
            if !seen_scopes.contains(c.cell.scope.as_str()) {
                return Err(DescriptorError::UndeclaredScope {
                    capability: c.name.clone(),
                    scope: c.cell.scope.clone(),
                });
            }
        }
        Ok(())
    }

    /// The channel set this tool may be handed, derived from `scopes`.
    pub fn required_channels(&self) -> BTreeSet<ChannelKind> {
        self.scopes.iter().map(|s| s.channel).collect()
    }

    pub fn trust_of(&self, scope: &str) -> Trust {
        self.scopes
            .iter()
            .find(|s| s.name == scope)
            .map(|s| s.trust)
            .unwrap_or(Trust::Ex) // fail-closed: undeclared == ex
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_trust_default_is_ex() {
        let json = serde_json::json!({
            "name": "workspace",
            "channel": "workspace",
        });
        let scope: ScopeDecl = serde_json::from_value(json).expect("trust field is optional");
        assert_eq!(scope.trust, Trust::Ex);
    }

    #[test]
    fn undeclared_scope_defaults_to_ex() {
        let desc = ToolDescriptor {
            name: "noop".into(),
            version: "0.1.0".into(),
            description: "no-op".into(),
            params_schema: serde_json::Value::Null,
            result_schema: serde_json::Value::Null,
            scopes: vec![ScopeDecl {
                name: "session".into(),
                trust: Trust::In,
                channel: ChannelKind::Session,
            }],
            capabilities: vec![],
            artifact: ArtifactIdentity {
                sha256: "deadbeef".into(),
                toolchain: "rustc-1.96".into(),
            },
            abi_version: 1,
        };
        assert_eq!(desc.trust_of("nope"), Trust::Ex);
        assert_eq!(desc.trust_of("session"), Trust::In);
    }

    #[test]
    fn cell_ref_round_trips() {
        // Full cartesian product across the closed enums (scope is open
        // vocabulary; one non-empty sample is enough to exercise the join).
        let ctx_ops = [CtxOp::Alloc, CtxOp::Free, CtxOp::Mutate];
        let ext_impacts = [ExtImpact::None, ExtImpact::Read, ExtImpact::Write];
        for ctx in ctx_ops {
            for ext in ext_impacts {
                let cell = Cell {
                    ctx_op: ctx,
                    ext_impact: ext,
                    scope: "workspace".into(),
                };
                let parsed = parse_cell_ref(&cell.reference()).expect("round trip parses");
                assert_eq!(parsed, cell, "round trip for {ctx:?}/{ext:?}");
            }
        }
    }

    #[test]
    fn cell_ref_rejects_guest_supplied_trust() {
        // A 4-segment guest-supplied trust suffix must be rejected at the
        // CellRef parsing seam — guests cannot self-claim `in`.
        let result = parse_cell_ref("alloc:read:ws:in");
        assert_eq!(result, Err(CellKeyError::SegmentCount(4)));
    }

    #[test]
    fn host_derives_audit_trust() {
        // Guest only ever emits a trust-free CellRef; the host adds the
        // trust segment after descriptor lookup. Inputs below are
        // independent literals, not derived from `audit_key`.
        let cell = Cell {
            ctx_op: CtxOp::Alloc,
            ext_impact: ExtImpact::Read,
            scope: "workspace".into(),
        };
        assert_eq!(cell.reference(), "alloc:read:workspace");
        assert_eq!(cell.audit_key(Trust::In), "alloc:read:workspace:in");
        assert_eq!(cell.audit_key(Trust::Ex), "alloc:read:workspace:ex");

        // No public seam lets a guest hand `Trust` to `audit_key`; the only
        // caller that can supply trust is the host, post descriptor lookup.
        let guest_supplied: Result<Cell, _> = parse_cell_ref("alloc:read:workspace:in");
        assert!(guest_supplied.is_err(), "guest cannot encode trust");
    }

    fn artifact() -> ArtifactIdentity {
        ArtifactIdentity {
            sha256: "deadbeef".into(),
            toolchain: "rustc-1.96".into(),
        }
    }

    fn descriptor_with_capability(name: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: "grammar".into(),
            version: "0.1.0".into(),
            description: "capability name grammar".into(),
            params_schema: serde_json::Value::Null,
            result_schema: serde_json::Value::Null,
            scopes: vec![ScopeDecl {
                name: "session".into(),
                trust: Trust::In,
                channel: ChannelKind::Session,
            }],
            capabilities: vec![Capability {
                name: name.into(),
                cell: Cell {
                    ctx_op: CtxOp::Alloc,
                    ext_impact: ExtImpact::None,
                    scope: "session".into(),
                },
            }],
            artifact: artifact(),
            abi_version: 1,
        }
    }

    #[test]
    fn descriptor_validation_enforces_scope_and_capability_rules() {
        let mut ghost = descriptor_with_capability("ghost.read");
        ghost.name = "ghost".into();
        ghost.description = "undeclared scope".into();
        ghost.capabilities[0].cell.ext_impact = ExtImpact::Read;
        ghost.capabilities[0].cell.scope = "nope".into();

        let mut dup_scope = descriptor_with_capability("todo.write");
        dup_scope.name = "dup-scope".into();
        dup_scope.description = "duplicate scope".into();
        dup_scope.scopes.push(dup_scope.scopes[0].clone());
        dup_scope.capabilities.clear();

        let mut dup_cap = descriptor_with_capability("todo.write");
        dup_cap.name = "dup-cap".into();
        dup_cap.description = "duplicate capability".into();
        dup_cap.capabilities.push(dup_cap.capabilities[0].clone());

        let mut bad = descriptor_with_capability("BAD");
        bad.capabilities[0].cell.scope = "nope".into();
        bad.capabilities.push(bad.capabilities[0].clone());

        for (name, descriptor, expected) in [
            // Capability references a scope that the descriptor never declared.
            (
                "undeclared_scope",
                ghost,
                Err(DescriptorError::UndeclaredScope {
                    capability: "ghost.read".into(),
                    scope: "nope".into(),
                }),
            ),
            // Duplicate scope name.
            (
                "duplicate_scope",
                dup_scope,
                Err(DescriptorError::DuplicateScope("session".into())),
            ),
            // Duplicate capability name.
            (
                "duplicate_capability",
                dup_cap,
                Err(DescriptorError::DuplicateCapability("todo.write".into())),
            ),
            (
                "valid_session_result",
                descriptor_with_capability("session.result"),
                Ok(()),
            ),
            (
                "valid_session_events",
                descriptor_with_capability("session.events"),
                Ok(()),
            ),
            (
                "valid_net_fetch_web",
                descriptor_with_capability("net.fetch.web"),
                Ok(()),
            ),
            ("valid_a_b", descriptor_with_capability("a.b"), Ok(())),
            (
                "valid_todo_write_v2",
                descriptor_with_capability("todo_write.v2"),
                Ok(()),
            ),
            (
                "valid_mutate_edit_file0",
                descriptor_with_capability("mutate-edit.file0"),
                Ok(()),
            ),
            // Single segment: no namespace.
            (
                "invalid_single_segment",
                descriptor_with_capability("session"),
                Err(DescriptorError::InvalidCapabilityName("session".into())),
            ),
            // Uppercase.
            (
                "invalid_uppercase",
                descriptor_with_capability("Session.Result"),
                Err(DescriptorError::InvalidCapabilityName(
                    "Session.Result".into(),
                )),
            ),
            // Empty middle segment.
            (
                "invalid_empty_middle_segment",
                descriptor_with_capability("session..result"),
                Err(DescriptorError::InvalidCapabilityName(
                    "session..result".into(),
                )),
            ),
            // Leading dot.
            (
                "invalid_leading_dot",
                descriptor_with_capability(".session"),
                Err(DescriptorError::InvalidCapabilityName(".session".into())),
            ),
            // Trailing dot.
            (
                "invalid_trailing_dot",
                descriptor_with_capability("session."),
                Err(DescriptorError::InvalidCapabilityName("session.".into())),
            ),
            // Empty.
            (
                "invalid_empty",
                descriptor_with_capability(""),
                Err(DescriptorError::InvalidCapabilityName("".into())),
            ),
            // Space.
            (
                "invalid_space",
                descriptor_with_capability("session result"),
                Err(DescriptorError::InvalidCapabilityName(
                    "session result".into(),
                )),
            ),
            // Cell-ref separator.
            (
                "invalid_cell_ref_separator",
                descriptor_with_capability("session:result"),
                Err(DescriptorError::InvalidCapabilityName(
                    "session:result".into(),
                )),
            ),
            // An ill-formed name must surface as InvalidCapabilityName even when
            // it is also duplicated and roots in an undeclared scope.
            (
                "grammar_precedes_other_checks",
                bad,
                Err(DescriptorError::InvalidCapabilityName("BAD".into())),
            ),
        ] {
            assert_eq!(descriptor.validate(), expected, "{name}");
        }
    }

    #[test]
    fn tool_name_grammar_is_accepted_by_every_provider() {
        let with_name = |name: &str| ToolDescriptor {
            name: name.into(),
            ..descriptor_with_capability("session.result")
        };
        for valid in ["session__todo_write", "fetch-web", &"a".repeat(64)] {
            assert_eq!(with_name(valid).validate(), Ok(()), "{valid}");
        }
        for invalid in [
            "mutate.create",
            "",
            &"a".repeat(65),
            "ns:tool",
            "mutate create",
        ] {
            assert_eq!(
                with_name(invalid).validate(),
                Err(DescriptorError::InvalidToolName(invalid.into())),
                "{invalid:?}"
            );
        }
    }
}
