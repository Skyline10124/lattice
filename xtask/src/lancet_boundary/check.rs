// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use cargo_metadata::{DependencyKind, MetadataCommand};

use crate::scan_rs_tree::{LineHit, collect_rs_line_hits};

/// `lancet-audit` owns operator ledger reads without composing the Tool runtime.
/// Keeping the runtime dependency absent makes that boundary structural.
const AUDIT_FORBIDDEN_DEPS: &[&str] = &["lancet-runtime"];

/// `lancet-audit` owns the operator audit read path (#39), so an operator read
/// never composing the Tool runtime is a dependency-graph property.
///
/// Dev-dependencies count: a test-only edge still puts the types one `use` away
/// from the production module in the same crate.
pub fn lancet_audit_forbidden_dependency_graph(repo_root: &Path) -> anyhow::Result<Vec<LineHit>> {
    let mut parts = Vec::new();

    let metadata = MetadataCommand::new()
        .current_dir(repo_root)
        .no_deps()
        .exec()
        .map_err(|e| anyhow::anyhow!("cargo metadata --no-deps --format-version 1 failed: {e}"))?;

    let audit = metadata
        .packages
        .iter()
        .find(|pkg| pkg.name == "lancet-audit")
        .ok_or_else(|| anyhow::anyhow!("lancet-audit is missing from the workspace metadata"))?;

    for dep in &audit.dependencies {
        if AUDIT_FORBIDDEN_DEPS.contains(&dep.name.as_str()) {
            parts.push(format!(
                "lancet-audit -> {} ({:?}) reopens the Tool runtime path",
                dep.name, dep.kind
            ));
        }
    }

    let manifest = repo_root.join("crates/lancet-audit/Cargo.toml");
    let manifest_text = std::fs::read_to_string(&manifest)?;
    for (line_no, line) in manifest_text.lines().enumerate() {
        if AUDIT_FORBIDDEN_DEPS
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

/// Symbols the #139-B hard cut deletes (plan §5 Commit 1 / §5 Commit 9).
///
/// The needles deliberately include comments and doc text: after the cut,
/// even a mention of the retired vocabulary in production code is residue,
/// because a reader cannot tell a stale comment from a live path without
/// opening every hit.
pub const ISSUE139B_LEGACY_NEEDLES: &[&str] = &[
    "AgentLoop",
    "CepAuditStore",
    "AuditBacking",
    "ProvisionalCompletionHook",
    "ToolCallExecutor",
    "legacy_status",
    "call_id: u64",
    "evidence_id",
    // The retired numeric JSON-RPC request-id allocator, replaced by
    // caller-allocated `InvocationId`.
    "next_id",
];

/// Forbidden normal/dev dependency edges of the #139-B composition (plan
/// §2.1). `lancet-audit -> lattice-session` as a *dev*-dependency is the
/// documented #139-A qualification seam and stays legal until that harness
/// is deleted; a normal edge would make LATTICE session types nameable from
/// the audit production surface and break LANCET's independent
/// embeddability.
pub fn issue139b_forbidden_dependency_graph(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    let metadata = MetadataCommand::new()
        .current_dir(repo_root)
        .no_deps()
        .exec()
        .map_err(|e| anyhow::anyhow!("cargo metadata --no-deps --format-version 1 failed: {e}"))?;

    let mut parts = Vec::new();
    for pkg in &metadata.packages {
        for dep in &pkg.dependencies {
            let forbidden = match (pkg.name.as_str(), dep.name.as_str()) {
                ("lancet-audit", "lattice-session") => dep.kind == DependencyKind::Normal,
                ("lattice-session", "lancet-audit") => true,
                _ => false,
            };
            if forbidden {
                parts.push(format!(
                    "{} -> {} ({:?}) is a forbidden #139-B edge",
                    pkg.name, dep.name, dep.kind
                ));
            }
        }
    }
    Ok(parts)
}

/// Every legacy-symbol hit fails permanently after the Commit 7 hard cut.
pub fn issue139b_legacy_residue_hits(repo_root: &Path) -> anyhow::Result<Vec<LineHit>> {
    let mut hits = Vec::new();
    for needle in ISSUE139B_LEGACY_NEEDLES {
        hits.extend(scan_workspace_rust_targets(repo_root, needle)?);
    }
    hits.sort();
    hits.dedup();
    Ok(hits)
}

fn scan_workspace_rust_targets(repo_root: &Path, needle: &str) -> anyhow::Result<Vec<LineHit>> {
    let mut hits = Vec::new();
    for root in ["crates", "tools"] {
        let path = repo_root.join(root);
        if path.is_dir() {
            hits.extend(collect_rs_line_hits(
                &path,
                repo_root,
                |_| false,
                |line| line.contains(needle),
            )?);
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn joined(hits: &[LineHit]) -> String {
        hits.join("\n")
    }

    #[test]
    fn issue139b_needles_cover_the_plan_residue_set() {
        for required in [
            "AgentLoop",
            "CepAuditStore",
            "AuditBacking",
            "ProvisionalCompletionHook",
            "ToolCallExecutor",
            "legacy_status",
            "call_id: u64",
            "evidence_id",
            "next_id",
        ] {
            assert!(
                ISSUE139B_LEGACY_NEEDLES.contains(&required),
                "missing plan §5 Commit 9 needle: {required}"
            );
        }
    }

    #[test]
    fn issue139b_residue_scan_reports_every_hit() {
        let dir = TempDir::new().unwrap();
        let hit = dir.path().join("crates/lattice-api/src/turn/fresh.rs");
        fs::create_dir_all(hit.parent().unwrap()).unwrap();
        fs::write(&hit, "fn x() { let _ = AgentLoop; }\n").unwrap();
        let hits = issue139b_legacy_residue_hits(dir.path()).unwrap();
        assert!(joined(&hits).contains("fresh.rs"));
    }

    #[test]
    fn issue139b_residue_scan_covers_build_scripts_and_tools() {
        let dir = TempDir::new().unwrap();
        let build = dir.path().join("crates/example/build.rs");
        let guest = dir.path().join("tools/example/src/lib.rs");
        fs::create_dir_all(build.parent().unwrap()).unwrap();
        fs::create_dir_all(guest.parent().unwrap()).unwrap();
        fs::write(&build, "fn main() { let _ = AgentLoop; }\n").unwrap();
        fs::write(&guest, "fn x() { let _ = next_id; }\n").unwrap();
        let text = joined(&issue139b_legacy_residue_hits(dir.path()).unwrap());
        assert!(text.contains("build.rs"));
        assert!(text.contains("tools/example/src/lib.rs"));
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
