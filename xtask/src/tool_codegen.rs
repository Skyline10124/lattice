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
//! - `generated/render.json` — the presentation-intent projection, emitted only
//!   when the manifest declares a `[render]` section. It is deliberately outside
//!   the descriptor identity chain (see [`render`]).
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

pub(crate) mod render;

use render::{RenderSchemas, RenderSection};

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
        let out = render_outputs(&descriptor, &m)
            .with_context(|| format!("manifest {}", manifest_path.display()))?;
        write_or_check(&manifest_path, &out, check)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Manifest model (strict: deny_unknown_fields everywhere)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    tool: ToolSection,
    params: SchemaSection,
    result: SchemaSection,
    #[serde(default)]
    scopes: Vec<ScopeDecl>,
    #[serde(default)]
    capabilities: Vec<CapabilitySection>,
    artifact: ArtifactSection,
    /// Presentation intent. Optional: a Tool without it is still renderable,
    /// and it never reaches `lower_manifest`, so it cannot affect the
    /// descriptor identity chain.
    #[serde(default)]
    render: Option<RenderSection>,
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

pub(crate) fn discover_tool_manifests(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
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
    /// `Some` only when the manifest declares `[render]`. `None` means the file
    /// must **not** exist, which the write path enforces by deleting a stale one
    /// and `--check` enforces by failing on it.
    render_json: Option<String>,
}

/// Render every artifact. The `[render]` section is validated against the
/// manifest's own schemas here rather than in `lower_manifest`, keeping it off
/// the descriptor path entirely: `descriptor` is already built and is only read.
fn render_outputs(descriptor: &ToolDescriptor, m: &Manifest) -> anyhow::Result<Outputs> {
    let render_json = match &m.render {
        Some(section) => {
            let schemas = RenderSchemas {
                params: &descriptor.params_schema,
                result: &descriptor.result_schema,
            };
            render::validate_render(section, &schemas)?;
            Some(render::render_projection(
                &descriptor.name,
                section,
                &schemas,
            )?)
        }
        None => None,
    };
    Ok(Outputs {
        wit: render_wit(descriptor),
        descriptor_json: render_canonical_descriptor_json(descriptor),
        llm_projection: render_llm_tool_definition(descriptor),
        registration_rs: render_inventory_registration(descriptor),
        identity_json: render_expected_identity(descriptor),
        render_json,
    })
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
pub(crate) fn component_filename(name: &str) -> String {
    format!("{}.component.wasm", name.replace('.', "_"))
}

/// Canonical SHA-256 over bytes, hex-lowercase. The identity candidate
/// records the real descriptor digest; the host recomputes and verifies it
/// at composition time.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
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

/// Every artifact filename this generator owns, paired with the content it must
/// have — or `None` when the artifact must not exist. A conditional artifact
/// needs the absent case to be part of the gate: a check loop that only compares
/// files it expects to *find* cannot see an orphan left behind when a manifest
/// drops its `[render]` section, and that orphan would keep feeding annotations
/// the manifest no longer declares.
fn expected_files(out: &Outputs) -> Vec<(&'static str, Option<&str>)> {
    vec![
        ("tool-host.wit", Some(out.wit.as_str())),
        ("descriptor.json", Some(out.descriptor_json.as_str())),
        ("llm-projection.json", Some(out.llm_projection.as_str())),
        ("registration.rs", Some(out.registration_rs.as_str())),
        ("expected-identity.json", Some(out.identity_json.as_str())),
        ("render.json", out.render_json.as_deref()),
    ]
}

fn write_or_check(manifest_path: &Path, out: &Outputs, check: bool) -> anyhow::Result<()> {
    let gen_dir = generated_dir(manifest_path);
    let files = expected_files(out);

    if check {
        for (name, content) in files {
            let path = gen_dir.join(name);
            let Some(content) = content else {
                if path.exists() {
                    bail!(
                        "stale {}: the manifest declares no `[render]` section; run \
                         `cargo xtask tool-codegen` and commit the deletion",
                        path.display()
                    );
                }
                continue;
            };
            // Only ENOENT means "not generated yet"; a permission or encoding
            // failure would otherwise be reported as absence and send the author
            // to re-run codegen, which changes nothing.
            let existing = std::fs::read_to_string(&path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::anyhow!(
                        "--check: {} missing; run `cargo xtask tool-codegen`",
                        path.display()
                    )
                } else {
                    anyhow::Error::new(e)
                        .context(format!("--check: cannot read {}", path.display()))
                }
            })?;
            if existing != content {
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
        let path = gen_dir.join(name);
        match content {
            Some(content) => std::fs::write(&path, content)?,
            // Removing an artifact that must no longer exist is part of
            // generating: leaving it would make the committed tree disagree with
            // the manifest while every gate still passed.
            None => match std::fs::remove_file(&path) {
                Ok(()) => println!("removed stale {}", path.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("failed to remove {}", path.display()));
                }
            },
        }
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
    use lancet_protocol::cell::{ChannelKind, DescriptorError, Trust};

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
        let a = render_outputs(&d, &m).unwrap();
        let b = render_outputs(&d, &m).unwrap();
        assert_eq!(a.wit, b.wit);
        assert_eq!(a.descriptor_json, b.descriptor_json);
        assert_eq!(a.llm_projection, b.llm_projection);
        assert_eq!(a.registration_rs, b.registration_rs);
        assert_eq!(a.identity_json, b.identity_json);
        assert_eq!(a.render_json, b.render_json);
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

    /// A typo'd top-level section must not be dropped on the floor: the root
    /// is as strict as every sub-table, or a whole `[[capabilities]]` block
    /// misspelled once would compose as an empty capability set.
    #[test]
    fn manifest_rejects_unknown_top_level_section() {
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

[[capabilties]]
name = "session.result"
cell = "alloc:none:session"

[artifact]
sha256 = "00"
toolchain = "t"
"#;
        let err = toml::from_str::<Manifest>(bad).expect_err("unknown root section is rejected");
        assert!(
            err.to_string().contains("capabilties"),
            "the error must name the typo'd section: {err}"
        );
    }

    #[test]
    fn manifest_rejects_unknown_top_level_field() {
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

[artifact]
sha256 = "00"
toolchain = "t"

[bogus]
anything = true
"#;
        assert!(
            toml::from_str::<Manifest>(bad).is_err(),
            "an unknown top-level table must be rejected, not silently ignored"
        );
    }

    #[test]
    fn misspelled_capabilities_table_is_rejected_not_silently_empty() {
        // Fail-open regression: `capabilties` (typo) used to deserialize into a
        // Manifest whose real `capabilities` defaulted to EMPTY, so artifacts
        // were generated for a tool with no declared capabilities at all.
        let typo = r#"
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

[[capabilties]]
name = "session.result"
cell = "alloc:none:session"

[artifact]
sha256 = "00"
toolchain = "t"
"#;
        let err = toml::from_str::<Manifest>(typo)
            .expect_err("misspelled `capabilities` must fail closed");
        assert!(
            err.to_string().contains("capabilties"),
            "error should name the offending key, got: {err}"
        );
    }

    #[test]
    fn manifest_rejects_undeclared_scope_capability() {
        // The name is grammar-conforming on purpose: `validate` checks the
        // capability-name grammar first, so a single-segment fixture would
        // fail on the wrong rule and prove nothing about scope declaration.
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
name = "ghost.read"
cell = "alloc:none:workspace"

[artifact]
sha256 = "00"
toolchain = "t"
"#;
        let m: Manifest = toml::from_str(bad).unwrap();
        let d = lower_manifest(&m).unwrap();
        assert_eq!(
            d.validate()
                .expect_err("undeclared scope must fail validation"),
            DescriptorError::UndeclaredScope {
                capability: "ghost.read".into(),
                scope: "workspace".into(),
            },
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

    // -----------------------------------------------------------------------
    // `[render]` wiring (#113). Format-level tests live in
    // `tool_codegen/render_tests.rs`; these cover the generator seam.
    // -----------------------------------------------------------------------

    /// A manifest fixture whose schemas are rich enough to annotate, in two
    /// variants that differ *only* by the presence of `[render]`.
    fn manifest_pair() -> (Manifest, Manifest) {
        const BASE: &str = r#"
[tool]
name = "noop.echo"
version = "0.1.0"
description = "Tracer no-op."
abi-version = 1

[params]
schema = '{"type":"object","properties":{"message":{"type":"string"}}}'

[result]
schema = '{"type":"object","properties":{"echoed":{"type":"string"}}}'

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
"#;
        const RENDER: &str = r#"
[[render.annotations]]
role = "summary"
path = "result.echoed"
"#;
        (
            toml::from_str(BASE).expect("base manifest parses"),
            toml::from_str(&format!("{BASE}{RENDER}")).expect("render manifest parses"),
        )
    }

    /// AC 3, the load-bearing one: adding `[render]` must leave every artifact
    /// on the identity chain byte-identical. Comparing the artifacts themselves
    /// (not a derived claim about them) is what makes this a proof — the digest
    /// is recomputed from `descriptor.json`, so an equal descriptor is an equal
    /// `descriptor-sha256` by construction, and the assertion below pins that too.
    #[test]
    fn render_section_does_not_perturb_the_identity_chain() {
        let (plain, with_render) = manifest_pair();
        let a = render_outputs(&lower_manifest(&plain).unwrap(), &plain).unwrap();
        let b = render_outputs(&lower_manifest(&with_render).unwrap(), &with_render).unwrap();

        assert_eq!(
            a.descriptor_json, b.descriptor_json,
            "`[render]` must not change descriptor.json"
        );
        assert_eq!(
            sha256_hex(a.descriptor_json.as_bytes()),
            sha256_hex(b.descriptor_json.as_bytes()),
            "`[render]` must not change descriptor-sha256"
        );
        assert_eq!(
            a.identity_json, b.identity_json,
            "`[render]` must not change expected-identity.json"
        );
        assert_eq!(a.wit, b.wit, "`[render]` must not change the WIT world");
        assert_eq!(
            a.registration_rs, b.registration_rs,
            "`[render]` must not change registration.rs"
        );
        assert_eq!(
            a.llm_projection, b.llm_projection,
            "`[render]` must not change the LLM projection"
        );

        // The Component bytes are produced by `tool-build` from the guest crate
        // and the WIT world; neither reads `[render]`. An unchanged WIT world is
        // the whole of that dependency, and the artifact digest travels inside
        // the descriptor, which is asserted equal above.
        assert!(
            !a.descriptor_json.contains("render") && !a.identity_json.contains("render"),
            "no render vocabulary may appear on the identity chain"
        );

        // And the projection itself only exists in the second variant.
        assert!(a.render_json.is_none());
        assert!(b.render_json.is_some());

        // Comparing in-memory `Outputs` fields alone would not see a
        // *filename-level* collision: if the render projection were ever routed
        // to an identity artifact's filename, every assertion above would still
        // hold while the on-disk descriptor was overwritten. So assert the
        // property where it actually matters — on disk, through the real write
        // path — and pin the artifact-name partition that makes it hold.
        let names: Vec<&str> = expected_files(&b).iter().map(|(n, _)| *n).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            names.len(),
            unique.len(),
            "each generated artifact must own a distinct filename: {names:?}"
        );

        let tmp = tempfile::tempdir().unwrap();
        let manifest_path = tmp.path().join("manifest.toml");
        std::fs::write(&manifest_path, "").unwrap();
        let gen_dir = tmp.path().join("generated");

        write_or_check(&manifest_path, &a, false).expect("write without [render]");
        let plain_bytes: Vec<(String, Vec<u8>)> = read_generated(&gen_dir);
        write_or_check(&manifest_path, &b, false).expect("write with [render]");
        let render_bytes: Vec<(String, Vec<u8>)> = read_generated(&gen_dir);

        for (name, bytes) in &plain_bytes {
            let after = render_bytes
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, b)| b)
                .unwrap_or_else(|| panic!("{name} disappeared when [render] was added"));
            assert_eq!(
                bytes, after,
                "adding `[render]` changed {name} on disk; every pre-existing artifact must be \
                 byte-identical"
            );
        }
        // The only on-disk difference is the new projection.
        let added: Vec<&String> = render_bytes
            .iter()
            .map(|(n, _)| n)
            .filter(|n| !plain_bytes.iter().any(|(p, _)| p == *n))
            .collect();
        assert_eq!(added, vec!["render.json"], "only render.json may appear");
    }

    /// Every file in a `generated/` directory, sorted, as raw bytes.
    fn read_generated(gen_dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(gen_dir)
            .expect("generated dir exists")
            .filter_map(Result::ok)
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).expect("artifact readable"),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// AC 5: absence is legal, not an error, and emits no file.
    #[test]
    fn manifest_without_render_is_legal_and_emits_no_projection() {
        let (plain, _) = manifest_pair();
        let out = render_outputs(&lower_manifest(&plain).unwrap(), &plain)
            .expect("a manifest without [render] must generate cleanly");
        assert!(out.render_json.is_none());
        assert!(
            expected_files(&out)
                .iter()
                .any(|(name, content)| *name == "render.json" && content.is_none()),
            "render.json must be listed as expected-absent so the gate can see an orphan"
        );
    }

    /// AC 2/5 interaction: the gate must fail on a `render.json` the manifest no
    /// longer declares. Without this, deleting `[render]` leaves an orphan that
    /// both `tool-codegen` and `cargo xtask ci` report as clean.
    #[test]
    fn check_fails_on_a_stale_render_projection() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_path = tmp.path().join("manifest.toml");
        std::fs::write(&manifest_path, "").unwrap();
        let gen_dir = tmp.path().join("generated");
        std::fs::create_dir_all(&gen_dir).unwrap();

        let (plain, with_render) = manifest_pair();
        let with = render_outputs(&lower_manifest(&with_render).unwrap(), &with_render).unwrap();
        write_or_check(&manifest_path, &with, false).expect("write with [render]");
        assert!(gen_dir.join("render.json").is_file());

        let without = render_outputs(&lower_manifest(&plain).unwrap(), &plain).unwrap();
        let err = write_or_check(&manifest_path, &without, true)
            .expect_err("--check must fail while a stale render.json exists");
        assert!(
            err.to_string().contains("stale") && err.to_string().contains("render.json"),
            "error must name the stale artifact, got: {err}"
        );

        // The write path removes it, and only then does --check pass.
        write_or_check(&manifest_path, &without, false).expect("write without [render]");
        assert!(!gen_dir.join("render.json").exists());
        write_or_check(&manifest_path, &without, true).expect("--check clean after removal");
    }

    /// The committed `tools/noop` manifest must carry the sample section (AC 6)
    /// and its committed projection must be exactly what codegen produces.
    #[test]
    fn committed_noop_manifest_carries_a_render_sample() {
        let root = repo_root::repo_root().unwrap();
        let manifest_path = root.join("tools/noop/manifest.toml");
        let m = parse_manifest(&manifest_path).expect("noop manifest parses");
        assert!(
            m.render.is_some(),
            "tools/noop must carry a [render] section as the format sample"
        );
        let out = render_outputs(&lower_manifest(&m).unwrap(), &m).expect("noop renders");
        let committed =
            std::fs::read_to_string(root.join("tools/noop/generated/render.json")).unwrap();
        assert_eq!(
            committed,
            out.render_json.expect("noop declares [render]"),
            "tools/noop/generated/render.json is stale; run `cargo xtask tool-codegen`"
        );
    }
}
