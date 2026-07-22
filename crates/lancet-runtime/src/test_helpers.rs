// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Test helpers for the composition gate (`test-utils` feature).
//!
//! These allow integration tests to construct synthetic registrations and
//! expected identities to exercise the gate's negative cases. Production
//! code never uses these — the public constructor only reads `inventory` +
//! the generated `EXPECTED_COMPONENTS`.

#![cfg(feature = "test-utils")]

use lancet_protocol::cell::{
    ArtifactIdentity, Capability, Cell, ChannelKind, CtxOp, ExtImpact, ScopeDecl, ToolDescriptor,
    Trust,
};
use sha2::{Digest, Sha256};

use crate::composition::{ClosedCatalog, CompositionError, EmbeddedTool, ExpectedToolIdentity};

/// A synthetic registration + expected identity pair. Tests mutate fields
/// to create drift.
pub struct SyntheticTool {
    pub registration: EmbeddedTool,
    pub identity: ExpectedToolIdentity,
}

/// Mutations on `SyntheticTool.identity` to create drift for negative tests.
impl SyntheticTool {
    /// Set the descriptor digest to a wrong value → `DescriptorDrift`.
    pub fn with_wrong_descriptor_digest(&mut self) {
        self.identity.descriptor_sha256[0] ^= 0xff;
    }

    /// Set the artifact digest to a wrong value → `ArtifactHashMismatch`.
    pub fn with_wrong_artifact_digest(&mut self) {
        self.identity.artifact_sha256[0] ^= 0xff;
    }

    /// Set the ABI version to an unsupported value → `UnsupportedAbi`.
    pub fn with_unsupported_abi(&mut self) {
        self.identity.abi_version = 99;
    }

    /// Set the imports to a wrong set → `ImportDrift`.
    pub fn with_wrong_imports(&mut self, imports: &'static [&'static str]) {
        self.identity.imports = imports;
    }
}

/// Mutations on `SyntheticTool.registration` to create drift for negative tests.
impl SyntheticTool {
    /// Build a SyntheticTool from a descriptor JSON and component bytes.
    /// The identity digests are computed from the actual bytes, so a fresh
    /// SyntheticTool always composes. Tests then mutate one field to
    /// create the drift they want to assert.
    pub fn new(
        name: &'static str,
        descriptor_json: &'static [u8],
        component_bytes: &'static [u8],
        abi_version: u32,
        imports: &'static [&'static str],
    ) -> Self {
        let descriptor_digest = Sha256::digest(descriptor_json);
        let artifact_digest = Sha256::digest(component_bytes);

        let registration = EmbeddedTool {
            name,
            descriptor_json,
            component_bytes,
        };
        let identity = ExpectedToolIdentity {
            name,
            descriptor_sha256: digest_to_array(descriptor_digest),
            artifact_sha256: digest_to_array(artifact_digest),
            abi_version,
            imports,
        };

        Self {
            registration,
            identity,
        }
    }

    /// Build a SyntheticTool from the committed noop fixture bytes but with
    /// a custom name (for duplicate/missing tests).
    pub fn from_noop_bytes(name: &'static str) -> Self {
        let descriptor_json = include_bytes!("../../../tools/noop/generated/descriptor.json");
        let component_bytes =
            include_bytes!("../../../tools/noop/generated/noop_echo.component.wasm");
        Self::new(
            name,
            descriptor_json,
            component_bytes,
            1,
            &["lattice:tool-host/host-control@0.1.0"],
        )
    }
}

/// Compose a catalog from synthetic tools. Returns the catalog handle or
/// the composition error.
pub fn compose(tools: &[&SyntheticTool]) -> Result<ClosedCatalogHandle, CompositionError> {
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    let engine = wasmtime::Engine::new(&c).map_err(CompositionError::Engine)?;

    let regs: Vec<&'static EmbeddedTool> = tools.iter().map(|t| leak(t.registration)).collect();
    let ids: Vec<ExpectedToolIdentity> = tools.iter().map(|t| t.identity).collect();
    let ids: &'static [ExpectedToolIdentity] = leak_vec(ids);

    let catalog = ClosedCatalog::compose(&engine, regs, ids)?;
    Ok(ClosedCatalogHandle {
        _engine: engine,
        catalog,
    })
}

/// A handle that keeps the engine alive for the catalog.
#[derive(Debug)]
pub struct ClosedCatalogHandle {
    _engine: wasmtime::Engine,
    catalog: ClosedCatalog,
}

impl ClosedCatalogHandle {
    pub fn descriptor_names(&self) -> Vec<String> {
        self.catalog.descriptors().map(|d| d.name.clone()).collect()
    }
}

/// Build a minimal valid descriptor JSON for tests.
pub fn noop_descriptor_json(name: &str, abi_version: u32) -> Vec<u8> {
    let desc = ToolDescriptor {
        name: name.to_string(),
        version: "0.1.0".into(),
        description: "synthetic".into(),
        params_schema: serde_json::json!({
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"],
            "additionalProperties": false,
        }),
        result_schema: serde_json::json!({
            "type": "object",
            "properties": {"echoed": {"type": "string"}},
            "required": ["echoed"],
            "additionalProperties": false,
        }),
        scopes: vec![ScopeDecl {
            name: "session".into(),
            trust: Trust::In,
            channel: ChannelKind::Session,
        }],
        capabilities: vec![Capability {
            name: "session.result".into(),
            cell: Cell {
                ctx_op: CtxOp::Alloc,
                ext_impact: ExtImpact::None,
                scope: "session".into(),
            },
        }],
        artifact: ArtifactIdentity {
            sha256: "0".repeat(64),
            toolchain: "test".into(),
        },
        abi_version,
    };
    serde_json::to_vec(&desc).expect("descriptor serializes")
}

// ---------------------------------------------------------------------------
// Leaking helpers (test-only: synthetic tools are 'static for the gate API)
// ---------------------------------------------------------------------------

fn leak_vec<T>(value: Vec<T>) -> &'static [T] {
    Box::leak(value.into_boxed_slice())
}

fn leak<T>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn digest_to_array(digest: impl AsRef<[u8]>) -> [u8; 32] {
    let d = digest.as_ref();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d[..32]);
    out
}
