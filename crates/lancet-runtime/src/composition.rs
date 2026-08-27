// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Closed composition gate — the startup verification that every embedded
//! Tool registration matches a host-pinned identity before the catalog is
//! sealed.
//!
//! The gate is fail-closed: missing, duplicate, unapproved, drifted, or
//! ambient-importing registrations are rejected, as are duplicate
//! host-pinned expected identities (checked before any registration is
//! processed). After `compose` returns, the `ClosedCatalog` is immutable
//! — there is no runtime registration API.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lancet_protocol::cell::{Cell, ChannelKind, ToolDescriptor};
use sha2::{Digest, Sha256};
use wasmtime::component::Component;

use crate::limits::ResourceFootprint;

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
    #[error("duplicate expected Tool identity: {0}")]
    DuplicateExpectedName(String),
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
    #[error("capability {name} maps to different cells across tools: {tools:?}")]
    CapabilityConflict { name: String, tools: Vec<String> },
    #[error("engine creation failed")]
    Engine(#[source] wasmtime::Error),
    #[error("linker construction failed")]
    Linker(#[source] wasmtime::Error),
    #[error("component async runtime creation failed")]
    AsyncRuntime(#[source] std::io::Error),
}

/// A compiled Tool ready for execution: descriptor, Component, pre-built
/// JSON Schema validators (invalid schemas fail startup, not the first call),
/// and the static resource footprint used to classify count-limit failures.
pub struct CompiledTool {
    pub descriptor: Arc<ToolDescriptor>,
    /// Host-pinned digest of the exact descriptor JSON accepted at composition.
    pub descriptor_identity: String,
    pub component: Component,
    pub params_validator: jsonschema::Validator,
    pub result_validator: jsonschema::Validator,
    pub(crate) footprint: ResourceFootprint,
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
        let mut expected: BTreeMap<&str, &ExpectedToolIdentity> = BTreeMap::new();
        for identity in expected_identities {
            if expected.insert(identity.name, identity).is_some() {
                return Err(CompositionError::DuplicateExpectedName(
                    identity.name.to_string(),
                ));
            }
        }
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

            // Static resource footprint: parsed once here so a count-limit
            // instantiation failure can be classified without matching
            // wasmtime's error strings.
            let footprint = component_footprint(reg.component_bytes);

            let descriptor = Arc::new(descriptor);
            tools.insert(
                descriptor.name.clone(),
                CompiledTool {
                    descriptor,
                    descriptor_identity: encode_sha256_hex(identity.descriptor_sha256),
                    component,
                    params_validator,
                    result_validator,
                    footprint,
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

        // Cross-tool capability rule, checked once over the sealed catalog.
        check_capability_conflicts(tools.values().map(|tool| tool.descriptor.as_ref()))?;

        Ok(Self { tools })
    }

    pub fn get(&self, name: &str) -> Option<&CompiledTool> {
        self.tools.get(name)
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &ToolDescriptor> {
        self.tools.values().map(|tool| tool.descriptor.as_ref())
    }

    pub fn descriptor_identities(&self) -> impl Iterator<Item = (&ToolDescriptor, &str)> {
        self.tools
            .values()
            .map(|tool| (tool.descriptor.as_ref(), tool.descriptor_identity.as_str()))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One capability name is one cell for the whole composed catalog (#28 C2).
/// Same name over the same cell is a permitted shared alias across tools;
/// same name over a different cell refuses startup, because the name would
/// otherwise mean two different authorizations depending on which tool a
/// profile enabled. Iteration is over the name-sorted catalog, so the
/// reported pair is stable.
fn check_capability_conflicts<'a>(
    descriptors: impl IntoIterator<Item = &'a ToolDescriptor>,
) -> Result<(), CompositionError> {
    let mut seen: BTreeMap<&str, (&Cell, &str)> = BTreeMap::new();
    for descriptor in descriptors {
        for capability in &descriptor.capabilities {
            match seen.get(capability.name.as_str()) {
                Some((cell, tool)) if **cell != capability.cell => {
                    return Err(CompositionError::CapabilityConflict {
                        name: capability.name.clone(),
                        tools: vec![(*tool).to_string(), descriptor.name.clone()],
                    });
                }
                Some(_) => {}
                None => {
                    seen.insert(&capability.name, (&capability.cell, &descriptor.name));
                }
            }
        }
    }
    Ok(())
}

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

/// Compute a Component's static [`ResourceFootprint`]: the core instances,
/// linear memories, and tables it allocates on instantiation. This mirrors
/// wasmtime's `bump_resource_counts` accounting (one instance plus the module's
/// defined memories/tables per instantiated core module), so a count-limit
/// instantiation failure can be classified without matching wasmtime's error
/// strings.
///
/// Uses `wasmparser` directly. Only the top-level component's core modules and
/// core instance section are considered; deeper nesting is ignored (the tool
/// pipeline emits a single flat component).
fn component_footprint(bytes: &[u8]) -> ResourceFootprint {
    use wasmparser::{Instance, Payload};

    // Per top-level core module: (defined memories, defined tables), indexed by
    // core module definition order.
    let mut modules: Vec<(usize, usize)> = Vec::new();
    // Nesting stack. `Some(idx)` marks a top-level core module body whose
    // sections we are collecting; `None` marks any other nested container
    // (nested module or nested component) whose sections we ignore.
    let mut stack: Vec<Option<usize>> = Vec::new();
    let mut footprint = ResourceFootprint::default();

    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        let Ok(payload) = payload else { break };
        match payload {
            Payload::ModuleSection { .. } => {
                if stack.is_empty() {
                    modules.push((0, 0));
                    stack.push(Some(modules.len() - 1));
                } else {
                    stack.push(None);
                }
            }
            Payload::ComponentSection { .. } => stack.push(None),
            Payload::End(_) => {
                stack.pop();
            }
            Payload::MemorySection(reader) => {
                if let [Some(idx)] = stack.as_slice() {
                    modules[*idx].0 += reader.count() as usize;
                }
            }
            Payload::TableSection(reader) => {
                if let [Some(idx)] = stack.as_slice() {
                    modules[*idx].1 += reader.count() as usize;
                }
            }
            Payload::InstanceSection(reader) if stack.is_empty() => {
                for instance in reader.into_iter() {
                    let Ok(instance) = instance else { continue };
                    if let Instance::Instantiate { module_index, .. } = instance
                        && let Some(&(memories, tables)) = modules.get(module_index as usize)
                    {
                        footprint.instances += 1;
                        footprint.memories += memories;
                        footprint.tables += tables;
                    }
                }
            }
            _ => {}
        }
    }
    footprint
}

/// Derive the domain channel set supplied by the Component's *actual* imports.
///
/// Only known Tool Host domain WIT interfaces grant a channel; `host-control`
/// grants none, because it is control-only. This is the seam that makes a
/// declared scope insufficient on its own: a Tool declaring a workspace-channel
/// scope without importing a workspace interface has no way to reach the
/// filesystem, so composition refuses it with `MissingChannelAdapters` rather
/// than admitting a scope nothing can serve.
///
/// The imports are read out of the Component binary, not out of the manifest, so
/// this reflects what the Tool actually linked. An unknown interface name grants
/// nothing — which fails closed, since the required-channel check is a subset
/// test against what is granted here.
fn channels_for_domain_imports(imports: &BTreeSet<String>) -> BTreeSet<ChannelKind> {
    let mut channels = BTreeSet::new();
    for import in imports {
        // The versioned instance-import name is `lattice:tool-host/<iface>@<ver>`.
        // Match on the interface segment so a version bump does not silently drop
        // a channel — which would turn a working Tool into a composition failure
        // for a reason unrelated to its authority.
        let interface = import
            .rsplit_once('@')
            .map_or(import.as_str(), |(name, _version)| name)
            .rsplit_once('/')
            .map_or("", |(_package, iface)| iface);
        // #30 / #31: Policy-Visible Workspace observation and Structured
        // Workspace Mutation. Either import makes the workspace channel
        // reachable; the host enforces every path decision behind them.
        if interface == "workspace-observe" || interface == "workspace-mutate" {
            channels.insert(ChannelKind::Workspace);
        } else if interface == "network-fetch" {
            channels.insert(ChannelKind::Network);
        }
    }
    channels
}

/// Parse a 64-char hex string into a `[u8; 32]` digest.
///
/// `const` because the generated host composition builds its
/// `EXPECTED_COMPONENTS` identities in a `static`: the pinned digests are
/// compile-time constants of the built runtime, not values assembled at startup.
/// A malformed digest is therefore a build failure, not a runtime panic.
pub(crate) const fn parse_hex_digest(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let bytes = hex.as_bytes();
    assert!(bytes.len() == 64, "digest must be 64 hex chars");
    let mut i = 0;
    while i < 32 {
        let hi = hex_nibble(bytes[i * 2]);
        let lo = hex_nibble(bytes[i * 2 + 1]);
        out[i] = (hi << 4) | lo;
        i += 1;
    }
    out
}

const fn hex_nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("invalid hex digit"),
    }
}

// Re-exported under a `_`-prefixed alias so the generated modules
// (`host_composition`, and `noop_fixture` under `test-utils`) can construct
// identities without widening the crate's public surface.
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

    /// The committed noop Component expands into 3 core instances (the guest
    /// module plus the wit-component shim and fixup), 1 memory, and 2 tables.
    /// This pins the footprint parser against the real artifact so drift (e.g. a
    /// wasmtime or wit-component change) is caught, and documents why the
    /// default count limits carry headroom above 1.
    #[test]
    fn noop_component_footprint_is_three_instances() {
        let bytes = include_bytes!("../../../tools/noop/generated/noop_echo.component.wasm");
        let fp = component_footprint(bytes);
        assert_eq!(
            fp,
            ResourceFootprint {
                instances: 3,
                memories: 1,
                tables: 2,
            }
        );
    }
}
