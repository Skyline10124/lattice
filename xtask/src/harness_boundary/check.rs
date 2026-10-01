// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use cargo_metadata::{Metadata, MetadataCommand, PackageId};
use regex::Regex;
use toml::Value;

use crate::path_norm::normpath;

const HARNESS_PACKAGE: &str = "lattice-system-harness";

static WORKSPACE_TRUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"workspace\s*=\s*true").unwrap());

pub fn manifest_forbidden_deps(
    manifest_path: &Path,
    repo_root: &Path,
) -> anyhow::Result<Vec<String>> {
    let raw = std::fs::read_to_string(manifest_path)?;
    let data: Value = toml::from_str(&raw)?;
    let manifest_dir = manifest_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("manifest has no parent"))?;
    let crates_dir = repo_root.join("crates");

    let mut hits = Vec::new();
    for (table_path, dep_key, dep_val) in iter_dep_tables(&data, "") {
        let (actual_name, path_val) = dep_identity(dep_key, dep_val);
        if actual_name.starts_with("lattice-") || actual_name.starts_with("lancet-") {
            hits.push(format!(
                "{}: [{table_path}] {dep_key}: forbidden product crate name '{actual_name}'",
                manifest_path.display()
            ));
        }
        if let Some(path_val) = path_val {
            let resolved = manifest_dir.join(path_val);
            if under_crates_dir(&resolved, &crates_dir) {
                hits.push(format!(
                    "{}: [{table_path}] {dep_key}: product path dependency -> {}",
                    manifest_path.display(),
                    resolved.display()
                ));
            }
        }
    }
    Ok(hits)
}

pub fn manifest_workspace_inheritance(manifest_path: &Path) -> anyhow::Result<Vec<String>> {
    let raw = std::fs::read_to_string(manifest_path)?;
    let mut hits = Vec::new();
    for (line_no, line) in raw.lines().enumerate() {
        if WORKSPACE_TRUE_RE.is_match(line) {
            hits.push(format!("{}:{}", line_no + 1, line));
        }
    }
    Ok(hits)
}

pub fn metadata_transitive_workspace_members(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    let metadata = load_metadata(repo_root)?;
    transitive_workspace_member_hits(&metadata)
}

pub fn transitive_workspace_member_hits(metadata: &Metadata) -> anyhow::Result<Vec<String>> {
    let id_to_name: HashMap<&PackageId, &str> = metadata
        .packages
        .iter()
        .map(|p| (&p.id, p.name.as_str()))
        .collect();
    let workspace_members: HashSet<&PackageId> = metadata.workspace_members.iter().collect();

    let root_id = metadata
        .packages
        .iter()
        .find(|p| p.name == HARNESS_PACKAGE)
        .map(|p| &p.id)
        .ok_or_else(|| {
            anyhow::anyhow!("metadata: {HARNESS_PACKAGE} package missing from workspace")
        })?;

    let nodes: HashMap<&PackageId, &cargo_metadata::Node> = metadata
        .resolve
        .as_ref()
        .map(|r| r.nodes.iter().map(|n| (&n.id, n)).collect())
        .unwrap_or_default();

    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![root_id];

    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if pid != root_id && workspace_members.contains(pid) {
            let name = id_to_name.get(pid).copied().unwrap_or("unknown");
            hits.push(format!(
                "metadata: forbidden workspace-member dependency {name}"
            ));
        }
        if let Some(node) = nodes.get(pid) {
            for dep_id in &node.dependencies {
                stack.push(dep_id);
            }
        }
    }
    Ok(hits)
}

fn load_metadata(repo_root: &Path) -> anyhow::Result<Metadata> {
    MetadataCommand::new()
        .current_dir(repo_root)
        .exec()
        .map_err(|e| anyhow::anyhow!("failed to read workspace metadata: {e}"))
}

fn under_crates_dir(resolved: &Path, crates_dir: &Path) -> bool {
    let resolved = normpath(resolved);
    let crates_dir = normpath(crates_dir);
    resolved == crates_dir || resolved.starts_with(crates_dir.join(""))
}

fn dep_identity<'a>(dep_key: &'a str, dep_val: &'a Value) -> (&'a str, Option<&'a str>) {
    match dep_val {
        Value::Table(table) => {
            let name = table
                .get("package")
                .and_then(Value::as_str)
                .unwrap_or(dep_key);
            let path = table.get("path").and_then(Value::as_str);
            (name, path)
        }
        _ => (dep_key, None),
    }
}

fn iter_dep_tables<'a>(table: &'a Value, prefix: &str) -> Vec<(String, &'a str, &'a Value)> {
    let mut out = Vec::new();
    let Value::Table(map) = table else {
        return out;
    };
    for (key, value) in map {
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Table(_) if key.ends_with("dependencies") => {
                if let Value::Table(deps) = value {
                    for (dep_key, dep_val) in deps {
                        out.push((full.clone(), dep_key.as_str(), dep_val));
                    }
                }
            }
            Value::Table(_) => {
                out.extend(iter_dep_tables(value, &full));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn joined(hits: &[String]) -> String {
        hits.join("\n")
    }

    #[test]
    fn flags_string_form_lattice_dependency() {
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("Cargo.toml");
        fs::write(
            &manifest,
            r#"
[package]
name = "harness"
version = "0.0.0"

[dependencies]
lattice-api = "0.1"
"#,
        )
        .unwrap();
        let out = manifest_forbidden_deps(&manifest, dir.path()).unwrap();
        assert!(joined(&out).contains("forbidden product crate name 'lattice-api'"));
    }

    #[test]
    fn flags_package_rename_with_neutral_key() {
        let dir = TempDir::new().unwrap();
        let crates = dir.path().join("crates/net-guard");
        fs::create_dir_all(&crates).unwrap();
        let manifest = dir.path().join("Cargo.toml");
        fs::write(
            &manifest,
            r#"
[dependencies]
guard = { package = "lancet-protocol", path = '../crates/lancet-protocol' }
"#,
        )
        .unwrap();
        let out = manifest_forbidden_deps(&manifest, dir.path()).unwrap();
        assert!(joined(&out).contains("forbidden product crate name 'lancet-protocol'"));
    }

    #[test]
    fn flags_single_quoted_path_under_crates() {
        let dir = TempDir::new().unwrap();
        let product = dir.path().join("crates/foo");
        fs::create_dir_all(&product).unwrap();
        let manifest = dir.path().join("Cargo.toml");
        fs::write(
            &manifest,
            r#"
[dependencies]
foo = { path = 'crates/foo' }
"#,
        )
        .unwrap();
        let out = manifest_forbidden_deps(&manifest, dir.path()).unwrap();
        assert!(joined(&out).contains("product path dependency"));
    }

    #[test]
    fn parent_relative_path_under_crates_is_flagged() {
        let dir = TempDir::new().unwrap();
        let harness_dir = dir.path().join("crates/lattice-system-harness");
        fs::create_dir_all(&harness_dir).unwrap();
        let product = dir.path().join("crates/net-guard");
        fs::create_dir_all(&product).unwrap();
        let manifest = harness_dir.join("Cargo.toml");
        fs::write(
            &manifest,
            r#"
[dependencies]
guard = { path = '../../crates/net-guard' }
"#,
        )
        .unwrap();
        let out = manifest_forbidden_deps(&manifest, dir.path()).unwrap();
        assert!(joined(&out).contains("product path dependency"));
    }

    #[test]
    fn transitive_workspace_member_hit_when_present() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"
[workspace]
resolver = "3"
members = ["crates/lattice-system-harness", "crates/net-guard"]
"#,
        )
        .unwrap();
        let harness = root.join("crates/lattice-system-harness");
        let guard = root.join("crates/net-guard");
        fs::create_dir_all(&harness).unwrap();
        fs::create_dir_all(&guard).unwrap();
        fs::write(
            harness.join("Cargo.toml"),
            r#"
[package]
name = "lattice-system-harness"
version = "0.0.0"
edition = "2024"

[dependencies]
net-guard = { path = "../net-guard" }
"#,
        )
        .unwrap();
        fs::write(
            guard.join("Cargo.toml"),
            r#"
[package]
name = "net-guard"
version = "0.0.0"
edition = "2024"
"#,
        )
        .unwrap();
        fs::create_dir_all(harness.join("src")).unwrap();
        fs::create_dir_all(guard.join("src")).unwrap();
        fs::write(harness.join("src/lib.rs"), "").unwrap();
        fs::write(guard.join("src/lib.rs"), "").unwrap();
        let out = metadata_transitive_workspace_members(root).unwrap();
        assert!(joined(&out).contains("net-guard"));
    }
}
