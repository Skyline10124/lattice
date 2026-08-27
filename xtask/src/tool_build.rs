// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `xtask tool-build` — build a Source Tool's WASM Component and pin its hash.
//!
//! For each `tools/<name>/manifest.toml`:
//! 1. Build the guest crate (`tools/<name>`, package name derived from the tool
//!    name) for `wasm32-unknown-unknown` in release mode with
//!    `--remap-path-prefix` for reproducible debug paths.
//! 2. Encode the core module into a Component with `wasm-tools component new`.
//! 3. Validate the Component and assert its import surface is exactly the WIT
//!    world's manifest-declared Tool Host interfaces (no `wasi:*` ambient authority).
//! 4. SHA-256 the Component, write the digest back into `manifest.toml`
//!    `[artifact] sha256`, and re-run `tool-codegen` so the generated
//!    `expected-identity.json` (artifact + descriptor digest) stays consistent.
//!
//! `--verify` rebuilds into a temp path and compares the digest against the
//! committed Component. Per #5 decision 1 this lane is **warn-only**: a
//! mismatch prints a warning and does not fail, so bit-for-bit reproducibility
//! can be tracked without blocking slice B.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::repo_root;
use crate::tool_codegen;
use crate::tool_codegen::{component_filename, discover_tool_manifests, sha256_hex};
use anyhow::{Context as _, bail};
use clap::Args;

#[derive(Args, Debug)]
pub struct ToolBuildArgs {
    /// Rebuild into a temp path and compare the digest against the committed
    /// Component. Warn-only (decision 1): never fails the gate on mismatch.
    #[arg(long)]
    verify: bool,
}

pub fn run(args: ToolBuildArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let manifests = discover_tool_manifests(&root)?;
    if manifests.is_empty() {
        bail!("no tool manifests found under tools/*/manifest.toml");
    }
    for manifest_path in manifests {
        if args.verify {
            verify_build(&root, &manifest_path)?;
        } else {
            build_and_pin(&root, &manifest_path)?;
        }
    }
    Ok(())
}

/// CI rebuild-and-compare lane (decision 1): rebuild each Component into a
/// temp path and compare its digest to the committed one. Warn-only — never
/// fails, so bit-for-bit reproducibility is tracked without blocking.
pub fn verify() -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let manifests = discover_tool_manifests(&root)?;
    for manifest_path in manifests {
        verify_build(&root, &manifest_path)?;
    }
    Ok(())
}

fn build_and_pin(root: &Path, manifest_path: &Path) -> anyhow::Result<()> {
    let tool_name = read_tool_name(manifest_path)?;
    let pkg = crate_name(&tool_name);
    let tool_dir = manifest_path.parent().unwrap();
    let gen_dir = tool_dir.join("generated");
    let component_file = component_filename(&tool_name);
    let component_path = gen_dir.join(&component_file);

    let module_path = build_guest(root, &pkg, false)?;
    encode_component(&module_path, &component_path)?;
    validate_component(&component_path, &gen_dir)?;
    let digest = sha256_file(&component_path)?;

    write_manifest_sha256(manifest_path, &digest)?;
    // Re-run codegen so expected-identity.json (artifact + descriptor digest)
    // reflects the pinned hash. The descriptor carries the artifact sha256, so
    // its digest changes too.
    tool_codegen::generate()?;

    println!(
        "built {} -> {} (sha256 {digest})",
        pkg,
        component_path.display()
    );
    Ok(())
}

fn verify_build(root: &Path, manifest_path: &Path) -> anyhow::Result<()> {
    let tool_name = read_tool_name(manifest_path)?;
    let pkg = crate_name(&tool_name);
    let tool_dir = manifest_path.parent().unwrap();
    let gen_dir = tool_dir.join("generated");
    let component_file = component_filename(&tool_name);
    let committed = gen_dir.join(&component_file);

    let module_path = build_guest(root, &pkg, true)?;
    let temp_component = module_path.with_extension("component.wasm");
    encode_component(&module_path, &temp_component)?;
    validate_component(&temp_component, &gen_dir)?;

    if !committed.is_file() {
        println!(
            "warn: {} has no committed Component; run `cargo xtask tool-build`",
            committed.display()
        );
        return Ok(());
    }
    let rebuilt_digest = sha256_file(&temp_component)?;
    let committed_digest = sha256_file(&committed)?;
    if rebuilt_digest == committed_digest {
        println!("verify: {pkg} Component reproduces committed digest ({committed_digest})");
    } else {
        // Warn-only per decision 1: collect reproducibility data, do not fail.
        println!(
            "warn: {pkg} Component digest drifted (rebuilt {rebuilt_digest} != committed \
             {committed_digest}); bit-for-bit reproducibility not yet enforced (decision 1)"
        );
    }
    Ok(())
}

/// Build the guest crate for wasm32-unknown-unknown. When `temp` is true the
/// artifact is written to a unique temp path so `--verify` can compare without
/// clobbering the committed Component.
fn build_guest(root: &Path, pkg: &str, temp: bool) -> anyhow::Result<PathBuf> {
    // --remap-path-prefix strips machine-specific absolute paths from debug
    // info so the Component reproduces bit-for-bit across machines (#73).
    //
    // Two prefixes are needed, not one. The workspace root covers guest source
    // paths. The toolchain sysroot covers std/core source paths that rustc
    // resolves through the *local* rustup installation instead of its
    // virtualized `/rustc/<commit-hash>/` prefix: without it the committed
    // `.wasm` embeds e.g.
    // `/home/<user>/.rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu/lib/
    //  rustlib/src/rust/library/core/src/str/pattern.rs`
    // — one maintainer-host path among otherwise already-normalized
    // `/rustc/<hash>/library/...` entries, which alone makes every
    // cross-machine `--verify` report a spurious digest drift. Remapping the
    // sysroot onto the same `/rustc/<commit-hash>` prefix rustc uses for the
    // rest of std makes the two representations agree.
    let mut remap = format!("--remap-path-prefix={}=", root.display());
    if let Some((sysroot, commit_hash)) = rustc_sysroot_and_commit_hash() {
        remap.push_str(&format!(
            " --remap-path-prefix={sysroot}/lib/rustlib/src/rust=/rustc/{commit_hash}"
        ));
    }
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root).env("RUSTFLAGS", &remap).args([
        "build",
        "-p",
        pkg,
        "--release",
        "--target",
        "wasm32-unknown-unknown",
    ]);
    if temp {
        // Redirect the artifact to a temp file via a unique target-dir slot.
        let tmp = std::env::temp_dir().join(format!("lattice-toolbuild-{pkg}"));
        cmd.arg("--target-dir").arg(&tmp);
    }
    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        bail!("cargo build -p {pkg} --target wasm32-unknown-unknown failed");
    }
    let target_dir = if temp {
        std::env::temp_dir().join(format!("lattice-toolbuild-{pkg}"))
    } else {
        root.join("target")
    };
    // cdylib artifact name on wasm32: `<pkg_with_underscores>.wasm`
    // (no `lib` prefix — the wasm cdylib convention differs from Unix).
    let lib_name = format!("{}.wasm", pkg.replace('-', "_"));
    let module = target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join(&lib_name);
    if !module.is_file() {
        bail!("expected guest artifact at {}", module.display());
    }
    Ok(module)
}

/// Resolve the active toolchain's sysroot and its rustc commit hash, so std
/// source paths can be remapped onto the same `/rustc/<commit-hash>` prefix
/// rustc already uses for most of std. Returns `None` when either lookup fails
/// — the workspace-root remap still applies, and a missing sysroot remap only
/// costs reproducibility, so this must not fail the build.
fn rustc_sysroot_and_commit_hash() -> Option<(String, String)> {
    let sysroot = Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;
    if !sysroot.status.success() {
        return None;
    }
    let sysroot = String::from_utf8(sysroot.stdout).ok()?.trim().to_owned();
    if sysroot.is_empty() {
        return None;
    }

    let verbose = Command::new("rustc")
        .arg("--version")
        .arg("--verbose")
        .output()
        .ok()?;
    if !verbose.status.success() {
        return None;
    }
    let commit_hash = String::from_utf8(verbose.stdout)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("commit-hash: "))
        .map(|h| h.trim().to_owned())?;
    if commit_hash.is_empty() {
        return None;
    }

    Some((sysroot, commit_hash))
}

fn encode_component(module: &Path, out: &Path) -> anyhow::Result<()> {
    let status = Command::new("wasm-tools")
        .arg("component")
        .arg("new")
        .arg(module)
        .arg("-o")
        .arg(out)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("failed to run `wasm-tools component new` (is wasm-tools installed?)")?;
    if !status.success() {
        bail!("wasm-tools component new failed for {}", module.display());
    }
    Ok(())
}

fn validate_component(component: &Path, gen_dir: &Path) -> anyhow::Result<()> {
    // 1. Structural validity.
    let status = Command::new("wasm-tools")
        .arg("validate")
        .arg(component)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        bail!("wasm-tools validate failed for {}", component.display());
    }
    // 2. Import-surface closure: the Component must import exactly the WIT
    //    world's interfaces and no `wasi:*` ambient authority. We extract the
    //    world and assert.
    let wit_out = Command::new("wasm-tools")
        .arg("component")
        .arg("wit")
        .arg(component)
        .output()
        .context("failed to run `wasm-tools component wit`")?;
    if !wit_out.status.success() {
        bail!(
            "wasm-tools component wit failed for {}",
            component.display()
        );
    }
    let world = String::from_utf8_lossy(&wit_out.stdout);
    if world.contains("wasi:") {
        bail!(
            "{} imports a wasi:* interface — ambient WASI authority is forbidden",
            component.display()
        );
    }
    // 3. The Component must conform to the generated WIT world.
    let wit_path = gen_dir.join("tool-host.wit");
    let status = Command::new("wasm-tools")
        .arg("component")
        .arg("targets")
        .arg(&wit_path)
        .arg(component)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        bail!(
            "{} does not conform to the generated world {}",
            component.display(),
            wit_path.display()
        );
    }
    Ok(())
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(sha256_hex(&bytes))
}

/// Write the pinned sha256 back into `manifest.toml [artifact] sha256`,
/// preserving all other formatting via `toml_edit`.
fn write_manifest_sha256(manifest_path: &Path, digest: &str) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(manifest_path)?;
    let mut doc = raw
        .parse::<toml_edit::DocumentMut>()
        .context("manifest is not valid TOML")?;
    let artifact = doc
        .get_mut("artifact")
        .and_then(|v| v.as_table_mut())
        .context("manifest missing [artifact] table")?;
    artifact["sha256"] = toml_edit::value(digest);
    std::fs::write(manifest_path, doc.to_string())?;
    Ok(())
}

fn read_tool_name(manifest_path: &Path) -> anyhow::Result<String> {
    let raw = std::fs::read_to_string(manifest_path)?;
    let value: toml::Value = toml::from_str(&raw)?;
    let name = value
        .get("tool")
        .and_then(|t| t.get("name"))
        .and_then(|n| n.as_str())
        .context("manifest missing [tool] name")?;
    Ok(name.to_owned())
}

fn crate_name(tool_name: &str) -> String {
    tool_name.replace('.', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_maps_dots_to_dashes() {
        assert_eq!(crate_name("noop.echo"), "noop-echo");
    }

    #[test]
    fn component_filename_maps_dots_to_underscores() {
        assert_eq!(component_filename("noop.echo"), "noop_echo.component.wasm");
    }
}
