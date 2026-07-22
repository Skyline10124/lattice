// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Closed composition gate — the startup verification that every embedded
//! Tool registration matches a host-pinned identity before the catalog is
//! sealed.
//!
//! The gate is fail-closed: missing, duplicate, unapproved, drifted, or
//! ambient-importing registrations are rejected. After `compose` returns,
//! the `ClosedCatalog` is immutable — there is no runtime registration API.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lancet_protocol::cell::{ChannelKind, ToolDescriptor};
use sha2::{Digest, Sha256};
use wasmtime::component::Component;

/// Tool-side discovery record. It is **not** an approval record — a Tool
/// crate submits name + embedded bytes; only the trusted host composition
/// step may pin the identity (digest/ABI/imports).
#[derive(Debug, Copy, Clone)]
pub struct EmbeddedTool {
    pub name: &'static str,
    pub descriptor_json: &'static [u8],
    pub component_bytes: &'static [u8],
}

inventory::collect!(&'static EmbeddedTool);

/// Private to `lancet-runtime`; generated only by trusted host composition.
/// A Tool cannot self-approve: the digests, ABI, and import set are pinned
/// by the host, not supplied by the registration.
#[derive(Debug, Copy, Clone)]
pub struct ExpectedToolIdentity {
    pub name: &'static str,
    pub descriptor_sha256: [u8; 32],
    pub artifact_sha256: [u8; 32],
    pub abi_version: u32,
    pub imports: &'static [&'static str],
}

/// Every way the composition gate can reject a registration or identity set.
#[derive(Debug, thiserror::Error)]
pub enum CompositionError {
    #[error("unapproved tool name: {0}")]
    UnapprovedName(String),
    #[error("duplicate tool name: {0}")]
    DuplicateName(String),
    #[error("descriptor digest drift for {0}")]
    DescriptorDrift(#[source] sha2::digest::InvalidOutputSize),
    #[error("artifact hash mismatch for {0}")]
    ArtifactHashMismatch(String),
    #[error("descriptor JSON parse error for {0}")]
    DescriptorJson(#[source] serde_json::Error),
    #[error("descriptor validation failed for {0}")]
    DescriptorInvalid(#[source] lancet_protocol::cell::DescriptorError),
    #[error("descriptor name drift: registration={registration}, descriptor={descriptor}")]
    DescriptorNameDrift {
        registration: String,
        descriptor: String,
    },
    #[error("unsupported ABI version: {0}")]
    UnsupportedAbi(u32),
    #[error("descriptor artifact hash drift for {0}")]
    DescriptorArtifactDrift(String),
    #[error("component compile error for {0}")]
    Compile(String),
    #[error("ambient WASI import detected: {0:?}")]
    AmbientImport(BTreeSet<String>),
    #[error("import drift: declared={declared:?}, actual={actual:?}")]
    ImportDrift {
        declared: BTreeSet<String>,
        actual: BTreeSet<String>,
    },
    #[error("missing channel adapters: {missing:?}")]
    MissingChannelAdapters { missing: BTreeSet<ChannelKind> },
    #[error("params schema invalid for {0}")]
    ParamsSchema(String),
    #[error("result schema invalid for {0}")]
    ResultSchema(String),
    #[error("missing expected tools: {0:?}")]
    MissingNames(Vec<String>),
    #[error("engine creation failed")]
    Engine(#[source] wasmtime::Error),
    #[error("linker construction failed")]
    Linker(#[source] wasmtime::Error),
}

/// A compiled Tool ready for execution: descriptor, Component, and pre-built
/// JSON Schema validators (invalid schemas fail startup, not the first call).
pub struct CompiledTool {
    pub descriptor: Arc<ToolDescriptor>,
    pub component: Component,
    pub params_validator: jsonschema::Validator,
    pub result_validator: jsonschema::Validator,
}

/// Immutable catalog of verified Tools. No insert/register/load/path API:
/// after `compose`, the catalog is sealed.
pub struct ClosedCatalog {
    tools: BTreeMap<String, CompiledTool>,
}

impl std::fmt::Debug for ClosedCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClosedCatalog")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The supported Component Tool ABI version.
const SUPPORTED_ABI: u32 = 1;

impl ClosedCatalog {
    /// Compose the catalog from registrations and host-pinned identities.
    /// Every registration must match an expected identity; every expected
    /// identity must be satisfied. The catalog is immutable after this.
    pub fn compose(
        engine: &wasmtime::Engine,
        registrations: impl IntoIterator<Item = &'static EmbeddedTool>,
        expected_identities: &[ExpectedToolIdentity],
    ) -> Result<Self, CompositionError> {
        let expected: BTreeMap<&str, &ExpectedToolIdentity> = expected_identities
            .iter()
            .map(|identity| (identity.name, identity))
            .collect();
        let mut tools: BTreeMap<String, CompiledTool> = BTreeMap::new();

        for reg in registrations {
            let identity = expected
                .get(reg.name)
                .ok_or_else(|| CompositionError::UnapprovedName(reg.name.to_string()))?;
            if tools.contains_key(reg.name) {
                return Err(CompositionError::DuplicateName(reg.name.to_string()));
            }

            // Digests are host-pinned, not supplied by the registration.
            verify_sha256(reg.descriptor_json, identity.descriptor_sha256)
                .map_err(CompositionError::DescriptorDrift)?;
            verify_sha256(reg.component_bytes, identity.artifact_sha256)
                .map_err(|_| CompositionError::ArtifactHashMismatch(reg.name.to_string()))?;

            let descriptor: ToolDescriptor = serde_json::from_slice(reg.descriptor_json)
                .map_err(CompositionError::DescriptorJson)?;
            descriptor
                .validate()
                .map_err(CompositionError::DescriptorInvalid)?;
            if descriptor.name != reg.name {
                return Err(CompositionError::DescriptorNameDrift {
                    registration: reg.name.to_string(),
                    descriptor: descriptor.name.clone(),
                });
            }
            if descriptor.abi_version != identity.abi_version
                || identity.abi_version != SUPPORTED_ABI
            {
                return Err(CompositionError::UnsupportedAbi(descriptor.abi_version));
            }
            if descriptor.artifact.sha256 != encode_sha256_hex(identity.artifact_sha256) {
                return Err(CompositionError::DescriptorArtifactDrift(
                    reg.name.to_string(),
                ));
            }

            let component = Component::new(engine, reg.component_bytes)
                .map_err(|e| CompositionError::Compile(format!("{}: {e}", reg.name)))?;

            // Import inspection: no ambient WASI, exact match with declared.
            let actual_imports = collect_component_imports(reg.component_bytes);
            if actual_imports.iter().any(|name| name.starts_with("wasi:")) {
                return Err(CompositionError::AmbientImport(actual_imports));
            }
            let declared_imports: BTreeSet<String> =
                identity.imports.iter().map(|s| s.to_string()).collect();
            if actual_imports != declared_imports {
                return Err(CompositionError::ImportDrift {
                    declared: declared_imports,
                    actual: actual_imports,
                });
            }

            // `tool-api` result sink is the non-operational Session adapter.
            // Domain channels are supplied only by known ToolHost imports;
            // host-control never grants a domain channel.
            let mut supplied_channels = channels_for_domain_imports(&actual_imports);
            supplied_channels.insert(ChannelKind::Session);
            let required_channels = descriptor.required_channels();
            if !required_channels.is_subset(&supplied_channels) {
                return Err(CompositionError::MissingChannelAdapters {
                    missing: required_channels
                        .difference(&supplied_channels)
                        .copied()
                        .collect(),
                });
            }

            // Invalid JSON Schemas fail startup, not the first call.
            let params_validator = jsonschema::validator_for(&descriptor.params_schema)
                .map_err(|e| CompositionError::ParamsSchema(format!("{}: {e}", reg.name)))?;
            let result_validator = jsonschema::validator_for(&descriptor.result_schema)
                .map_err(|e| CompositionError::ResultSchema(format!("{}: {e}", reg.name)))?;

            let descriptor = Arc::new(descriptor);
            tools.insert(
                descriptor.name.clone(),
                CompiledTool {
                    descriptor,
                    component,
                    params_validator,
                    result_validator,
                },
            );
        }

        // Every expected identity must be satisfied.
        let missing: Vec<String> = expected
            .keys()
            .filter(|name| !tools.contains_key(**name))
            .map(|name| name.to_string())
            .collect();
        if !missing.is_empty() {
            return Err(CompositionError::MissingNames(missing));
        }

        Ok(Self { tools })
    }

    pub fn get(&self, name: &str) -> Option<&CompiledTool> {
        self.tools.get(name)
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &ToolDescriptor> {
        self.tools.values().map(|tool| tool.descriptor.as_ref())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute SHA-256 of `bytes` and compare with `expected`. Returns `Ok(())`
/// on match.
fn verify_sha256(bytes: &[u8], expected: [u8; 32]) -> Result<(), sha2::digest::InvalidOutputSize> {
    let digest = Sha256::digest(bytes);
    if digest.as_slice() == expected {
        Ok(())
    } else {
        // The error type is a stand-in; the caller wraps it with the tool name.
        Err(sha2::digest::InvalidOutputSize)
    }
}

/// Hex-encode a 32-byte digest (lowercase).
fn encode_sha256_hex(digest: [u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Collect the import names of a Component from its binary. Interface
/// imports are reported as `package/interface` (e.g.
/// `lattice:tool-host/host-control`). Uses `wasmparser` directly to inspect
/// the component binary without relying on wasmtime's (private) type layer.
///
/// Only top-level component imports are collected. Nested core module
/// sections and nested components are skipped — their imports are not
/// the component's own import interface.
fn collect_component_imports(bytes: &[u8]) -> BTreeSet<String> {
    let mut imports = BTreeSet::new();
    let mut depth = 0i32;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        let Ok(payload) = payload else { break };
        match payload {
            wasmparser::Payload::ComponentImportSection(reader) if depth == 0 => {
                for import in reader.into_iter() {
                    let Ok(import) = import else { continue };
                    // `import.name` is a `ComponentExternName` with a `.name: &str`
                    // field. The WIT world `import host-control;` becomes an
                    // instance import named `lattice:tool-host/host-control@0.1.0`.
                    imports.insert(import.name.name.to_string());
                }
            }
            wasmparser::Payload::ModuleSection { .. }
            | wasmparser::Payload::ComponentSection { .. } => {
                depth += 1;
            }
            wasmparser::Payload::End(_) if depth > 0 => {
                depth -= 1;
            }
            _ => {}
        }
    }
    imports
}

/// Derive the domain channel set supplied by the actual imports. Only known
/// ToolHost domain WIT imports grant a channel; `host-control` grants none.
/// #6 will add handle-per-capability domain imports.
fn channels_for_domain_imports(_imports: &BTreeSet<String>) -> BTreeSet<ChannelKind> {
    // #5: no domain imports exist yet. host-control is control-only.
    BTreeSet::new()
}

/// Parse a 64-char hex string into a `[u8; 32]` digest. Used by the generated
/// fixture to construct `ExpectedToolIdentity` from the committed
/// `expected-identity.json` candidate.
#[cfg(any(feature = "test-utils", test))]
pub(crate) fn parse_hex_digest(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let bytes = hex.as_bytes();
    for i in 0..32 {
        let hi = hex_nibble(bytes[i * 2]);
        let lo = hex_nibble(bytes[i * 2 + 1]);
        out[i] = (hi << 4) | lo;
    }
    out
}

#[cfg(any(feature = "test-utils", test))]
fn hex_nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("invalid hex digit: {c}"),
    }
}

// Re-export so `generated::noop_fixture` can construct identities.
#[cfg(feature = "test-utils")]
pub(crate) use parse_hex_digest as _parse_hex_digest;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_digest_round_trips() {
        let hex = "c452ffe16c8837746ea1088224422e5047baaf110adb7c862c454a3f4259e6ca";
        let digest = parse_hex_digest(hex);
        assert_eq!(encode_sha256_hex(digest), hex);
    }

    #[test]
    fn verify_sha256_matches_known_digest() {
        let data = b"hello";
        let expected = Sha256::digest(data);
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&expected);
        assert!(verify_sha256(data, arr).is_ok());
        arr[0] ^= 0xff;
        assert!(verify_sha256(data, arr).is_err());
    }
}
