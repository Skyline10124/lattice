// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `xtask tool-codegen` — deterministic Source Tool artifact generator.
//!
//! The Source Tool manifest (`tools/<name>/manifest.toml`) is the sole Tool
//! Descriptor authority (ADR 0004 §2). This command is the only producer of
//! the generated artifacts a built runtime embeds:
//!
//! - `generated/tool-host.wit` — the WIT world the guest Component targets.
//! - `generated/descriptor.json` — the canonical host `ToolDescriptor`.
//! - `generated/llm-projection.json` — the LLM-facing tool definition.
//! - `generated/registration.rs` — the `inventory` registration (name +
//!   `include_bytes!` of descriptor + component bytes; **cannot self-approve**).
//! - `generated/expected-identity.json` — the trusted-host composition
//!   candidate (descriptor/artifact digest, ABI, imports). Only the trusted
//!   host composition step may pin it into `host_composition.rs`.
//!
//! Determinism contract: stable field order, no timestamps, `\n` endings, one
//! trailing newline. `--check` re-runs generation and diffs instead of writing
//! (the CI gate). Two full runs are byte-identical.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use clap::Args;
use lancet_protocol::cell::{
    ArtifactIdentity, Capability, ScopeDecl, ToolDescriptor, parse_cell_ref,
};
use serde::Deserialize;

use crate::repo_root;

#[derive(Args, Debug)]
pub struct ToolCodegenArgs {
    /// Diff generated outputs against the committed files instead of writing.
    /// Exit non-zero on any drift. This is the CI determinism gate.
    #[arg(long)]
    check: bool,
}

/// Entry point for `cargo xtask tool-codegen`.
pub fn run(args: ToolCodegenArgs) -> anyhow::Result<()> {
    run_inner(args.check)
}

/// CI determinism gate: re-generate and diff against committed files.
/// Exit non-zero on any drift.
pub fn check() -> anyhow::Result<()> {
    run_inner(true)
}

/// Write generated artifacts for every manifest (the non-check path).
/// Used by `tool-build` after it pins the artifact hash.
pub fn generate() -> anyhow::Result<()> {
    run_inner(false)
}

fn run_inner(check: bool) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let manifests = discover_tool_manifests(&root)?;
    if manifests.is_empty() {
        bail!("no tool manifests found under tools/*/manifest.toml");
    }
    for manifest_path in manifests {
        let m = parse_manifest(&manifest_path)?;
        let descriptor = lower_manifest(&m)?;
        descriptor
            .validate()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let out = render_outputs(&descriptor);
        write_or_check(&manifest_path, &out, check)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Manifest model (strict: deny_unknown_fields everywhere)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Manifest {
    tool: ToolSection,
    params: SchemaSection,
    result: SchemaSection,
    #[serde(default)]
    scopes: Vec<ScopeDecl>,
    #[serde(default)]
    capabilities: Vec<CapabilitySection>,
    artifact: ArtifactSection,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolSection {
    name: String,
    version: String,
    description: String,
    #[serde(rename = "abi-version")]
    abi_version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaSection {
    /// A JSON Schema document as a multi-line TOML string.
    schema: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilitySection {
    name: String,
    /// Trust-free 3-segment CellRef `ctx:ext:scope`.
    cell: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactSection {
    sha256: String,
    toolchain: String,
}

fn parse_manifest(path: &Path) -> anyhow::Result<Manifest> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read manifest {}", path.display()))?;
    let m: Manifest = toml::from_str(&raw)
        .with_context(|| format!("failed to parse manifest {}", path.display()))?;
    Ok(m)
}

/// Lower the strict manifest into the host `ToolDescriptor`, parsing each
/// capability's CellRef via `lancet_protocol::parse_cell_ref` so the owning
/// object (not the string) is what gets serialized.
fn lower_manifest(m: &Manifest) -> anyhow::Result<ToolDescriptor> {
    let params_schema = parse_json_schema(&m.params.schema, "params")?;
    let result_schema = parse_json_schema(&m.result.schema, "result")?;

    let scopes = m.scopes.clone();

    let mut capabilities = Vec::with_capacity(m.capabilities.len());
    for c in &m.capabilities {
        let cell = parse_cell_ref(&c.cell).map_err(|e| {
            anyhow::anyhow!("capability `{}` has invalid cell `{}`: {e}", c.name, c.cell)
        })?;
        capabilities.push(Capability {
            name: c.name.clone(),
            cell,
        });
    }

    Ok(ToolDescriptor {
        name: m.tool.name.clone(),
        version: m.tool.version.clone(),
        description: m.tool.description.clone(),
        params_schema,
        result_schema,
        scopes,
        capabilities,
        artifact: ArtifactIdentity {
            sha256: m.artifact.sha256.clone(),
            toolchain: m.artifact.toolchain.clone(),
        },
        abi_version: m.tool.abi_version,
    })
}

fn parse_json_schema(s: &str, what: &str) -> anyhow::Result<serde_json::Value> {
    serde_json::from_str(s)
        .with_context(|| format!("manifest {what} schema is not valid JSON: {s}"))
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

fn discover_tool_manifests(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let tools_dir = root.join("tools");
    if !tools_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&tools_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest = entry.path().join("manifest.toml");
        if manifest.is_file() {
            found.push(manifest);
        }
    }
    // Deterministic order so generation output is stable across runs.
    found.sort();
    Ok(found)
}

/// The directory of generated artifacts for a tool: `tools/<name>/generated/`.
fn generated_dir(manifest_path: &Path) -> PathBuf {
    manifest_path
        .parent()
        .expect("manifest has a parent dir")
        .join("generated")
}

// ---------------------------------------------------------------------------
// Output rendering — all deterministic
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Outputs {
    wit: String,
    descriptor_json: String,
    llm_projection: String,
    registration_rs: String,
    identity_json: String,
}

fn render_outputs(descriptor: &ToolDescriptor) -> Outputs {
    Outputs {
        wit: render_wit(descriptor),
        descriptor_json: render_canonical_descriptor_json(descriptor),
        llm_projection: render_llm_tool_definition(descriptor),
        registration_rs: render_inventory_registration(descriptor),
        identity_json: render_expected_identity(descriptor),
    }
}

/// B.2 WIT world. The no-op imports only the authority-free `host-control`
/// interface (cancellation + structured events); it imports no workspace /
/// network / session domain operation, so "declare == authorize" cannot form
/// before #6 capability admission. The `tool-api` export's result sink is a
/// host-owned Session adapter: it only receives/validates ToolOutput.
fn render_wit(d: &ToolDescriptor) -> String {
    let mut s = String::new();
    s.push_str("// @generated by `cargo xtask tool-codegen` from the Source Tool manifest.\n");
    s.push_str("// DO NOT EDIT — re-run `cargo xtask tool-codegen`.\n");
    s.push_str(&format!("package lattice:tool-host@{};\n", d.version));
    s.push('\n');
    s.push_str("interface host-control {\n");
    s.push_str("    /// Cooperative cancellation probe (host also enforces epoch deadline).\n");
    s.push_str("    cancelled: func() -> bool;\n");
    s.push_str("    /// Structured progress event, mirrored to the CEP event stream.\n");
    s.push_str("    emit-event: func(payload: string);\n");
    s.push_str("}\n");
    s.push('\n');
    s.push_str("interface tool-api {\n");
    s.push_str("    record tool-output {\n");
    s.push_str("        /// Trust-free 3-segment CellRef; host derives trust from descriptor.\n");
    s.push_str("        result-cell: string,\n");
    s.push_str("        value-json: string,\n");
    s.push_str("    }\n");
    s.push_str("    run: func(params-json: string) -> result<tool-output, string>;\n");
    s.push_str("}\n");
    s.push('\n');
    s.push_str("world tool {\n");
    s.push_str("    import host-control;\n");
    s.push_str("    export tool-api;\n");
    s.push_str("}\n");
    s
}

/// Canonical host descriptor JSON. `serde_json::to_string_pretty` with a
/// stable serializer yields deterministic key order matching struct field
/// order; we use 2-space indent and one trailing newline.
fn render_canonical_descriptor_json(d: &ToolDescriptor) -> String {
    // Pure JSON — no comment header: the runtime embeds this via
    // `include_bytes!` and parses it with strict `serde_json`, which rejects
    // `//` comments. Provenance is carried by the `generated/` directory and
    // the sibling `.wit`/`.rs` markers.
    let json = serde_json::to_string_pretty(d).expect("descriptor serializes");
    let mut out = json;
    out.push('\n');
    out
}

/// LLM-facing tool definition (OpenAI-style `function` projection). Carries
/// only what an LLM needs: name, description, and the params JSON Schema.
fn render_llm_tool_definition(d: &ToolDescriptor) -> String {
    let projection = serde_json::json!({
        "type": "function",
        "function": {
            "name": d.name,
            "description": d.description,
            "parameters": d.params_schema,
        }
    });
    let mut out = serde_json::to_string_pretty(&projection).expect("projection serializes");
    out.push('\n');
    out
}

/// Generated `inventory` registration. It carries **only** the tool name and
/// `include_bytes!` of the descriptor + component bytes — never a self-
/// approved identity. The `inventory::submit!` makes it discoverable by the
/// runtime's embedded catalog.
fn render_inventory_registration(d: &ToolDescriptor) -> String {
    let static_ident = descriptor_static_ident(&d.name);
    let component_file = component_filename(&d.name);
    let mut s = String::new();
    s.push_str("// @generated by `cargo xtask tool-codegen`. DO NOT EDIT.\n");
    s.push_str("// Tool registration: name + embedded bytes only. A Tool crate cannot\n");
    s.push_str("// self-approve its identity; only the trusted host composition step may\n");
    s.push_str("// pin descriptor/artifact digest, ABI, and imports.\n");
    s.push_str("pub static ");
    s.push_str(&static_ident);
    s.push_str(": lancet_runtime::EmbeddedTool = lancet_runtime::EmbeddedTool {\n");
    s.push_str("    name: \"");
    s.push_str(&d.name);
    s.push_str("\",\n");
    s.push_str("    descriptor_json: include_bytes!(\"descriptor.json\"),\n");
    s.push_str("    component_bytes: include_bytes!(\"");
    s.push_str(&component_file);
    s.push_str("\"),\n");
    s.push_str("};\n");
    s.push_str("inventory::submit!(&");
    s.push_str(&static_ident);
    s.push_str(");\n");
    s
}

/// The expected-identity *candidate*. Not linked into the host; the trusted
/// host composition step selects it and privately pins it. Records the
/// descriptor digest, artifact digest, ABI version, and the import set the
/// guest is expected to require (derived from the WIT world).
fn render_expected_identity(d: &ToolDescriptor) -> String {
    let descriptor_digest = sha256_hex(render_canonical_descriptor_json(d).as_bytes());
    let identity = serde_json::json!({
        "name": d.name,
        "version": d.version,
        "abi-version": d.abi_version,
        "descriptor-sha256": descriptor_digest,
        "artifact-sha256": d.artifact.sha256,
        "toolchain": d.artifact.toolchain,
        // The no-op world imports exactly `host-control`; domain operation
        // imports arrive with #6 capability admission.
        "expected-imports": ["host-control"],
    });
    // Pure JSON — no comment header (parsed by host composition with strict
    // serde_json). The "candidate, not approved" semantics are documented here
    // and enforced by the composition step, not by this file's format.
    let mut out = serde_json::to_string_pretty(&identity).expect("identity serializes");
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Naming helpers
// ---------------------------------------------------------------------------

/// `noop.echo` -> `NOOP_ECHO` (Rust static identifier).
fn descriptor_static_ident(name: &str) -> String {
    name.to_uppercase().replace('.', "_")
}

/// `noop.echo` -> `noop_echo.component.wasm`.
fn component_filename(name: &str) -> String {
    format!("{}.component.wasm", name.replace('.', "_"))
}

/// Canonical SHA-256 over bytes, hex-lowercase. The identity candidate
/// records the real descriptor digest; the host recomputes and verifies it
/// at composition time.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Write / check
// ---------------------------------------------------------------------------

fn write_or_check(manifest_path: &Path, out: &Outputs, check: bool) -> anyhow::Result<()> {
    let gen_dir = generated_dir(manifest_path);
    let files: [(&str, &str); 5] = [
        ("tool-host.wit", &out.wit),
        ("descriptor.json", &out.descriptor_json),
        ("llm-projection.json", &out.llm_projection),
        ("registration.rs", &out.registration_rs),
        ("expected-identity.json", &out.identity_json),
    ];

    if check {
        for (name, content) in files {
            let path = gen_dir.join(name);
            let existing = std::fs::read_to_string(&path).with_context(|| {
                format!(
                    "--check: {} missing; run `cargo xtask tool-codegen`",
                    path.display()
                )
            })?;
            if existing != *content {
                bail!(
                    "codegen drift for {}: run `cargo xtask tool-codegen` and commit the result",
                    path.display()
                );
            }
        }
        return Ok(());
    }

    std::fs::create_dir_all(&gen_dir)?;
    for (name, content) in files {
        std::fs::write(gen_dir.join(name), content)?;
    }
    println!("generated artifacts for {}", manifest_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::cell::{ChannelKind, Trust};

    fn noop_manifest() -> Manifest {
        toml::from_str(
            r#"
[tool]
name = "noop.echo"
version = "0.1.0"
description = "Tracer no-op."
abi-version = 1

[params]
schema = '{"type":"object"}'

[result]
schema = '{"type":"object"}'

[[scopes]]
name = "session"
trust = "in"
channel = "session"

[[capabilities]]
name = "session.result"
cell = "alloc:none:session"

[artifact]
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
toolchain = "rustc-1.96.0"
"#,
        )
        .expect("manifest parses")
    }

    #[test]
    fn lower_manifest_parses_cellref_to_cell() {
        let m = noop_manifest();
        let d = lower_manifest(&m).expect("lower");
        assert_eq!(d.capabilities.len(), 1);
        assert_eq!(d.capabilities[0].cell.scope, "session");
        assert_eq!(d.capabilities[0].cell.reference(), "alloc:none:session");
        assert_eq!(d.scopes[0].trust, Trust::In);
        assert_eq!(d.scopes[0].channel, ChannelKind::Session);
    }

    #[test]
    fn codegen_is_byte_identical_across_runs() {
        // Determinism: two renders of the same manifest produce identical bytes.
        let m = noop_manifest();
        let d = lower_manifest(&m).unwrap();
        let a = render_outputs(&d);
        let b = render_outputs(&d);
        assert_eq!(a.wit, b.wit);
        assert_eq!(a.descriptor_json, b.descriptor_json);
        assert_eq!(a.llm_projection, b.llm_projection);
        assert_eq!(a.registration_rs, b.registration_rs);
        assert_eq!(a.identity_json, b.identity_json);
    }

    #[test]
    fn wit_has_no_wasi_imports_and_only_host_control() {
        let m = noop_manifest();
        let d = lower_manifest(&m).unwrap();
        let wit = render_wit(&d);
        assert!(!wit.contains("wasi:"), "no ambient WASI imports");
        assert!(wit.contains("import host-control;"));
        assert!(wit.contains("export tool-api;"));
        // No domain operation import before #6 admission.
        assert!(!wit.contains("workspace"));
        assert!(!wit.contains("network"));
    }

    #[test]
    fn descriptor_json_is_canonical_and_trust_free() {
        let m = noop_manifest();
        let d = lower_manifest(&m).unwrap();
        let json = render_canonical_descriptor_json(&d);
        // The owning Cell serializes ctx/ext/scope, never a trust segment.
        assert!(json.contains("\"scope\": \"session\""));
        assert!(!json.contains(":in\""));
        // Round-trips back into a ToolDescriptor with the same cell ref.
        let body = json
            .lines()
            .filter(|l| !l.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let back: ToolDescriptor = serde_json::from_str(&body).unwrap();
        assert_eq!(back.capabilities[0].cell.reference(), "alloc:none:session");
    }

    #[test]
    fn registration_cannot_self_approve() {
        let m = noop_manifest();
        let d = lower_manifest(&m).unwrap();
        let rs = render_inventory_registration(&d);
        // Only name + include_bytes! fields; no self-approval or identity
        // field on the EmbeddedTool (identity is pinned by host composition).
        assert!(rs.contains("name: \"noop.echo\""));
        assert!(rs.contains("include_bytes!(\"descriptor.json\")"));
        assert!(rs.contains("include_bytes!(\"noop_echo.component.wasm\")"));
        assert!(!rs.contains("approved:"));
        assert!(!rs.contains("identity:"));
    }

    #[test]
    fn manifest_rejects_unknown_field() {
        let bad = r#"
[tool]
name = "x"
version = "0.1.0"
description = "x"
abi-version = 1
bogus = true

[params]
schema = '{}'

[result]
schema = '{}'

[artifact]
sha256 = "00"
toolchain = "t"
"#;
        assert!(toml::from_str::<Manifest>(bad).is_err());
    }

    #[test]
    fn manifest_rejects_undeclared_scope_capability() {
        let bad = r#"
[tool]
name = "x"
version = "0.1.0"
description = "x"
abi-version = 1

[params]
schema = '{}'

[result]
schema = '{}'

[[scopes]]
name = "session"
trust = "in"
channel = "session"

[[capabilities]]
name = "ghost"
cell = "alloc:none:workspace"

[artifact]
sha256 = "00"
toolchain = "t"
"#;
        let m: Manifest = toml::from_str(bad).unwrap();
        let d = lower_manifest(&m).unwrap();
        assert!(
            d.validate().is_err(),
            "undeclared scope must fail validation"
        );
    }

    #[test]
    fn manifest_rejects_bad_cellref() {
        let bad = r#"
[tool]
name = "x"
version = "0.1.0"
description = "x"
abi-version = 1

[params]
schema = '{}'

[result]
schema = '{}'

[[scopes]]
name = "session"
trust = "in"
channel = "session"

[[capabilities]]
name = "r"
cell = "alloc:none:session:in"

[artifact]
sha256 = "00"
toolchain = "t"
"#;
        let m: Manifest = toml::from_str(bad).unwrap();
        // 4-segment guest-supplied trust must be rejected at the CellRef seam.
        assert!(lower_manifest(&m).is_err());
    }

    #[test]
    fn expected_identity_lists_host_control_only() {
        let m = noop_manifest();
        let d = lower_manifest(&m).unwrap();
        let id = render_expected_identity(&d);
        assert!(id.contains("\"expected-imports\""));
        assert!(id.contains("host-control"));
        assert!(!id.contains("workspace"));
    }
}
