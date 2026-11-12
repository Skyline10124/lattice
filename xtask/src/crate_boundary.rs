// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 受控 crate 的内部直接依赖白名单检查。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cargo_metadata::{DependencyKind, Metadata, Package, PackageId};

use crate::boundary_report;
use crate::path_norm::normpath;
use crate::repo_root;
use crate::workspace_metadata;

#[derive(Clone, Copy, Debug)]
pub struct DirectDependencyRule {
    pub package: &'static str,
    pub allowed_workspace_dependencies: &'static [&'static str],
}

pub const RULES: &[DirectDependencyRule] = &[DirectDependencyRule {
    package: "lattice-ipc",
    allowed_workspace_dependencies: &["lattice-session", "lattice-conversation", "lattice-store"],
}];

pub fn run() -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let hits = check_workspace_rules(&root, RULES)?;
    boundary_report::run_groups(
        "crate 依赖边界检查通过",
        "crate 依赖边界检查失败",
        &[("lattice-ipc 直接内部依赖", hits)],
    )
}

pub fn check_workspace_rules(
    repo_root: &Path,
    rules: &[DirectDependencyRule],
) -> anyhow::Result<Vec<String>> {
    let metadata = workspace_metadata::load_no_deps(repo_root)?;
    check_metadata_rules(&metadata, rules)
}

pub fn check_metadata_rules(
    metadata: &Metadata,
    rules: &[DirectDependencyRule],
) -> anyhow::Result<Vec<String>> {
    let packages_by_id: BTreeMap<&PackageId, &Package> =
        metadata.packages.iter().map(|p| (&p.id, p)).collect();

    let mut members_by_dir: BTreeMap<PathBuf, &Package> = BTreeMap::new();
    let mut members_by_name: BTreeMap<&str, &Package> = BTreeMap::new();

    for member_id in &metadata.workspace_members {
        let pkg = packages_by_id.get(member_id).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "crate 依赖边界：workspace_members 中的成员 '{member_id}' 未出现在 packages 中"
            )
        })?;
        let manifest_dir = pkg.manifest_path.parent().ok_or_else(|| {
            anyhow::anyhow!(
                "crate 依赖边界：manifest_path '{}' 没有父目录",
                pkg.manifest_path
            )
        })?;
        let norm_dir = normpath(manifest_dir.as_std_path());
        members_by_dir.insert(norm_dir, pkg);
        members_by_name.insert(pkg.name.as_str(), pkg);
    }

    let mut hits = Vec::new();
    for rule in rules {
        let checked_pkg = members_by_name.get(rule.package).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "crate 依赖边界：受检包 '{}' 未出现在 workspace_members 中",
                rule.package
            )
        })?;

        for dep in &checked_pkg.dependencies {
            let section = match dep.kind {
                DependencyKind::Development => continue,
                DependencyKind::Normal => "dependencies",
                DependencyKind::Build => "build-dependencies",
                DependencyKind::Unknown => {
                    anyhow::bail!(
                        "crate 依赖边界：{}: 无法识别依赖 '{}' 的 kind",
                        checked_pkg.name,
                        dep.name
                    );
                }
            };

            if dep.name.trim().is_empty() {
                anyhow::bail!(
                    "crate 依赖边界：{}: dependencies 条目的包名为空",
                    checked_pkg.name
                );
            }

            let workspace_dep_name = match (dep.path.as_ref(), dep.source.as_deref()) {
                (Some(dep_path), _) => {
                    let norm_dep_dir = normpath(dep_path.as_std_path());
                    if let Some(member_pkg) = members_by_dir.get(&norm_dep_dir).copied() {
                        if member_pkg.name.as_str() != dep.name.as_str() {
                            anyhow::bail!(
                                "crate 依赖边界：{}: 依赖路径 '{}' 解析到内部成员 '{}'，与声明的包名 '{}' 不一致",
                                checked_pkg.name,
                                dep_path,
                                member_pkg.name,
                                dep.name
                            );
                        }
                        Some(member_pkg.name.as_str())
                    } else {
                        if !norm_dep_dir.join("Cargo.toml").is_file() {
                            anyhow::bail!(
                                "crate 依赖边界：{}: 无法识别位于 '{}' 的路径依赖 '{}'",
                                checked_pkg.name,
                                dep_path,
                                dep.name
                            );
                        }
                        None
                    }
                }
                (None, Some(_)) => None,
                (None, None) => {
                    anyhow::bail!(
                        "crate 依赖边界：{}: 无法识别依赖 '{}'（metadata 中同时缺少 path 与 source）",
                        checked_pkg.name,
                        dep.name
                    );
                }
            };

            let Some(workspace_dep_name) = workspace_dep_name else {
                continue;
            };

            if !rule
                .allowed_workspace_dependencies
                .contains(&workspace_dep_name)
            {
                let mut qualifiers = vec![format!("[{section}]")];
                if let Some(target) = &dep.target {
                    qualifiers.push(format!("target '{target}'"));
                }
                if dep.optional {
                    qualifiers.push("optional".to_owned());
                }
                let identity = match dep.rename.as_deref() {
                    Some(alias) if alias != workspace_dep_name => {
                        format!("'{workspace_dep_name}'（改名为 '{alias}'）")
                    }
                    _ => format!("'{workspace_dep_name}'"),
                };
                hits.push(format!(
                    "{}: {} 出现不允许的内部依赖 {}；允许的内部依赖：[{}]",
                    checked_pkg.name,
                    qualifiers.join(" "),
                    identity,
                    rule.allowed_workspace_dependencies.join(", ")
                ));
            }
        }
    }

    Ok(hits)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn write_crate(dir: &Path, name: &str, extra_manifest: &str) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "").unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{extra_manifest}"
            ),
        )
        .unwrap();
    }

    fn setup_workspace(ipc_extra_manifest: &str) -> (TempDir, PathBuf) {
        let temp = TempDir::new().unwrap();
        let ws_root = temp.path().join("ws");
        fs::create_dir_all(&ws_root).unwrap();
        fs::write(
            ws_root.join("Cargo.toml"),
            concat!(
                "[workspace]\n",
                "resolver = \"3\"\n",
                "members = [\n",
                "  \"crates/lattice-ipc\",\n",
                "  \"crates/lattice-session\",\n",
                "  \"crates/lattice-conversation\",\n",
                "  \"crates/lattice-store\",\n",
                "  \"crates/lattice-llm\",\n",
                "  \"crates/plain-helper\",\n",
                "]\n",
            ),
        )
        .unwrap();
        for member in [
            "lattice-session",
            "lattice-conversation",
            "lattice-store",
            "lattice-llm",
            "plain-helper",
        ] {
            write_crate(&ws_root.join("crates").join(member), member, "");
        }
        write_crate(
            &ws_root.join("crates/lattice-ipc"),
            "lattice-ipc",
            ipc_extra_manifest,
        );
        write_crate(&temp.path().join("outside-util"), "outside-util", "");
        (temp, ws_root)
    }

    #[test]
    fn rejects_forbidden_normal_workspace_dependency() {
        let (_temp, ws_root) =
            setup_workspace("\n[dependencies]\nlattice-llm = { path = \"../lattice-llm\" }\n");
        let hits = check_workspace_rules(&ws_root, RULES).unwrap();
        assert_eq!(hits.len(), 1, "应命中且只命中 1 条违规，实际：{hits:?}");
        assert!(
            hits[0].starts_with("lattice-ipc: "),
            "报错应带受检包名：{hits:?}"
        );
        assert!(
            hits[0].contains("[dependencies]"),
            "报错应带依赖段：{hits:?}"
        );
        assert!(
            hits[0].contains("出现不允许的内部依赖 'lattice-llm'"),
            "报错应带违规依赖名：{hits:?}"
        );
    }

    #[test]
    fn rejects_forbidden_build_workspace_dependency() {
        let (_temp, ws_root) = setup_workspace(
            "\n[build-dependencies]\nlattice-llm = { path = \"../lattice-llm\" }\n",
        );
        let hits = check_workspace_rules(&ws_root, RULES).unwrap();
        assert_eq!(hits.len(), 1, "应命中且只命中 1 条违规，实际：{hits:?}");
        assert!(
            hits[0].starts_with("lattice-ipc: "),
            "报错应带受检包名：{hits:?}"
        );
        assert!(
            hits[0].contains("[build-dependencies]"),
            "报错应带 build 依赖段：{hits:?}"
        );
        assert!(
            hits[0].contains("出现不允许的内部依赖 'lattice-llm'"),
            "报错应带违规依赖名：{hits:?}"
        );
    }

    #[test]
    fn rejects_renamed_forbidden_workspace_dependency_and_reports_alias() {
        let (_temp, ws_root) = setup_workspace(
            "\n[dependencies]\nllm_alias = { package = \"lattice-llm\", path = \"../lattice-llm\" }\n",
        );
        let hits = check_workspace_rules(&ws_root, RULES).unwrap();
        assert_eq!(hits.len(), 1, "应命中且只命中 1 条违规，实际：{hits:?}");
        assert!(
            hits[0].starts_with("lattice-ipc: "),
            "报错应带受检包名：{hits:?}"
        );
        assert!(
            hits[0].contains("[dependencies]"),
            "报错应带依赖段：{hits:?}"
        );
        assert!(
            hits[0].contains("'lattice-llm'（改名为 'llm_alias'）"),
            "报错应同时给出真实包名与改名别名：{hits:?}"
        );
    }

    #[test]
    fn rejects_inactive_target_specific_and_optional_workspace_dependency() {
        let (_temp, ws_root) = setup_workspace(concat!(
            "\n[target.'cfg(windows)'.dependencies]\n",
            "lattice-llm = { path = \"../lattice-llm\", optional = true }\n",
        ));
        let hits = check_workspace_rules(&ws_root, RULES).unwrap();
        assert_eq!(hits.len(), 1, "应命中且只命中 1 条违规，实际：{hits:?}");
        assert!(
            hits[0].starts_with("lattice-ipc: "),
            "报错应带受检包名：{hits:?}"
        );
        assert!(
            hits[0].contains("[dependencies]"),
            "报错应带依赖段：{hits:?}"
        );
        assert!(
            hits[0].contains("target 'cfg(windows)'"),
            "报错应带 target 表达式：{hits:?}"
        );
        assert!(
            hits[0].contains("optional"),
            "报错应带 optional 标记：{hits:?}"
        );
        assert!(
            hits[0].contains("出现不允许的内部依赖 'lattice-llm'"),
            "报错应带违规依赖名：{hits:?}"
        );
    }

    #[test]
    fn rejects_non_lattice_named_workspace_member() {
        let (_temp, ws_root) =
            setup_workspace("\n[dependencies]\nplain-helper = { path = \"../plain-helper\" }\n");
        let hits = check_workspace_rules(&ws_root, RULES).unwrap();
        assert_eq!(hits.len(), 1, "应命中且只命中 1 条违规，实际：{hits:?}");
        assert!(
            hits[0].starts_with("lattice-ipc: "),
            "报错应带受检包名：{hits:?}"
        );
        assert!(
            hits[0].contains("[dependencies]"),
            "报错应带依赖段：{hits:?}"
        );
        assert!(
            hits[0].contains("出现不允许的内部依赖 'plain-helper'"),
            "报错应带非 lattice 前缀内部成员名：{hits:?}"
        );
    }

    #[test]
    fn allows_dev_only_forbidden_members_allowed_members_and_outside_path_crate() {
        let (_temp, ws_root) = setup_workspace(concat!(
            "\n[dependencies]\n",
            "lattice-session = { path = \"../lattice-session\" }\n",
            "conv_alias = { package = \"lattice-conversation\", path = \"../lattice-conversation\" }\n",
            "lattice-store = { path = \"../lattice-store\", optional = true }\n",
            "outside-util = { path = \"../../../outside-util\" }\n",
            "\n[dev-dependencies]\n",
            "lattice-llm = { path = \"../lattice-llm\" }\n",
            "plain-helper = { path = \"../plain-helper\" }\n",
        ));
        let hits = check_workspace_rules(&ws_root, RULES).unwrap();
        assert!(hits.is_empty(), "预期检查通过且无违规，实际：{hits:?}");
    }

    #[test]
    fn errors_when_checked_package_missing_or_dependency_unidentifiable() {
        let (_temp, ws_root) = setup_workspace(
            "\n[dependencies]\nlattice-session = { path = \"../lattice-session\" }\n",
        );
        let missing_rule = [DirectDependencyRule {
            package: "missing-package",
            allowed_workspace_dependencies: &[],
        }];
        let err = check_workspace_rules(&ws_root, &missing_rule).unwrap_err();
        assert!(
            err.to_string().contains("受检包 'missing-package'"),
            "报错应指出缺失的受检包：{err}"
        );

        let mut metadata = workspace_metadata::load_no_deps(&ws_root).unwrap();
        let ipc_pkg = metadata
            .packages
            .iter_mut()
            .find(|p| p.name == "lattice-ipc")
            .unwrap();
        ipc_pkg.dependencies[0].path = None;
        ipc_pkg.dependencies[0].source = None;
        let err = check_metadata_rules(&metadata, RULES).unwrap_err();
        assert!(
            err.to_string().contains("无法识别依赖"),
            "报错应指出无法识别依赖：{err}"
        );
    }
}
