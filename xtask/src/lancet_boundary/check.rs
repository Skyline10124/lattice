// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use cargo_metadata::{DependencyKind, MetadataCommand};

type LineHit = String;

/// 会话库的操作员读取不能组合 Tool runtime；开发依赖也不能引入其类型。
const SESSION_FORBIDDEN_DEPS: &[&str] = &["lancet-runtime"];

pub fn session_forbidden_dependency_graph(repo_root: &Path) -> anyhow::Result<Vec<LineHit>> {
    let mut parts = Vec::new();

    let metadata = MetadataCommand::new()
        .current_dir(repo_root)
        .no_deps()
        .exec()
        .map_err(|e| anyhow::anyhow!("cargo metadata --no-deps --format-version 1 failed: {e}"))?;

    let session = metadata
        .packages
        .iter()
        .find(|pkg| pkg.name == "lattice-session")
        .ok_or_else(|| anyhow::anyhow!("lattice-session is missing from the workspace metadata"))?;

    for dep in &session.dependencies {
        if SESSION_FORBIDDEN_DEPS.contains(&dep.name.as_str()) {
            parts.push(format!(
                "lattice-session -> {} ({:?}) reopens the Tool runtime path",
                dep.name, dep.kind
            ));
        }
    }

    let manifest = repo_root.join("crates/lattice-session/Cargo.toml");
    let manifest_text = std::fs::read_to_string(&manifest)?;
    for (line_no, line) in manifest_text.lines().enumerate() {
        if SESSION_FORBIDDEN_DEPS
            .iter()
            .any(|forbidden| line.contains(forbidden))
        {
            parts.push(format!("{}:{}", line_no + 1, line));
        }
    }

    Ok(parts)
}

/// A test-only feature enabled from a production `[dependencies]` entry cancels
/// the `#[cfg(feature = "...")]` gate it is supposed to respect: the helpers
/// ship in the release binary and in every downstream dependency graph. There
/// is never a need for it — `#[cfg(test)]` unit tests already see
/// `[dev-dependencies]`, which is where `lattice-cli` reaches
/// `init_fixture_repo` from after #88.
///
/// Keyed on the feature-name prefix rather than an enumerated list, so a new
/// `test-*` feature on any crate is covered without touching this gate.
pub fn production_test_feature_hits(repo_root: &Path) -> anyhow::Result<Vec<LineHit>> {
    let metadata = MetadataCommand::new()
        .current_dir(repo_root)
        .no_deps()
        .exec()
        .map_err(|e| anyhow::anyhow!("cargo metadata --no-deps --format-version 1 failed: {e}"))?;

    let packages: std::collections::BTreeMap<_, _> = metadata
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
            if feature_reaches_test_only(
                &pkg.features,
                feature,
                &mut std::collections::BTreeSet::new(),
            ) {
                parts.push(format!(
                    "{}: production feature '{feature}' forwards to a test-only feature",
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
                        feature_reaches_test_only(
                            &package.features,
                            feature,
                            &mut std::collections::BTreeSet::new(),
                        )
                    })
                {
                    flagged.push(feature.as_str());
                }
            }
            if dep.uses_default_features
                && target.is_some_and(|package| {
                    feature_reaches_test_only(
                        &package.features,
                        "default",
                        &mut std::collections::BTreeSet::new(),
                    )
                })
            {
                flagged.push("default");
            }
            if !flagged.is_empty() {
                parts.push(format!(
                    "{}: [dependencies] {} enables test-only feature(s) {}; move the feature to the [dev-dependencies] entry",
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
    features: &std::collections::BTreeMap<String, Vec<String>>,
    feature: &str,
    visiting: &mut std::collections::BTreeSet<String>,
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

    fn joined(hits: &[LineHit]) -> String {
        hits.join("\n")
    }

    fn write_session_workspace(root: &Path, dependencies: &str) {
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/lattice-session\", \"crates/lancet-runtime\"]\n",
        )
        .unwrap();
        for name in ["lattice-session", "lancet-runtime"] {
            let path = root.join("crates").join(name);
            fs::create_dir_all(path.join("src")).unwrap();
            fs::write(path.join("src/lib.rs"), "").unwrap();
            let deps = if name == "lattice-session" {
                dependencies
            } else {
                ""
            };
            fs::write(
                path.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n{deps}",
                ),
            )
            .unwrap();
        }
    }

    #[test]
    fn session_without_runtime_dependency_is_clean() {
        let dir = TempDir::new().unwrap();
        write_session_workspace(dir.path(), "");
        let hits = session_forbidden_dependency_graph(dir.path()).unwrap();
        assert!(hits.is_empty(), "runtime-free session rejected: {hits:?}");
    }

    fn assert_session_runtime_dependency_rejected(section: &str, kind: &str) {
        let dir = TempDir::new().unwrap();
        write_session_workspace(
            dir.path(),
            &format!("[{section}]\nlancet-runtime = {{ path = \"../lancet-runtime\" }}\n"),
        );
        let hits = session_forbidden_dependency_graph(dir.path()).unwrap();
        assert!(
            joined(&hits).contains(&format!("lattice-session -> lancet-runtime ({kind})")),
            "session runtime edge not flagged: {hits:?}"
        );
    }

    #[test]
    fn session_normal_runtime_dependency_is_rejected() {
        assert_session_runtime_dependency_rejected("dependencies", "Normal");
    }

    #[test]
    fn session_dev_runtime_dependency_is_rejected() {
        assert_session_runtime_dependency_rejected("dev-dependencies", "Development");
    }

    /// A two-crate virtual workspace where the consumer enables `test-utils`
    /// from both dependency sections. Only the `[dependencies]` entry is a
    /// finding; the `[dev-dependencies]` one is the fix #88 applied.
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
            "production test-utils not flagged: {text}"
        );
        assert_eq!(
            hits.len(),
            1,
            "dev-dependencies entry must not count: {text}"
        );
    }

    #[test]
    fn dev_only_test_feature_is_clean() {
        let dir = TempDir::new().unwrap();
        write_test_feature_workspace(dir.path(), "");
        let hits = production_test_feature_hits(dir.path()).unwrap();
        assert!(hits.is_empty(), "dev-only test-utils flagged: {hits:?}");
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
        assert!(joined(&hits).contains("production feature 'release'"));
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
