// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Composition gate tests (#27 slice C.7, Codeberg #5).
//!
//! Each negative case is exercised with synthetic registrations/identities
//! constructed directly in the test, not through the public fixture. The
//! fixture composes from fixed committed bytes only.

use lancet_runtime::test_helpers::{SyntheticTool, compose};
use lancet_runtime::{ClosedCatalog, ComponentRuntime, CompositionError};
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

/// Tracer bullet: the fixed no-op fixture composes successfully, and the
/// catalog contains exactly the noop.echo descriptor.
#[test]
fn noop_fixture_composes() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let descriptors: Vec<_> = rt.descriptors().collect();
    assert_eq!(descriptors.len(), 1, "exactly one tool in the fixture");
    assert_eq!(descriptors[0].name, "noop.echo");
    assert_eq!(descriptors[0].abi_version, 1);
}

/// A fresh synthetic tool with correct digests composes.
#[test]
fn synthetic_tool_composes() {
    let tool = SyntheticTool::from_noop_bytes("noop.echo");
    let catalog = compose(&[&tool]).expect("synthetic composes");
    assert_eq!(catalog.descriptor_names(), vec!["noop.echo"]);
}

/// Missing: an expected identity has no matching registration.
#[test]
fn gate_rejects_missing_expected_name() {
    let tool = SyntheticTool::from_noop_bytes("noop.echo");
    let ids: &'static [_] = Box::leak(vec![tool.identity].into_boxed_slice());
    let result = ClosedCatalog::compose(&engine(), std::iter::empty(), ids);
    assert!(
        matches!(result, Err(CompositionError::MissingNames(ref n)) if n == &vec!["noop.echo".to_string()]),
        "{result:?}"
    );
}

/// Duplicate registration: two registrations share a name. The expected
/// identity set carries a single pin for that name, so the
/// duplicate-registration check fires (not the duplicate-expected check,
/// which guards the host-pinned identity set and runs first).
#[test]
fn gate_rejects_duplicate_name() {
    let tool_a = SyntheticTool::from_noop_bytes("noop.echo");
    let tool_b = SyntheticTool::from_noop_bytes("noop.echo");
    // One expected identity for "noop.echo"; two registrations claim it, so
    // the expected set is unique and the registration duplicate fires.
    let ids: &'static [_] = Box::leak(vec![tool_a.identity].into_boxed_slice());
    let reg_a: &'static _ = Box::leak(Box::new(tool_a.registration));
    let reg_b: &'static _ = Box::leak(Box::new(tool_b.registration));
    let result = ClosedCatalog::compose(&engine(), vec![reg_a, reg_b], ids);
    assert!(
        matches!(result, Err(CompositionError::DuplicateName(ref n)) if n == "noop.echo"),
        "{result:?}"
    );
}

/// Duplicate expected identity: two host-pinned identities share a name with
/// different digests. The gate must fail closed rather than silently letting
/// the later entry overwrite the earlier pin (no last-wins). The uniqueness
/// check runs before registration processing.
#[test]
fn gate_rejects_duplicate_expected_name() {
    let tool = SyntheticTool::from_noop_bytes("noop.echo");
    // A second identity with the same name but a different descriptor digest,
    // proving the gate does not silently adopt a last-wins pin.
    let mut second = tool.identity;
    second.descriptor_sha256[0] ^= 0xff;

    let ids: &'static [_] = Box::leak(vec![tool.identity, second].into_boxed_slice());
    let reg: &'static _ = Box::leak(Box::new(tool.registration));
    let result = ClosedCatalog::compose(&engine(), std::iter::once(reg), ids);
    assert!(
        matches!(result, Err(CompositionError::DuplicateExpectedName(ref n)) if n == "noop.echo"),
        "{result:?}"
    );
}

/// Unapproved: a registration whose name is not in the expected set.
#[test]
fn gate_rejects_unapproved_name() {
    let tool = SyntheticTool::from_noop_bytes("rogue.tool");
    let reg: &'static _ = Box::leak(Box::new(tool.registration));
    let result = ClosedCatalog::compose(&engine(), std::iter::once(reg), &[]);
    assert!(
        matches!(result, Err(CompositionError::UnapprovedName(ref n)) if n == "rogue.tool"),
        "{result:?}"
    );
}

/// Descriptor digest drift: the host-pinned digest doesn't match the bytes.
#[test]
fn gate_rejects_descriptor_digest_drift() {
    let mut tool = SyntheticTool::from_noop_bytes("noop.echo");
    tool.with_wrong_descriptor_digest();
    let result = compose(&[&tool]);
    assert!(
        matches!(result, Err(CompositionError::DescriptorDrift(_))),
        "{result:?}"
    );
}

/// Artifact hash mismatch: the host-pinned artifact digest doesn't match.
#[test]
fn gate_rejects_artifact_hash_mismatch() {
    let mut tool = SyntheticTool::from_noop_bytes("noop.echo");
    tool.with_wrong_artifact_digest();
    let result = compose(&[&tool]);
    assert!(
        matches!(result, Err(CompositionError::ArtifactHashMismatch(_))),
        "{result:?}"
    );
}

/// Import drift: the declared imports don't match the actual.
#[test]
fn gate_rejects_import_drift() {
    let mut tool = SyntheticTool::from_noop_bytes("noop.echo");
    tool.with_wrong_imports(&["lattice:tool-host/some-other@0.1.0"]);
    let result = compose(&[&tool]);
    assert!(
        matches!(result, Err(CompositionError::ImportDrift { .. })),
        "{result:?}"
    );
}

/// Unsupported ABI: the identity declares an ABI version the host doesn't
/// support.
#[test]
fn gate_rejects_unsupported_abi() {
    let mut tool = SyntheticTool::from_noop_bytes("noop.echo");
    tool.with_unsupported_abi();
    let result = compose(&[&tool]);
    // The gate reports the descriptor's abi_version (1, from the noop
    // descriptor) because descriptor.abi_version != identity.abi_version.
    assert!(
        matches!(result, Err(CompositionError::UnsupportedAbi(_))),
        "{result:?}"
    );
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

/// The same name over the same cell is a permitted shared alias.
#[test]
fn gate_accepts_a_capability_name_shared_over_one_cell() {
    let first = tool_with_capability("a.tool", "session.result", "alloc");
    let second = tool_with_capability("b.tool", "session.result", "alloc");
    let catalog = compose(&[&first, &second]).expect("shared alias composes");
    assert_eq!(catalog.descriptor_names(), vec!["a.tool", "b.tool"]);
}

/// Closed catalog has no mutation API: the descriptors iterator is
/// read-only and the catalog is immutable after compose.
#[test]
fn closed_catalog_is_immutable_after_compose() {
    let tool = SyntheticTool::from_noop_bytes("noop.echo");
    let catalog = compose(&[&tool]).expect("composes");
    let names = catalog.descriptor_names();
    assert_eq!(names, vec!["noop.echo"]);
}
