// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use cargo_metadata::{DependencyKind, Metadata, PackageId};
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
        .ok_or_else(|| anyhow::anyhow!("清单文件没有上级目录"))?;
    let crates_dir = repo_root.join("crates");

    let mut hits = Vec::new();
    for (table_path, dep_key, dep_val) in iter_dep_tables(&data, "") {
        let (actual_name, path_val) = dep_identity(dep_key, dep_val);
        if actual_name.starts_with("lattice-") {
            hits.push(format!(
                "{}: [{table_path}] {dep_key} 指向禁止的产品 crate '{actual_name}'",
                manifest_path.display()
            ));
        }
        if let Some(path_val) = path_val {
            let resolved = manifest_dir.join(path_val);
            if under_crates_dir(&resolved, &crates_dir) {
                hits.push(format!(
                    "{}: [{table_path}] {dep_key} 的路径依赖指向产品目录 {}",
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
    let metadata = crate::workspace_metadata::load_full(repo_root)?;
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
        .ok_or_else(|| anyhow::anyhow!("cargo metadata 中缺少 {HARNESS_PACKAGE} 包"))?;

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
            let name = id_to_name.get(pid).copied().unwrap_or("（未知包）");
            hits.push(format!("cargo metadata：依赖了 workspace 成员 {name}"));
        }
        if let Some(node) = nodes.get(pid) {
            for dep_id in &node.dependencies {
                stack.push(dep_id);
            }
        }
    }
    Ok(hits)
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

/// 测试专用 feature 一旦进入生产依赖图，它守护的 `#[cfg(feature = "...")]` 门控随之失效：测试
/// 辅助代码会进入 release 二进制与所有下游依赖图。测试代码经 `[dev-dependencies]` 启用这些
/// feature。本检查覆盖三种泄漏：非测试 feature 转发到测试 feature、普通依赖显式启用测试 feature，
/// 以及普通依赖的默认 feature 转发到测试 feature；dev-dependencies 与 build-dependencies 不在检查
/// 范围内。
///
/// 按 feature 名的 `test-` 前缀识别，不维护枚举列表，任何 crate 新增的 `test-*` feature 都自动
/// 纳入检查。
pub fn production_test_feature_hits(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    let metadata = crate::workspace_metadata::load_no_deps(repo_root)?;

    let packages: BTreeMap<_, _> = metadata
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect();
    let mut parts = Vec::new();
    for pkg in &metadata.packages {
        for feature in pkg
            .features
            .keys()
            .filter(|name| !name.starts_with("test-"))
        {
            if feature_reaches_test_only(&pkg.features, feature, &mut BTreeSet::new()) {
                parts.push(format!(
                    "{}: 生产 feature '{feature}' 转发到测试专用 feature",
                    pkg.name
                ));
            }
        }
        for dep in &pkg.dependencies {
            if dep.kind != DependencyKind::Normal {
                continue;
            }
            let target = packages.get(dep.name.as_str()).copied();
            let mut flagged = Vec::new();
            for feature in &dep.features {
                if feature.starts_with("test-")
                    || target.is_some_and(|package| {
                        feature_reaches_test_only(&package.features, feature, &mut BTreeSet::new())
                    })
                {
                    flagged.push(feature.as_str());
                }
            }
            if dep.uses_default_features
                && target.is_some_and(|package| {
                    feature_reaches_test_only(&package.features, "default", &mut BTreeSet::new())
                })
            {
                flagged.push("default");
            }
            if !flagged.is_empty() {
                parts.push(format!(
                    "{}: [dependencies] {} 启用了测试专用 feature {}；请把它们移到 [dev-dependencies] 中的对应条目",
                    pkg.name.as_str(),
                    dep.name,
                    flagged.join(", ")
                ));
            }
        }
    }
    Ok(parts)
}

fn feature_reaches_test_only(
    features: &BTreeMap<String, Vec<String>>,
    feature: &str,
    visiting: &mut BTreeSet<String>,
) -> bool {
    if feature.starts_with("test-") {
        return true;
    }
    if !visiting.insert(feature.to_string()) {
        return false;
    }
    let reaches = features.get(feature).is_some_and(|members| {
        members.iter().any(|member| {
            if let Some((_, dependency_feature)) = member.split_once('/') {
                dependency_feature.starts_with("test-")
            } else if member.starts_with("dep:") {
                false
            } else {
                feature_reaches_test_only(features, member, visiting)
            }
        })
    });
    visiting.remove(feature);
    reaches
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
lattice-server = "0.1"
"#,
        )
        .unwrap();
        let out = manifest_forbidden_deps(&manifest, dir.path()).unwrap();
        assert!(joined(&out).contains("指向禁止的产品 crate 'lattice-server'"));
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
guard = { package = "lattice-session", path = '../crates/lattice-session' }
"#,
        )
        .unwrap();
        let out = manifest_forbidden_deps(&manifest, dir.path()).unwrap();
        assert!(joined(&out).contains("指向禁止的产品 crate 'lattice-session'"));
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
        assert!(joined(&out).contains("的路径依赖指向产品目录"));
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
        assert!(joined(&out).contains("的路径依赖指向产品目录"));
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

    /// 两个 crate 组成的虚拟 workspace：consumer 在两个依赖段中都启用 `test-utils`。
    /// 只有 `[dependencies]` 中的条目算违规；`[dev-dependencies]` 中的写法正是 #88 采用的修正。
    fn write_test_feature_workspace(root: &Path, production_features: &str) {
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/consumer\", \"crates/helper\"]\n",
        )
        .unwrap();
        let consumer = root.join("crates/consumer");
        fs::create_dir_all(consumer.join("src")).unwrap();
        fs::write(consumer.join("src/lib.rs"), "").unwrap();
        fs::write(
            consumer.join("Cargo.toml"),
            format!(
                "[package]\nname = \"consumer\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
                 [dependencies]\nhelper = {{ path = \"../helper\"{production_features} }}\n\
                 [dev-dependencies]\nhelper = {{ path = \"../helper\", features = [\"test-utils\"] }}\n"
            ),
        )
        .unwrap();
        let helper = root.join("crates/helper");
        fs::create_dir_all(helper.join("src")).unwrap();
        fs::write(helper.join("src/lib.rs"), "").unwrap();
        fs::write(
            helper.join("Cargo.toml"),
            "[package]\nname = \"helper\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[features]\ntest-utils = []\n",
        )
        .unwrap();
    }

    #[test]
    fn production_test_feature_flagged_only_in_dependencies_section() {
        let dir = TempDir::new().unwrap();
        write_test_feature_workspace(dir.path(), ", features = [\"test-utils\"]");
        let hits = production_test_feature_hits(dir.path()).unwrap();
        let text = joined(&hits);
        assert!(
            text.contains("consumer") && text.contains("test-utils"),
            "生产依赖中的 test-utils 应被报告：{text}"
        );
        assert_eq!(hits.len(), 1, "dev-dependencies 中的条目不应计入：{text}");
    }

    #[test]
    fn production_feature_forwarding_to_test_utils_is_flagged() {
        let dir = TempDir::new().unwrap();
        write_test_feature_workspace(dir.path(), "");
        let manifest = dir.path().join("crates/consumer/Cargo.toml");
        let mut text = fs::read_to_string(&manifest).unwrap();
        text.push_str("[features]\nrelease = [\"helper/test-utils\"]\n");
        fs::write(&manifest, text).unwrap();
        let hits = production_test_feature_hits(dir.path()).unwrap();
        assert!(joined(&hits).contains("生产 feature 'release'"));
    }

    #[test]
    fn dependency_default_forwarding_to_test_utils_is_flagged() {
        let dir = TempDir::new().unwrap();
        write_test_feature_workspace(dir.path(), "");
        let manifest = dir.path().join("crates/helper/Cargo.toml");
        let mut text = fs::read_to_string(&manifest).unwrap();
        text.push_str("default = [\"test-utils\"]\n");
        fs::write(&manifest, text).unwrap();
        let hits = production_test_feature_hits(dir.path()).unwrap();
        let text = joined(&hits);
        assert!(text.contains("consumer: [dependencies] helper"));
        assert!(text.contains("default"));
    }
}
