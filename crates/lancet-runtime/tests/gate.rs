// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Composition gate tests (#27 slice C.7, Codeberg #5).
//!
//! Each negative case is exercised with synthetic registrations/identities
//! constructed directly in the test, not through the public fixture. The
//! fixture composes from fixed committed bytes only.

use lancet_runtime::test_helpers::{SyntheticTool, compose};
use lancet_runtime::{ClosedCatalog, CompositionError};
use sha2::{Digest, Sha256};

fn engine() -> wasmtime::Engine {
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    wasmtime::Engine::new(&c).unwrap()
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[test]
fn gate_rejects_invalid_catalog_names() {
    for (name, tool_name, registration_count, identity_count, digest_xor, accepts_error) in [
        // An expected identity without a registration is missing.
        (
            "missing_expected_name",
            "noop.echo",
            0,
            1,
            0,
            (|error: &CompositionError| matches!(error, CompositionError::MissingNames(n) if n == &vec!["noop.echo".to_string()]))
                as fn(&CompositionError) -> bool,
        ),
        // A single expected pin ensures the duplicate-registration check fires, not duplicate-expected.
        (
            "duplicate_name",
            "noop.echo",
            2,
            1,
            0,
            |error: &CompositionError| matches!(error, CompositionError::DuplicateName(n) if n == "noop.echo"),
        ),
        // Same name with different digests must not silently adopt a last-wins pin; checked before registrations.
        (
            "duplicate_expected_name",
            "noop.echo",
            1,
            2,
            0xff,
            |error: &CompositionError| matches!(error, CompositionError::DuplicateExpectedName(n) if n == "noop.echo"),
        ),
        // A registration outside the expected set is unapproved.
        (
            "unapproved_name",
            "rogue.tool",
            1,
            0,
            0,
            |error: &CompositionError| matches!(error, CompositionError::UnapprovedName(n) if n == "rogue.tool"),
        ),
    ] {
        let tool = SyntheticTool::from_noop_bytes(tool_name);
        let mut identities = vec![tool.identity; identity_count];
        if let Some(second) = identities.get_mut(1) {
            second.descriptor_sha256[0] ^= digest_xor;
        }
        let ids: &'static [_] = Box::leak(identities.into_boxed_slice());
        let registrations: Vec<&'static _> = (0..registration_count)
            .map(|_| {
                let tool = SyntheticTool::from_noop_bytes(tool_name);
                &*Box::leak(Box::new(tool.registration))
            })
            .collect();
        let result = ClosedCatalog::compose(&engine(), registrations, ids);
        assert!(
            matches!(&result, Err(error) if accepts_error(error)),
            "{name}: {result:?}"
        );
    }
}

#[test]
fn gate_rejects_tool_identity_drift() {
    for (name, mutate_identity, accepts_error) in [
        // The host-pinned descriptor digest must match the bytes.
        (
            "descriptor_digest_drift",
            SyntheticTool::with_wrong_descriptor_digest as fn(&mut SyntheticTool),
            (|error: &CompositionError| matches!(error, CompositionError::DescriptorDrift(_)))
                as fn(&CompositionError) -> bool,
        ),
        // The host-pinned artifact digest must match the bytes.
        (
            "artifact_hash_mismatch",
            SyntheticTool::with_wrong_artifact_digest,
            |error: &CompositionError| matches!(error, CompositionError::ArtifactHashMismatch(_)),
        ),
        // Declared imports must match actual imports.
        (
            "import_drift",
            |tool: &mut SyntheticTool| {
                tool.with_wrong_imports(&["lattice:tool-host/some-other@0.1.0"])
            },
            |error: &CompositionError| matches!(error, CompositionError::ImportDrift { .. }),
        ),
        // Identity ABI becomes 99 but descriptor ABI stays 1; the gate reports the descriptor ABI.
        (
            "unsupported_abi",
            SyntheticTool::with_unsupported_abi,
            |error: &CompositionError| matches!(error, CompositionError::UnsupportedAbi(_)),
        ),
    ] {
        let mut tool = SyntheticTool::from_noop_bytes("noop.echo");
        mutate_identity(&mut tool);
        let result = compose(&[&tool]);
        assert!(
            matches!(&result, Err(error) if accepts_error(error)),
            "{name}: {result:?}"
        );
    }
}

/// Ambient WASI import — clean test: the component only imports
/// `wasi:filesystem/types` and nothing else. The descriptor is crafted so
/// all prior gate checks pass, and the WASI import check fires.
#[test]
fn gate_rejects_ambient_wasi_import() {
    let wat = r#"
(component
  (import "wasi:filesystem/types@0.2.0" (instance))
)
"#;
    let wasm = wat::parse_str(wat).expect("parse wat");
    let wasm: &'static [u8] = Box::leak(wasm.into_boxed_slice());

    // Build a descriptor with artifact.sha256 matching the actual wasm hash.
    let artifact_hash = hex_sha256(wasm);
    let mut descriptor_json = lancet_runtime::test_helpers::noop_descriptor_json("wasi.tool", 1);
    let mut value: serde_json::Value = serde_json::from_slice(&descriptor_json).unwrap();
    value["artifact"]["sha256"] = serde_json::json!(artifact_hash);
    descriptor_json = serde_json::to_vec(&value).unwrap();
    let descriptor_json: &'static [u8] = Box::leak(descriptor_json.into_boxed_slice());

    let tool = SyntheticTool::new("wasi.tool", descriptor_json, wasm, 1, &[]);
    let result = compose(&[&tool]);
    assert!(
        matches!(result, Err(CompositionError::AmbientImport(ref imports)) if imports.iter().any(|n| n.starts_with("wasi:"))),
        "{result:?}"
    );
}

/// Missing channel adapter: the descriptor requires a Workspace channel but
/// no domain import supplies it (and host-control grants none).
#[test]
fn gate_rejects_missing_channel_adapter() {
    // Build a descriptor that requires a Workspace channel scope.
    let mut descriptor_json = lancet_runtime::test_helpers::noop_descriptor_json("ws.tool", 1);
    let mut value: serde_json::Value = serde_json::from_slice(&descriptor_json).unwrap();
    // Replace the session scope with a workspace scope.
    value["scopes"] = serde_json::json!([
        {"name": "workspace", "trust": "ex", "channel": "workspace"}
    ]);
    value["capabilities"] = serde_json::json!([
        {"name": "ws.read", "cell": {"ctx_op": "read", "ext_impact": "read", "scope": "workspace"}}
    ]);
    // Fix: ctx_op must be alloc/free/mutate, not read.
    value["capabilities"] = serde_json::json!([
        {"name": "ws.read", "cell": {"ctx_op": "alloc", "ext_impact": "read", "scope": "workspace"}}
    ]);
    descriptor_json = serde_json::to_vec(&value).unwrap();
    let descriptor_json: &'static [u8] = Box::leak(descriptor_json.into_boxed_slice());

    // Use the noop component bytes (imports host-control only).
    let component_bytes = include_bytes!("../../../tools/noop/generated/noop_echo.component.wasm");
    let component_bytes: &'static [u8] = component_bytes;

    // Patch the descriptor's artifact sha256 to match the noop component.
    let artifact_hash = hex_sha256(component_bytes);
    let mut value: serde_json::Value = serde_json::from_slice(descriptor_json).unwrap();
    value["artifact"]["sha256"] = serde_json::json!(artifact_hash);
    let descriptor_json = serde_json::to_vec(&value).unwrap();
    let descriptor_json: &'static [u8] = Box::leak(descriptor_json.into_boxed_slice());

    let tool = SyntheticTool::new(
        "ws.tool",
        descriptor_json,
        component_bytes,
        1,
        &["lattice:tool-host/host-control@0.1.0"],
    );
    let result = compose(&[&tool]);
    assert!(
        matches!(result, Err(CompositionError::MissingChannelAdapters { ref missing }) if missing.iter().any(|c| matches!(c, lancet_protocol::cell::ChannelKind::Workspace))),
        "{result:?}"
    );
}

/// A synthetic tool over the committed noop component bytes whose descriptor
/// declares exactly one capability, on the `session` scope.
fn tool_with_capability(name: &'static str, capability: &str, ctx_op: &str) -> SyntheticTool {
    let component_bytes: &'static [u8] =
        include_bytes!("../../../tools/noop/generated/noop_echo.component.wasm");
    let mut value: serde_json::Value =
        serde_json::from_slice(&lancet_runtime::test_helpers::noop_descriptor_json(name, 1))
            .expect("descriptor parses");
    value["capabilities"] = serde_json::json!([
        {"name": capability, "cell": {"ctx_op": ctx_op, "ext_impact": "none", "scope": "session"}}
    ]);
    // The gate compares the descriptor's artifact hash with the pinned one.
    value["artifact"]["sha256"] = serde_json::json!(hex_sha256(component_bytes));
    let descriptor_json: &'static [u8] = Box::leak(
        serde_json::to_vec(&value)
            .expect("descriptor serializes")
            .into_boxed_slice(),
    );
    SyntheticTool::new(
        name,
        descriptor_json,
        component_bytes,
        1,
        &["lattice:tool-host/host-control@0.1.0"],
    )
}

/// C6: one capability name pointing at two different cells refuses startup —
/// the name would otherwise mean two different authorizations depending on
/// which tool a profile enabled.
#[test]
fn gate_rejects_capability_name_over_two_cells() {
    let alloc = tool_with_capability("a.tool", "session.result", "alloc");
    let mutate = tool_with_capability("b.tool", "session.result", "mutate");
    let result = compose(&[&alloc, &mutate]);
    assert!(
        matches!(
            result,
            Err(CompositionError::CapabilityConflict { ref name, ref tools })
                if name == "session.result" && tools == &["a.tool".to_string(), "b.tool".to_string()]
        ),
        "{result:?}"
    );
}
