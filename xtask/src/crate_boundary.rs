// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 受控 crate 的内部直接依赖与普通依赖闭包检查。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use cargo_metadata::{DependencyKind, Metadata, Package, PackageId};

use crate::boundary_report;
use crate::path_norm::normpath;
use crate::repo_root;
use crate::workspace_metadata;

#[derive(Clone, Copy, Debug)]
pub enum WorkspaceDependencyPolicy {
    AllowOnly(&'static [&'static str]),
    Forbid(&'static [&'static str]),
}

#[derive(Clone, Copy, Debug)]
pub struct DirectDependencyRule {
    pub package: &'static str,
    pub policy: WorkspaceDependencyPolicy,
}

#[derive(Clone, Copy, Debug)]
pub struct NormalClosureRule {
    pub package: &'static str,
    pub forbidden_packages: &'static [&'static str],
}

pub const RULES: &[DirectDependencyRule] = &[
    DirectDependencyRule {
        package: "lattice-ipc",
        policy: WorkspaceDependencyPolicy::AllowOnly(&[
            "lattice-session",
            "lattice-conversation",
            "lattice-store",
        ]),
    },
    DirectDependencyRule {
        package: "lattice-tui",
        policy: WorkspaceDependencyPolicy::AllowOnly(&["lattice-local"]),
    },
    DirectDependencyRule {
        package: "lattice-local",
        policy: WorkspaceDependencyPolicy::AllowOnly(&[
            "lattice-ipc",
            "lattice-conversation",
            "lattice-store",
        ]),
    },
    DirectDependencyRule {
        package: "lattice-server",
        policy: WorkspaceDependencyPolicy::Forbid(&["lattice-local"]),
    },
];

pub const CLOSURE_RULES: &[NormalClosureRule] = &[
    NormalClosureRule {
        package: "lattice-tui",
        forbidden_packages: &[
            "lattice-server",
            "lattice-llm",
            "lattice-profile",
            "lattice-agent",
        ],
    },
    NormalClosureRule {
        package: "lattice-local",
        forbidden_packages: &[
            "lattice-server",
            "lattice-llm",
            "lattice-profile",
            "lattice-agent",
        ],
    },
];

pub fn run() -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let direct_hits = check_workspace_rules(&root, RULES)?;
    let closure_hits = check_workspace_normal_closure(&root, CLOSURE_RULES)?;
    boundary_report::run_groups(
        "crate 依赖边界检查通过",
        "crate 依赖边界检查失败",
        &[
            ("内部直接依赖", direct_hits),
            ("普通依赖闭包", closure_hits),
        ],
    )
}

pub fn check_workspace_rules(
    repo_root: &Path,
    rules: &[DirectDependencyRule],
) -> anyhow::Result<Vec<String>> {
    let metadata = workspace_metadata::load_no_deps(repo_root)?;
    check_metadata_rules(&metadata, rules)
}

pub fn check_workspace_normal_closure(
    repo_root: &Path,
    rules: &[NormalClosureRule],
) -> anyhow::Result<Vec<String>> {
    let metadata = workspace_metadata::load_full_all_features(repo_root)?;
    check_metadata_normal_closure(&metadata, rules)
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

            let policy_detail = match rule.policy {
                WorkspaceDependencyPolicy::AllowOnly(allowed)
                    if !allowed.contains(&workspace_dep_name) =>
                {
                    Some(format!("允许的内部依赖：[{}]", allowed.join(", ")))
                }
                WorkspaceDependencyPolicy::Forbid(forbidden)
                    if forbidden.contains(&workspace_dep_name) =>
                {
                    Some(format!("禁止的内部依赖：[{}]", forbidden.join(", ")))
                }
                _ => None,
            };

            if let Some(detail) = policy_detail {
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
                    "{}: {} 出现不允许的内部依赖 {}；{}",
                    checked_pkg.name,
                    qualifiers.join(" "),
                    identity,
                    detail
                ));
            }
        }
    }

    Ok(hits)
}

pub fn check_metadata_normal_closure(
    metadata: &Metadata,
    rules: &[NormalClosureRule],
) -> anyhow::Result<Vec<String>> {
    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("crate 依赖边界：metadata 缺少依赖解析图（`resolve`）"))?;

    let packages_by_id: BTreeMap<&PackageId, &Package> =
        metadata.packages.iter().map(|p| (&p.id, p)).collect();

    let mut members_by_name: BTreeMap<&str, &Package> = BTreeMap::new();
    for member_id in &metadata.workspace_members {
        let pkg = packages_by_id.get(member_id).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "crate 依赖边界：workspace_members 中的成员 '{member_id}' 未出现在 metadata.packages 中"
            )
        })?;
        members_by_name.insert(pkg.name.as_str(), pkg);
    }

    let mut nodes_by_id: BTreeMap<&PackageId, &cargo_metadata::Node> = BTreeMap::new();
    for node in &resolve.nodes {
        if !packages_by_id.contains_key(&node.id) {
            anyhow::bail!(
                "crate 依赖边界：悬空解析节点 '{}' 未出现在 metadata.packages 中",
                node.id
            );
        }
        nodes_by_id.insert(&node.id, node);
    }

    let mut hits = Vec::new();
    for rule in rules {
        let root_pkg = members_by_name.get(rule.package).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "crate 依赖边界：受检包 '{}' 未出现在 workspace_members 中",
                rule.package
            )
        })?;
        let root_id = &root_pkg.id;
        if !nodes_by_id.contains_key(root_id) {
            anyhow::bail!(
                "crate 依赖边界：受检包 '{}'（'{root_id}'）未出现在 resolve.nodes 中",
                rule.package
            );
        }

        let mut visited: BTreeSet<&PackageId> = BTreeSet::new();
        let mut parent: BTreeMap<&PackageId, &PackageId> = BTreeMap::new();
        let mut queue: VecDeque<&PackageId> = VecDeque::new();
        visited.insert(root_id);
        queue.push_back(root_id);

        while let Some(curr_id) = queue.pop_front() {
            let curr_pkg = packages_by_id.get(curr_id).copied().ok_or_else(|| {
                anyhow::anyhow!(
                    "crate 依赖边界：悬空包 ID '{curr_id}' 未出现在 metadata.packages 中"
                )
            })?;
            let curr_node = nodes_by_id.get(curr_id).copied().ok_or_else(|| {
                anyhow::anyhow!("crate 依赖边界：悬空包 ID '{curr_id}' 未出现在 resolve.nodes 中")
            })?;

            if curr_id != root_id && rule.forbidden_packages.contains(&curr_pkg.name.as_str()) {
                let mut chain = Vec::new();
                let mut cursor = curr_id;
                loop {
                    let pkg = packages_by_id
                        .get(cursor)
                        .expect("已验证节点存在于 packages_by_id");
                    chain.push(pkg.name.as_str());
                    if cursor == root_id {
                        break;
                    }
                    cursor = parent.get(cursor).copied().expect("已访问节点必有父边");
                }
                chain.reverse();
                hits.push(format!(
                    "{}: 普通依赖闭包中可达禁止的包 '{}'，路径：{}",
                    rule.package,
                    curr_pkg.name,
                    chain.join(" -> ")
                ));
            }

            for dep in &curr_node.deps {
                let mut has_normal = false;
                for info in &dep.dep_kinds {
                    match info.kind {
                        DependencyKind::Normal => has_normal = true,
                        DependencyKind::Development | DependencyKind::Build => {}
                        DependencyKind::Unknown => {
                            anyhow::bail!(
                                "crate 依赖边界：{}: 指向 '{}' 的依赖边存在未知 dep_kind",
                                curr_pkg.name,
                                dep.pkg
                            );
                        }
                    }
                }
                if !has_normal {
                    continue;
                }

                let target_id = &dep.pkg;
                if !packages_by_id.contains_key(target_id) || !nodes_by_id.contains_key(target_id) {
                    anyhow::bail!(
                        "crate 依赖边界：{}: 普通依赖边指向悬空节点 '{target_id}'",
                        curr_pkg.name
                    );
                }

                if visited.insert(target_id) {
                    parent.insert(target_id, curr_id);
                    queue.push_back(target_id);
                }
            }
        }
    }

    Ok(hits)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    const WORKSPACE_MEMBERS: &[&str] = &[
        "lattice-ipc",
        "lattice-tui",
        "lattice-local",
        "lattice-server",
        "lattice-session",
        "lattice-conversation",
        "lattice-store",
        "lattice-llm",
        "lattice-profile",
        "lattice-agent",
        "plain-helper",
    ];

    #[derive(Clone, Default)]
    struct CrateSpec {
        features: String,
        deps: String,
        dev_deps: String,
        build_deps: String,
        extra: String,
    }

    impl CrateSpec {
        fn with_deps(deps: &str) -> Self {
            Self {
                deps: deps.to_owned(),
                ..Self::default()
            }
        }
    }

    fn baseline_spec(name: &str) -> CrateSpec {
        match name {
            "lattice-ipc" => CrateSpec::with_deps(concat!(
                "lattice-session = { path = \"../lattice-session\" }\n",
                "lattice-conversation = { path = \"../lattice-conversation\" }\n",
                "lattice-store = { path = \"../lattice-store\" }\n",
                "outside-util = { path = \"../../../outside-util\" }\n",
            )),
            "lattice-tui" => CrateSpec::with_deps(concat!(
                "lattice-local = { path = \"../lattice-local\" }\n",
                "outside-util = { path = \"../../../outside-util\" }\n",
            )),
            "lattice-local" => CrateSpec::with_deps(concat!(
                "lattice-ipc = { path = \"../lattice-ipc\" }\n",
                "lattice-conversation = { path = \"../lattice-conversation\" }\n",
                "lattice-store = { path = \"../lattice-store\" }\n",
                "outside-util = { path = \"../../../outside-util\" }\n",
            )),
            "lattice-server" => CrateSpec::with_deps(concat!(
                "lattice-ipc = { path = \"../lattice-ipc\" }\n",
                "lattice-session = { path = \"../lattice-session\" }\n",
                "lattice-conversation = { path = \"../lattice-conversation\" }\n",
                "lattice-store = { path = \"../lattice-store\" }\n",
                "lattice-llm = { path = \"../lattice-llm\" }\n",
                "lattice-profile = { path = \"../lattice-profile\" }\n",
                "lattice-agent = { path = \"../lattice-agent\" }\n",
                "plain-helper = { path = \"../plain-helper\" }\n",
                "outside-util = { path = \"../../../outside-util\" }\n",
            )),
            _ => CrateSpec::default(),
        }
    }

    fn push_section(out: &mut String, header: &str, body: &str) {
        if body.is_empty() {
            return;
        }
        out.push('\n');
        out.push_str(header);
        out.push('\n');
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
    }

    fn render_manifest(name: &str, spec: &CrateSpec) -> String {
        let mut out =
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
        push_section(&mut out, "[features]", &spec.features);
        push_section(&mut out, "[dependencies]", &spec.deps);
        push_section(&mut out, "[dev-dependencies]", &spec.dev_deps);
        push_section(&mut out, "[build-dependencies]", &spec.build_deps);
        if !spec.extra.is_empty() {
            out.push('\n');
            out.push_str(&spec.extra);
            if !spec.extra.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }

    fn write_crate_dir(dir: &Path, name: &str, spec: &CrateSpec) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "").unwrap();
        fs::write(dir.join("Cargo.toml"), render_manifest(name, spec)).unwrap();
    }

    struct TestWorkspace {
        _temp: TempDir,
        ws_root: PathBuf,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let ws_root = temp.path().join("ws");
            fs::create_dir_all(&ws_root).unwrap();
            Self::write_workspace_toml(&ws_root, WORKSPACE_MEMBERS);
            for &member in WORKSPACE_MEMBERS {
                write_crate_dir(
                    &ws_root.join("crates").join(member),
                    member,
                    &baseline_spec(member),
                );
            }
            write_crate_dir(
                &temp.path().join("outside-util"),
                "outside-util",
                &CrateSpec::default(),
            );
            Self {
                _temp: temp,
                ws_root,
            }
        }

        fn write_workspace_toml(ws_root: &Path, members: &[&str]) {
            let mut manifest = String::from("[workspace]\nresolver = \"3\"\nmembers = [\n");
            for member in members {
                manifest.push_str(&format!("  \"crates/{member}\",\n"));
            }
            manifest.push_str("]\n");
            fs::write(ws_root.join("Cargo.toml"), manifest).unwrap();
        }

        fn write_member(&self, name: &str, spec: &CrateSpec) {
            fs::write(
                self.ws_root.join("crates").join(name).join("Cargo.toml"),
                render_manifest(name, spec),
            )
            .unwrap();
            let _ = fs::remove_file(self.ws_root.join("Cargo.lock"));
        }

        fn reset_member(&self, name: &str) {
            self.write_member(name, &baseline_spec(name));
        }
    }

    /// 对指定的受检包逐一构造四种违规写法：普通依赖、build 依赖、改名依赖，以及 target 专属的可选依赖；验证生产 `RULES` 每次只命中 1 条。
    fn assert_direct_rule_rejects_all_forms(
        ws: &TestWorkspace,
        package: &str,
        forbidden_dep: &str,
        expected_policy_fragment: &str,
    ) {
        let mut normal_spec = baseline_spec(package);
        normal_spec.deps.push_str(&format!(
            "{forbidden_dep} = {{ path = \"../{forbidden_dep}\" }}\n"
        ));

        let mut build_spec = baseline_spec(package);
        build_spec.build_deps = format!("{forbidden_dep} = {{ path = \"../{forbidden_dep}\" }}\n");

        let mut renamed_spec = baseline_spec(package);
        renamed_spec.deps.push_str(&format!(
            "forbidden_alias = {{ package = \"{forbidden_dep}\", path = \"../{forbidden_dep}\" }}\n"
        ));

        let mut target_opt_spec = baseline_spec(package);
        target_opt_spec.extra = format!(
            "[target.'cfg(windows)'.dependencies]\n{forbidden_dep} = {{ path = \"../{forbidden_dep}\", optional = true }}\n"
        );

        let cases: &[(&str, CrateSpec, &[&str])] = &[
            (
                "普通依赖",
                normal_spec,
                &[
                    "[dependencies]",
                    &format!("出现不允许的内部依赖 '{forbidden_dep}'"),
                ],
            ),
            (
                "build 依赖",
                build_spec,
                &[
                    "[build-dependencies]",
                    &format!("出现不允许的内部依赖 '{forbidden_dep}'"),
                ],
            ),
            (
                "改名依赖",
                renamed_spec,
                &[
                    "[dependencies]",
                    &format!("出现不允许的内部依赖 '{forbidden_dep}'（改名为 'forbidden_alias'）"),
                ],
            ),
            (
                "target 专属 optional 依赖",
                target_opt_spec,
                &[
                    "[dependencies]",
                    "target 'cfg(windows)'",
                    "optional",
                    &format!("出现不允许的内部依赖 '{forbidden_dep}'"),
                ],
            ),
        ];

        for (label, spec, fragments) in cases {
            ws.write_member(package, spec);
            let hits = check_workspace_rules(&ws.ws_root, RULES).unwrap();
            assert_eq!(
                hits.len(),
                1,
                "{package} -> {forbidden_dep}（{label}）应命中且只命中 1 条，实际：{hits:?}"
            );
            assert!(
                hits[0].starts_with(&format!("{package}: ")),
                "{package}（{label}）报错应带受检包名，实际：{hits:?}"
            );
            assert!(
                hits[0].contains(expected_policy_fragment),
                "{package}（{label}）报错应带策略说明 '{expected_policy_fragment}'，实际：{hits:?}"
            );
            for fragment in *fragments {
                assert!(
                    hits[0].contains(fragment),
                    "{package}（{label}）报错缺少片段 '{fragment}'，实际：{hits:?}"
                );
            }
        }

        ws.reset_member(package);
    }

    fn assert_direct_normal_violation(ws: &TestWorkspace, package: &str, forbidden_dep: &str) {
        let mut spec = baseline_spec(package);
        spec.deps.push_str(&format!(
            "{forbidden_dep} = {{ path = \"../{forbidden_dep}\" }}\n"
        ));
        ws.write_member(package, &spec);
        let hits = check_workspace_rules(&ws.ws_root, RULES).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "{package} -> {forbidden_dep} 普通依赖应只命中 1 条，实际：{hits:?}"
        );
        assert!(
            hits[0].starts_with(&format!("{package}: ")),
            "报错应带受检包名 {package}，实际：{hits:?}"
        );
        assert!(
            hits[0].contains("[dependencies]"),
            "报错应带依赖段 [dependencies]，实际：{hits:?}"
        );
        assert!(
            hits[0].contains(&format!("'{forbidden_dep}'")),
            "报错应带违规依赖名 '{forbidden_dep}'，实际：{hits:?}"
        );
        ws.reset_member(package);
    }

    fn pkg_id(name: &str) -> String {
        format!("path+file:///ws/crates/{name}#0.1.0")
    }

    fn make_pkg_json(name: &str, source: Option<&str>) -> Value {
        let id = match source {
            Some(_) => {
                format!("registry+https://github.com/rust-lang/crates.io-index#{name}@1.0.0")
            }
            None => pkg_id(name),
        };
        json!({
            "name": name,
            "version": "0.1.0",
            "id": id,
            "license": null,
            "license_file": null,
            "description": null,
            "source": source,
            "dependencies": [],
            "targets": [{
                "kind": ["lib"],
                "crate_types": ["lib"],
                "name": name.replace('-', "_"),
                "src_path": format!("/ws/crates/{name}/src/lib.rs"),
                "edition": "2024",
                "doc": true,
                "doctest": true,
                "test": true
            }],
            "features": {},
            "manifest_path": format!("/ws/crates/{name}/Cargo.toml"),
            "metadata": null,
            "publish": null,
            "authors": [],
            "categories": [],
            "keywords": [],
            "readme": null,
            "repository": null,
            "homepage": null,
            "documentation": null,
            "edition": "2024",
            "links": null,
            "default_run": null,
            "rust_version": null
        })
    }

    /// 构造内存中的 `cargo_metadata::Metadata` 解析图夹具，用于验证环、悬空节点与边种类过滤。
    fn build_graph_metadata(
        workspace_packages: &[&str],
        external_packages: &[&str],
        edges: &[(&str, &str, &[Option<&str>])],
        include_resolve: bool,
    ) -> Metadata {
        let mut packages = Vec::new();
        let mut id_by_name = BTreeMap::new();
        let mut workspace_members = Vec::new();

        for &name in workspace_packages {
            let pkg = make_pkg_json(name, None);
            let id = pkg["id"].as_str().unwrap().to_owned();
            id_by_name.insert(name, id.clone());
            workspace_members.push(id);
            packages.push(pkg);
        }
        for &name in external_packages {
            let pkg = make_pkg_json(
                name,
                Some("registry+https://github.com/rust-lang/crates.io-index"),
            );
            let id = pkg["id"].as_str().unwrap().to_owned();
            id_by_name.insert(name, id);
            packages.push(pkg);
        }

        let resolve = if include_resolve {
            let mut deps_by_from: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
            for &(from, to, kinds) in edges {
                let to_id = id_by_name
                    .get(to)
                    .cloned()
                    .unwrap_or_else(|| format!("path+file:///dangling/{to}#0.1.0"));
                let dep_kinds: Vec<Value> = kinds
                    .iter()
                    .map(|k| json!({ "kind": k, "target": null }))
                    .collect();
                deps_by_from.entry(from).or_default().push(json!({
                    "name": to.replace('-', "_"),
                    "pkg": to_id,
                    "dep_kinds": dep_kinds
                }));
            }
            let nodes: Vec<Value> = id_by_name
                .iter()
                .map(|(&name, id)| {
                    let deps = deps_by_from.remove(name).unwrap_or_default();
                    let dependencies: Vec<Value> = deps.iter().map(|d| d["pkg"].clone()).collect();
                    json!({
                        "id": id,
                        "dependencies": dependencies,
                        "deps": deps,
                        "features": []
                    })
                })
                .collect();
            json!({ "nodes": nodes, "root": null })
        } else {
            Value::Null
        };

        serde_json::from_value(json!({
            "packages": packages,
            "workspace_members": workspace_members,
            "workspace_default_members": workspace_members,
            "resolve": resolve,
            "target_directory": "/ws/target",
            "version": 1,
            "workspace_root": "/ws",
            "metadata": null
        }))
        .expect("构造测试 Metadata 夹具失败")
    }

    #[test]
    fn direct_baseline_and_dev_only_dependencies_pass_rules() {
        let ws = TestWorkspace::new();
        let baseline_hits = check_workspace_rules(&ws.ws_root, RULES).unwrap();
        assert!(
            baseline_hits.is_empty(),
            "基线状态下每个受检包只依赖允许成员与外部 path crate，结果应为空：{baseline_hits:?}"
        );

        // 白名单成员可以改名或设为可选；禁用成员只出现在 `[dev-dependencies]` 时，每条生产规则都不报错。
        let mut ipc_spec = CrateSpec::with_deps(concat!(
            "lattice-session = { path = \"../lattice-session\" }\n",
            "conv_alias = { package = \"lattice-conversation\", path = \"../lattice-conversation\" }\n",
            "lattice-store = { path = \"../lattice-store\", optional = true }\n",
            "outside-util = { path = \"../../../outside-util\" }\n",
        ));
        ipc_spec.dev_deps = concat!(
            "lattice-llm = { path = \"../lattice-llm\" }\n",
            "plain-helper = { path = \"../plain-helper\" }\n",
        )
        .to_owned();
        ws.write_member("lattice-ipc", &ipc_spec);

        let mut tui_spec = baseline_spec("lattice-tui");
        tui_spec.dev_deps = concat!(
            "lattice-server = { path = \"../lattice-server\" }\n",
            "plain-helper = { path = \"../plain-helper\" }\n",
        )
        .to_owned();
        ws.write_member("lattice-tui", &tui_spec);

        let mut local_spec = baseline_spec("lattice-local");
        local_spec.dev_deps = concat!(
            "lattice-session = { path = \"../lattice-session\" }\n",
            "lattice-server = { path = \"../lattice-server\" }\n",
            "plain-helper = { path = \"../plain-helper\" }\n",
        )
        .to_owned();
        ws.write_member("lattice-local", &local_spec);

        let mut server_spec = baseline_spec("lattice-server");
        server_spec.dev_deps = "lattice-local = { path = \"../lattice-local\" }\n".to_owned();
        ws.write_member("lattice-server", &server_spec);

        let dev_hits = check_workspace_rules(&ws.ws_root, RULES).unwrap();
        assert!(
            dev_hits.is_empty(),
            "只作 dev 依赖时生产 RULES 不应报错，实际：{dev_hits:?}"
        );
    }

    #[test]
    fn direct_rule_lattice_ipc_rejects_unlisted_workspace_dependencies() {
        let ws = TestWorkspace::new();
        assert_direct_rule_rejects_all_forms(
            &ws,
            "lattice-ipc",
            "lattice-llm",
            "允许的内部依赖：[lattice-session, lattice-conversation, lattice-store]",
        );
        assert_direct_normal_violation(&ws, "lattice-ipc", "plain-helper");
    }

    #[test]
    fn direct_rule_lattice_tui_rejects_unlisted_workspace_dependencies() {
        let ws = TestWorkspace::new();
        assert_direct_rule_rejects_all_forms(
            &ws,
            "lattice-tui",
            "lattice-server",
            "允许的内部依赖：[lattice-local]",
        );
        assert_direct_normal_violation(&ws, "lattice-tui", "plain-helper");
    }

    #[test]
    fn direct_rule_lattice_local_rejects_unlisted_workspace_dependencies() {
        let ws = TestWorkspace::new();
        assert_direct_rule_rejects_all_forms(
            &ws,
            "lattice-local",
            "lattice-session",
            "允许的内部依赖：[lattice-ipc, lattice-conversation, lattice-store]",
        );
        assert_direct_normal_violation(&ws, "lattice-local", "plain-helper");
    }

    #[test]
    fn direct_rule_lattice_server_rejects_lattice_local_dependency() {
        let ws = TestWorkspace::new();
        assert_direct_rule_rejects_all_forms(
            &ws,
            "lattice-server",
            "lattice-local",
            "禁止的内部依赖：[lattice-local]",
        );
    }

    #[test]
    fn errors_when_checked_package_missing_or_dependency_unidentifiable() {
        let ws = TestWorkspace::new();

        let mut metadata = workspace_metadata::load_no_deps(&ws.ws_root).unwrap();
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

        let members_without_server: Vec<&str> = WORKSPACE_MEMBERS
            .iter()
            .copied()
            .filter(|&m| m != "lattice-server")
            .collect();
        TestWorkspace::write_workspace_toml(&ws.ws_root, &members_without_server);
        let err = check_workspace_rules(&ws.ws_root, RULES).unwrap_err();
        assert!(
            err.to_string().contains("受检包 'lattice-server'"),
            "报错应指出缺失的受检包：{err}"
        );
    }

    #[test]
    fn closure_workspace_allows_baseline_and_dev_only_forbidden_dependencies() {
        let ws = TestWorkspace::new();

        let mut tui_spec = baseline_spec("lattice-tui");
        tui_spec.dev_deps = concat!(
            "lattice-server = { path = \"../lattice-server\" }\n",
            "lattice-agent = { path = \"../lattice-agent\" }\n",
        )
        .to_owned();
        ws.write_member("lattice-tui", &tui_spec);

        let mut local_spec = baseline_spec("lattice-local");
        local_spec.dev_deps = concat!(
            "lattice-server = { path = \"../lattice-server\" }\n",
            "lattice-llm = { path = \"../lattice-llm\" }\n",
            "lattice-profile = { path = \"../lattice-profile\" }\n",
        )
        .to_owned();
        ws.write_member("lattice-local", &local_spec);

        let mut ipc_spec = baseline_spec("lattice-ipc");
        ipc_spec.dev_deps = "lattice-llm = { path = \"../lattice-llm\" }\n".to_owned();
        ws.write_member("lattice-ipc", &ipc_spec);

        let hits = check_workspace_normal_closure(&ws.ws_root, CLOSURE_RULES).unwrap();
        assert!(
            hits.is_empty(),
            "只经 dev 依赖引入禁用 crate 时普通依赖闭包不应命中，实际：{hits:?}"
        );
    }

    #[test]
    fn closure_rule_lattice_local_rejects_non_default_optional_dependency() {
        let ws = TestWorkspace::new();
        // 断开 `lattice-tui -> lattice-local`，让本用例只检验 `CLOSURE_RULES` 中 `lattice-local` 的规则与 `--all-features` 的读取。
        ws.write_member(
            "lattice-tui",
            &CrateSpec::with_deps("outside-util = { path = \"../../../outside-util\" }\n"),
        );

        let mut local_spec = baseline_spec("lattice-local");
        local_spec.features = "default = []\nextra-llm = [\"dep:lattice-llm\"]\n".to_owned();
        local_spec
            .deps
            .push_str("lattice-llm = { path = \"../lattice-llm\", optional = true }\n");
        ws.write_member("lattice-local", &local_spec);

        let hits = check_workspace_normal_closure(&ws.ws_root, CLOSURE_RULES).unwrap();
        assert_eq!(
            hits,
            vec![
                "lattice-local: 普通依赖闭包中可达禁止的包 'lattice-llm'，路径：lattice-local -> lattice-llm"
            ],
            "仅经非默认 feature 启用的 optional 依赖必须被 --all-features 闭包检出"
        );
    }

    #[test]
    fn closure_rule_lattice_local_rejects_inactive_target_specific_dependency() {
        let ws = TestWorkspace::new();
        ws.write_member(
            "lattice-tui",
            &CrateSpec::with_deps("outside-util = { path = \"../../../outside-util\" }\n"),
        );

        let inactive_target = if cfg!(windows) {
            "cfg(unix)"
        } else {
            "cfg(windows)"
        };
        let mut local_spec = baseline_spec("lattice-local");
        local_spec.extra = format!(
            "[target.'{inactive_target}'.dependencies]\nlattice-profile = {{ path = \"../lattice-profile\" }}\n"
        );
        ws.write_member("lattice-local", &local_spec);

        let hits = check_workspace_normal_closure(&ws.ws_root, CLOSURE_RULES).unwrap();
        assert_eq!(
            hits,
            vec![
                "lattice-local: 普通依赖闭包中可达禁止的包 'lattice-profile'，路径：lattice-local -> lattice-profile"
            ],
            "仅经非当前平台 target 专属依赖引入的禁用 crate 必须被闭包检出"
        );
    }

    #[test]
    fn closure_rule_lattice_tui_rejects_renamed_dependency() {
        let ws = TestWorkspace::new();
        let mut tui_spec = baseline_spec("lattice-tui");
        tui_spec.deps.push_str(
            "agent_alias = { package = \"lattice-agent\", path = \"../lattice-agent\" }\n",
        );
        ws.write_member("lattice-tui", &tui_spec);

        let hits = check_workspace_normal_closure(&ws.ws_root, CLOSURE_RULES).unwrap();
        assert_eq!(
            hits,
            vec![
                "lattice-tui: 普通依赖闭包中可达禁止的包 'lattice-agent'，路径：lattice-tui -> lattice-agent"
            ],
            "经改名依赖引入的禁用 crate 必须被 lattice-tui 闭包规则检出"
        );
    }

    #[test]
    fn closure_workspace_reports_full_path_through_intermediate_workspace_members() {
        let ws = TestWorkspace::new();
        // 清空 `lattice-server` 自己的禁用依赖，只核对经中间成员传递到 `lattice-server` 的完整路径。
        ws.write_member("lattice-server", &CrateSpec::default());

        let mut ipc_spec = baseline_spec("lattice-ipc");
        ipc_spec
            .deps
            .push_str("plain-helper = { path = \"../plain-helper\" }\n");
        ws.write_member("lattice-ipc", &ipc_spec);

        ws.write_member(
            "plain-helper",
            &CrateSpec::with_deps("lattice-server = { path = \"../lattice-server\" }\n"),
        );

        let hits = check_workspace_normal_closure(&ws.ws_root, CLOSURE_RULES).unwrap();
        assert_eq!(
            hits.len(),
            2,
            "lattice-tui 与 lattice-local 两条闭包规则都应命中传递违规：{hits:?}"
        );
        assert!(
            hits.iter().any(|h| {
                h == "lattice-local: 普通依赖闭包中可达禁止的包 'lattice-server'，路径：lattice-local -> lattice-ipc -> plain-helper -> lattice-server"
            }),
            "应写出从 lattice-local 经中间 workspace 成员到达 lattice-server 的完整路径，实际：{hits:?}"
        );
        assert!(
            hits.iter().any(|h| {
                h == "lattice-tui: 普通依赖闭包中可达禁止的包 'lattice-server'，路径：lattice-tui -> lattice-local -> lattice-ipc -> plain-helper -> lattice-server"
            }),
            "应写出从 lattice-tui 经中间 workspace 成员到达 lattice-server 的完整路径，实际：{hits:?}"
        );
    }

    #[test]
    fn closure_reports_transitive_path_through_external_intermediate_node() {
        let metadata = build_graph_metadata(
            &["lattice-tui", "lattice-local", "lattice-llm"],
            &["external-bridge"],
            &[
                ("lattice-tui", "lattice-local", &[None]),
                ("lattice-local", "external-bridge", &[None]),
                ("external-bridge", "lattice-llm", &[None]),
            ],
            true,
        );
        let hits = check_metadata_normal_closure(&metadata, CLOSURE_RULES).unwrap();
        assert_eq!(
            hits.len(),
            2,
            "tui 与 local 两个根都应报告违规路径：{hits:?}"
        );
        assert!(
            hits.iter()
                .any(|h| h
                    .contains("lattice-tui -> lattice-local -> external-bridge -> lattice-llm")),
            "应报告完整路径 tui -> local -> 中间节点 -> llm，实际：{hits:?}"
        );
        assert!(
            hits.iter()
                .any(|h| h.contains("lattice-local -> external-bridge -> lattice-llm")),
            "应报告 local 起始路径，实际：{hits:?}"
        );
    }

    #[test]
    fn closure_handles_cycles_with_or_without_forbidden_packages() {
        // 环中含禁止包：local -> cycle-a -> lattice-profile -> cycle-a
        let in_cycle = build_graph_metadata(
            &["lattice-tui", "lattice-local", "lattice-profile"],
            &["cycle-a"],
            &[
                ("lattice-tui", "lattice-local", &[None]),
                ("lattice-local", "cycle-a", &[None]),
                ("cycle-a", "lattice-profile", &[None]),
                ("lattice-profile", "cycle-a", &[None]),
            ],
            true,
        );
        let hits = check_metadata_normal_closure(&in_cycle, CLOSURE_RULES).unwrap();
        assert!(
            hits.iter()
                .any(|h| h.contains("lattice-tui -> lattice-local -> cycle-a -> lattice-profile")),
            "环内禁止包应被检出并给出路径，实际：{hits:?}"
        );

        // 环后含禁止包：local -> cycle-a -> cycle-b -> cycle-a，且 cycle-b -> tail -> lattice-agent
        let after_cycle = build_graph_metadata(
            &["lattice-tui", "lattice-local", "lattice-agent"],
            &["cycle-a", "cycle-b", "tail"],
            &[
                ("lattice-tui", "lattice-local", &[None]),
                ("lattice-local", "cycle-a", &[None]),
                ("cycle-a", "cycle-b", &[None]),
                ("cycle-b", "cycle-a", &[None]),
                ("cycle-b", "tail", &[None]),
                ("tail", "lattice-agent", &[None]),
            ],
            true,
        );
        let hits = check_metadata_normal_closure(&after_cycle, CLOSURE_RULES).unwrap();
        assert!(
            hits.iter().any(|h| h.contains(
                "lattice-tui -> lattice-local -> cycle-a -> cycle-b -> tail -> lattice-agent"
            )),
            "环后禁止包应被检出并给出路径，实际：{hits:?}"
        );

        // 无禁止包的环必须正常终止且无命中
        let clean_cycle = build_graph_metadata(
            &["lattice-tui", "lattice-local"],
            &["cycle-a", "cycle-b"],
            &[
                ("lattice-tui", "lattice-local", &[None]),
                ("lattice-local", "cycle-a", &[None]),
                ("cycle-a", "cycle-b", &[None]),
                ("cycle-b", "cycle-a", &[None]),
            ],
            true,
        );
        let hits = check_metadata_normal_closure(&clean_cycle, CLOSURE_RULES).unwrap();
        assert!(hits.is_empty(), "无禁止包的环应干净终止，实际：{hits:?}");
    }

    #[test]
    fn closure_ignores_dev_and_build_only_edges_and_follows_mixed_normal_dev_edges() {
        // 只含 dev 或只含 build 的边不进入普通依赖闭包；`lattice-session` 与 `redb` 允许出现在普通依赖闭包中。
        let clean = build_graph_metadata(
            &[
                "lattice-tui",
                "lattice-local",
                "lattice-ipc",
                "lattice-session",
                "lattice-server",
                "lattice-llm",
            ],
            &["redb"],
            &[
                ("lattice-tui", "lattice-local", &[None]),
                ("lattice-local", "lattice-ipc", &[None]),
                ("lattice-ipc", "lattice-session", &[None]),
                ("lattice-session", "redb", &[None]),
                ("lattice-local", "lattice-server", &[Some("dev")]),
                ("lattice-ipc", "lattice-llm", &[Some("build")]),
            ],
            true,
        );
        let hits = check_metadata_normal_closure(&clean, CLOSURE_RULES).unwrap();
        assert!(
            hits.is_empty(),
            "只含 dev 或只含 build 的边不应进入普通依赖闭包，lattice-session 与 redb 应被允许，实际：{hits:?}"
        );

        // 同时含 normal 与 dev 的边进入普通依赖闭包。
        let mixed = build_graph_metadata(
            &["lattice-tui", "lattice-local", "lattice-server"],
            &[],
            &[
                ("lattice-tui", "lattice-local", &[None]),
                ("lattice-local", "lattice-server", &[Some("dev"), None]),
            ],
            true,
        );
        let hits = check_metadata_normal_closure(&mixed, CLOSURE_RULES).unwrap();
        assert_eq!(
            hits.len(),
            2,
            "同时含 normal 与 dev 的边应进入普通依赖闭包，实际：{hits:?}"
        );
        assert!(
            hits.iter()
                .any(|h| h.contains("lattice-tui -> lattice-local -> lattice-server")),
            "混合边闭包应报告从 lattice-tui 到 lattice-server 的完整路径，实际：{hits:?}"
        );
    }

    #[test]
    fn closure_errors_on_missing_resolve_missing_package_or_dangling_node() {
        let missing_resolve = build_graph_metadata(
            &["lattice-tui", "lattice-local"],
            &[],
            &[("lattice-tui", "lattice-local", &[None])],
            false,
        );
        let err = check_metadata_normal_closure(&missing_resolve, CLOSURE_RULES).unwrap_err();
        assert!(
            err.to_string().contains("缺少依赖解析图"),
            "报错应指出缺少 resolve：{err}"
        );

        let missing_pkg = build_graph_metadata(&["lattice-tui"], &[], &[], true);
        let err = check_metadata_normal_closure(&missing_pkg, CLOSURE_RULES).unwrap_err();
        assert!(
            err.to_string().contains("受检包 'lattice-local'"),
            "报错应指出缺失的受检包：{err}"
        );

        let dangling = build_graph_metadata(
            &["lattice-tui", "lattice-local"],
            &[],
            &[
                ("lattice-tui", "lattice-local", &[None]),
                ("lattice-local", "ghost-node", &[None]),
            ],
            true,
        );
        let err = check_metadata_normal_closure(&dangling, CLOSURE_RULES).unwrap_err();
        assert!(
            err.to_string().contains("悬空节点"),
            "报错应指出悬空节点：{err}"
        );
    }
}
