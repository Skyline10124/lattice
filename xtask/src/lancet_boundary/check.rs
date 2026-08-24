// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use cargo_metadata::{DependencyKind, MetadataCommand};
use regex::Regex;

use crate::scan_rs_tree::{LineHit, collect_rs_line_hits};

pub const DEFAULT_TOOL_EXECUTOR_ALLOWLIST: &[&str] = &[
    "crates/lancet-native/src/executor/",
    "crates/lancet-native/src/lib.rs",
];

pub const DEFAULT_TOOL_DEFINITIONS_ALLOWLIST: &[&str] = &[
    "crates/lancet-native/src/tool_definitions.rs",
    "crates/lancet-native/src/lib.rs",
    "crates/lattice-api/tests/usage_turn.rs",
];

pub const COMPOSITE_EXECUTOR_ALLOWLIST: &[&str] = &[
    "crates/lancet-native/src/registry.rs",
    "crates/lancet-native/src/transport.rs",
];

const CLI_PRODUCTION_ALLOWLIST: &[&str] = &["crates/lattice-cli/src/commands/run_tests.rs"];

/// Crates whose presence in `lancet-audit`'s dependency graph would make CEP
/// server, transport, and registry types nameable from the operator audit read
/// path.
const AUDIT_FORBIDDEN_DEPS: &[&str] = &[
    "lancet-native",
    "lancet-dispatch",
    "lancet-cep",
    "lancet-runtime",
];

/// `lancet-audit` owns the operator audit read path (#39), so "an operator read
/// never composes a CEP server" is a dependency-graph property: the server,
/// transport, and registry types simply are not nameable there.
///
/// This gate is what makes that structural. The source-string-scan tests that
/// used to assert it (`operator_audit_module_does_not_reference_cep_server_types`,
/// `audit_command_source_does_not_reference_cep_server`) were deleted under
/// RFC #41 decision 6, and the triage ledger records their replacement as
/// "架构边界由 crate 依赖/边界检查负责" — this check is that owner.
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
                "lancet-audit -> {} ({:?}) reopens the CEP-server path",
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

pub fn lattice_agent_forbidden_dependency_graph(repo_root: &Path) -> anyhow::Result<Vec<LineHit>> {
    let mut parts = Vec::new();

    let metadata = MetadataCommand::new()
        .current_dir(repo_root)
        .no_deps()
        .exec()
        .map_err(|e| anyhow::anyhow!("cargo metadata --no-deps --format-version 1 failed: {e}"))?;

    for pkg in &metadata.packages {
        if pkg.name != "lattice-agent" {
            continue;
        }
        for dep in &pkg.dependencies {
            if dep.name == "lancet-native" {
                parts.push(format!("lattice-agent -> {}", dep.name));
            }
        }
    }

    let manifest = repo_root.join("crates/lattice-agent/Cargo.toml");
    let manifest_text = std::fs::read_to_string(&manifest)?;
    for (line_no, line) in manifest_text.lines().enumerate() {
        if line.contains("lancet-native") {
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

    let mut parts = Vec::new();
    for pkg in &metadata.packages {
        for dep in &pkg.dependencies {
            if dep.kind != DependencyKind::Normal {
                continue;
            }
            let flagged: Vec<&str> = dep
                .features
                .iter()
                .map(String::as_str)
                .filter(|feature| feature.starts_with("test-"))
                .collect();
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

pub fn scan_with_allowlist(
    repo_root: &Path,
    needle: &str,
    allowlist: &[&str],
) -> anyhow::Result<Vec<LineHit>> {
    let hits = scan_crates_source_and_tests(repo_root, needle)?;
    Ok(filter_allowlist(&hits, allowlist))
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
    // The numeric JSON-RPC request-id allocator in `lancet-dispatch`
    // transport, replaced by caller-allocated `InvocationId`.
    "next_id",
];

/// Temporary allowlist: every pre-#139-B file that still carries a legacy
/// needle (plan §5 Commit 1).
///
/// This list is the machine-tracked residue ledger of the hard cut:
///
/// - every entry must name an existing file or directory, so deleting a
///   legacy file forces its entry out (stale entries fail the gate);
/// - Commit 7 must empty the list; from then on any hit anywhere under
///   `crates/**/{src,tests}` fails CI.
pub const ISSUE139B_LEGACY_ALLOWLIST: &[&str] = &[
    "crates/lancet-api/src/audit.rs",
    "crates/lancet-api/src/config.rs",
    "crates/lancet-api/src/lib.rs",
    "crates/lancet-api/src/rewind.rs",
    "crates/lancet-api/src/runtime.rs",
    "crates/lancet-api/src/status.rs",
    "crates/lancet-audit/src/operator.rs",
    "crates/lancet-audit/src/operator_tests.rs",
    "crates/lancet-authority/tests/guard_decide_fuzz.rs",
    "crates/lancet-dispatch/src/registry.rs",
    "crates/lancet-dispatch/src/transport.rs",
    "crates/lancet-native/src/effect_contract_tests/support.rs",
    "crates/lancet-native/src/enforcement/guard_l1.rs",
    "crates/lancet-native/src/handlers/external.rs",
    "crates/lancet-native/src/handlers/scaffold.rs",
    "crates/lancet-native/src/lib.rs",
    "crates/lancet-native/src/server/registry.rs",
    "crates/lancet-native/src/server/transport.rs",
    "crates/lancet-native/tests/cep_component_round_trip.rs",
    "crates/lancet-native/tests/cep_warn_harness.rs",
    "crates/lancet-runtime/src/history.rs",
    "crates/lancet-runtime/src/host_mutate.rs",
    "crates/lancet-runtime/src/runtime.rs",
    "crates/lattice-api/src/turn/cep.rs",
    "crates/lattice-api/src/turn/cep_tests.rs",
    "crates/lattice-api/src/turn/live.rs",
    "crates/lattice-api/src/turn/live_tests.rs",
    "crates/lattice-api/src/turn/types.rs",
    "crates/lattice-api/tests/cep_round_trip.rs",
    "crates/lattice-api/tests/direct_local_acceptance.rs",
    "crates/lattice-api/tests/dogfood_impact_matrix.rs",
    "crates/lattice-api/tests/release_e2e.rs",
    "crates/lattice-api/tests/rewind_acceptance.rs",
    "crates/lattice-api/tests/usage_turn.rs",
    "crates/lattice-api/tests/workspace_history_acceptance.rs",
    // Black-box negative assertion proving SQLite-only evidence identity is absent.
    "crates/lattice-system-harness/tests/release_operator.rs",
    "crates/lattice-session/src/git_graph/pending.rs",
    "crates/lattice-session/src/git_graph/tests/pending_journal.rs",
    "crates/lattice-session/src/pending/contract.rs",
    "crates/lattice-session/src/pending/outcome.rs",
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

/// Every legacy-symbol hit outside [`ISSUE139B_LEGACY_ALLOWLIST`] fails, and
/// every allowlist entry must still name an existing path so the ledger
/// cannot silently outlive its files (plan §5 Commit 1).
pub fn issue139b_legacy_residue_hits(repo_root: &Path) -> anyhow::Result<Vec<LineHit>> {
    let mut hits = Vec::new();
    for needle in ISSUE139B_LEGACY_NEEDLES {
        hits.extend(scan_crates_source_and_tests(repo_root, needle)?);
    }
    hits.sort();
    hits.dedup();

    let mut failures = filter_allowlist(&hits, ISSUE139B_LEGACY_ALLOWLIST);

    for entry in ISSUE139B_LEGACY_ALLOWLIST {
        if !repo_root.join(entry).exists() {
            failures.push(format!(
                "stale allowlist entry: {entry} names no existing path"
            ));
        }
    }
    Ok(failures)
}

pub fn tool_executor_reexport_hits(repo_root: &Path) -> anyhow::Result<Vec<LineHit>> {
    let re = Regex::new(r"pub use .*ToolExecutor|lattice_tooling::ToolExecutor")?;
    let agent_root = repo_root.join("crates/lattice-agent");
    collect_rs_line_hits(
        agent_root.as_path(),
        repo_root,
        |_| false,
        |line| re.is_match(line),
    )
}

pub fn cli_production_bypass_hits(repo_root: &Path) -> anyhow::Result<Vec<LineHit>> {
    let re = Regex::new(r"DefaultToolExecutor|AgentLoop::new")?;
    let cli_src = repo_root.join("crates/lattice-cli/src");
    let hits = collect_rs_line_hits(
        cli_src.as_path(),
        repo_root,
        |path| skip_cli_test_artifact(path, &cli_src),
        |line| re.is_match(line),
    )?;
    Ok(filter_allowlist(&hits, CLI_PRODUCTION_ALLOWLIST))
}

fn skip_cli_test_artifact(path: &Path, cli_src: &Path) -> bool {
    if path
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| n.ends_with("_tests.rs"))
    {
        return true;
    }
    let rel_to_src = path.strip_prefix(cli_src).unwrap_or(path);
    rel_to_src.components().any(|c| c.as_os_str() == "tests")
}

fn scan_crates_source_and_tests(repo_root: &Path, needle: &str) -> anyhow::Result<Vec<LineHit>> {
    let mut hits = Vec::new();
    for crate_dir in std::fs::read_dir(repo_root.join("crates"))? {
        let crate_dir = crate_dir?;
        let crate_path = crate_dir.path();
        for sub in ["src", "tests"] {
            let scan_root = crate_path.join(sub);
            if !scan_root.is_dir() {
                continue;
            }
            hits.extend(collect_rs_line_hits(
                &scan_root,
                repo_root,
                |_| false,
                |line| line.contains(needle),
            )?);
        }
    }
    Ok(hits)
}

pub fn filter_allowlist(hits: &[LineHit], allowlist: &[&str]) -> Vec<LineHit> {
    hits.iter()
        .filter(|line| {
            let file = line.split(':').next().unwrap_or(line.as_str());
            !is_allowlisted(file, allowlist)
        })
        .cloned()
        .collect()
}

pub fn is_allowlisted(file: &str, allowlist: &[&str]) -> bool {
    allowlist.iter().any(|prefix| file.starts_with(prefix))
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
    fn allowlist_prefix_semantics() {
        assert!(is_allowlisted(
            "crates/lancet-native/src/lib.rs",
            DEFAULT_TOOL_EXECUTOR_ALLOWLIST
        ));
        assert!(!is_allowlisted(
            "crates/lattice-api/src/lib.rs",
            DEFAULT_TOOL_EXECUTOR_ALLOWLIST
        ));
    }

    #[test]
    fn filter_allowlist_drops_matching_prefix() {
        let input = vec![
            "crates/lancet-native/src/lib.rs:1:DefaultToolExecutor".to_string(),
            "crates/lattice-api/src/x.rs:2:DefaultToolExecutor".to_string(),
        ];
        let out = filter_allowlist(&input, DEFAULT_TOOL_EXECUTOR_ALLOWLIST);
        let text = joined(&out);
        assert!(text.contains("lattice-api"));
        assert!(!text.contains("lancet-native/src/lib.rs"));
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
        ] {
            assert!(
                ISSUE139B_LEGACY_NEEDLES.contains(&required),
                "missing plan §5 Commit 9 needle: {required}"
            );
        }
    }

    #[test]
    fn issue139b_allowlist_entries_live_under_crates() {
        for entry in ISSUE139B_LEGACY_ALLOWLIST {
            assert!(
                entry.starts_with("crates/"),
                "allowlist entries must live under crates/: {entry}"
            );
            assert!(
                entry.ends_with(".rs") || entry.ends_with('/'),
                "allowlist entries must name crate files or directories: {entry}"
            );
        }
    }

    #[test]
    fn issue139b_residue_scan_reports_unallowlisted_hits() {
        let dir = TempDir::new().unwrap();
        // Seed every allowlist entry as an empty existing path so the
        // stale-entry check stays quiet, then plant one real hit inside an
        // allowlisted file and one outside.
        for entry in ISSUE139B_LEGACY_ALLOWLIST {
            let path = dir.path().join(entry);
            if path.extension().is_some() {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                if !path.exists() {
                    fs::write(&path, "").unwrap();
                }
            } else {
                fs::create_dir_all(&path).unwrap();
            }
        }
        let allowlisted = dir.path().join("crates/lattice-agent/src/lib.rs");
        let fresh = dir.path().join("crates/lattice-api/src/turn/fresh.rs");
        fs::create_dir_all(fresh.parent().unwrap()).unwrap();
        fs::write(&allowlisted, "fn x() { let _ = AgentLoop; }\n").unwrap();
        fs::write(&fresh, "fn x() { let _ = AgentLoop; }\n").unwrap();
        let hits = issue139b_legacy_residue_hits(dir.path()).unwrap();
        let text = joined(&hits);
        assert!(text.contains("fresh.rs"), "unallowlisted hit is reported");
        assert!(!text.contains("lattice-agent/src/lib.rs"));
    }

    #[test]
    fn cli_scan_skips_tests_dir_and_inline_test_files() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("crates/lattice-cli/src");
        fs::create_dir_all(src.join("commands")).unwrap();
        fs::create_dir_all(src.join("tests")).unwrap();
        fs::write(src.join("main.rs"), "fn x() { AgentLoop::new(); }\n").unwrap();
        fs::write(src.join("foo_tests.rs"), "fn x() { AgentLoop::new(); }\n").unwrap();
        fs::write(src.join("tests/mod.rs"), "fn x() { AgentLoop::new(); }\n").unwrap();
        let hits = cli_production_bypass_hits(dir.path()).unwrap();
        let text = joined(&hits);
        assert!(text.contains("main.rs"));
        assert!(!text.contains("foo_tests.rs"));
        assert!(!text.contains("tests/mod.rs"));
    }

    #[test]
    fn cli_allowlist_run_tests_only() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("crates/lattice-cli/src/commands");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("run_tests.rs"),
            "fn x() { DefaultToolExecutor::new(); }\n",
        )
        .unwrap();
        fs::write(
            src.join("other.rs"),
            "fn x() { DefaultToolExecutor::new(); }\n",
        )
        .unwrap();
        let hits = cli_production_bypass_hits(dir.path()).unwrap();
        let text = joined(&hits);
        assert!(!text.contains("run_tests.rs"));
        assert!(text.contains("other.rs"));
    }

    #[test]
    fn lattice_agent_manifest_lancet_native_hit() {
        let dir = TempDir::new().unwrap();
        // `lattice_agent_forbidden_dependency_graph` runs `cargo metadata`, which
        // resolves path dependencies, so the referenced crate must exist. Provide
        // a minimal virtual workspace with both lattice-agent and a placeholder
        // lancet-native so metadata succeeds; the asserted hit comes from the
        // lattice-agent manifest text scan branch.
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/lattice-agent\", \"crates/lancet-native\"]\n",
        )
        .unwrap();
        let agent = dir.path().join("crates/lattice-agent");
        fs::create_dir_all(agent.join("src")).unwrap();
        fs::write(agent.join("src/lib.rs"), "").unwrap();
        fs::write(
            agent.join("Cargo.toml"),
            "[package]\nname = \"lattice-agent\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\nlancet-native = { path = \"../lancet-native\" }\n",
        )
        .unwrap();
        let native = dir.path().join("crates/lancet-native");
        fs::create_dir_all(native.join("src")).unwrap();
        fs::write(native.join("src/lib.rs"), "").unwrap();
        fs::write(
            native.join("Cargo.toml"),
            "[package]\nname = \"lancet-native\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        let out = lattice_agent_forbidden_dependency_graph(dir.path()).unwrap();
        assert!(joined(&out).contains("lancet-native"));
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
    fn default_tool_definitions_allowlist_only_fixture() {
        let dir = TempDir::new().unwrap();
        let api = dir.path().join("crates/lattice-api/src");
        fs::create_dir_all(&api).unwrap();
        fs::write(
            api.join("lib.rs"),
            "fn x() { default_tool_definitions(); }\n",
        )
        .unwrap();
        let hits = scan_with_allowlist(
            dir.path(),
            "default_tool_definitions",
            DEFAULT_TOOL_DEFINITIONS_ALLOWLIST,
        )
        .unwrap();
        assert!(joined(&hits).contains("lattice-api"));
    }

    #[test]
    fn composite_executor_allowlist_registry_only() {
        let dir = TempDir::new().unwrap();
        let native = dir.path().join("crates/lancet-native/src");
        fs::create_dir_all(&native).unwrap();
        fs::write(native.join("registry.rs"), "struct CompositeExecutor;\n").unwrap();
        let api_src = dir.path().join("crates/lattice-api/src");
        fs::create_dir_all(&api_src).unwrap();
        fs::write(api_src.join("lib.rs"), "fn x() { CompositeExecutor; }\n").unwrap();
        let hits = scan_with_allowlist(
            dir.path(),
            "CompositeExecutor",
            COMPOSITE_EXECUTOR_ALLOWLIST,
        )
        .unwrap();
        let text = joined(&hits);
        assert!(text.contains("lattice-api"));
        assert!(!text.contains("registry.rs"));
    }

    #[test]
    fn tool_executor_reexport_detected_in_agent() {
        let dir = TempDir::new().unwrap();
        let agent = dir.path().join("crates/lattice-agent/src");
        fs::create_dir_all(&agent).unwrap();
        fs::write(agent.join("lib.rs"), "pub use foo::ToolExecutor;\n").unwrap();
        let hits = tool_executor_reexport_hits(dir.path()).unwrap();
        assert!(joined(&hits).contains("ToolExecutor"));
    }

    #[test]
    fn tool_executor_reexport_clean_agent_empty() {
        let dir = TempDir::new().unwrap();
        let agent = dir.path().join("crates/lattice-agent/src");
        fs::create_dir_all(&agent).unwrap();
        fs::write(agent.join("lib.rs"), "fn foo() {}\n").unwrap();
        let hits = tool_executor_reexport_hits(dir.path()).unwrap();
        assert!(hits.is_empty(), "clean agent flagged re-export: {hits:?}");
    }

    #[test]
    fn default_tool_executor_scan_flags_non_allowlisted() {
        let dir = TempDir::new().unwrap();
        let api = dir.path().join("crates/lattice-api/src");
        fs::create_dir_all(&api).unwrap();
        fs::write(api.join("lib.rs"), "fn x() { DefaultToolExecutor; }\n").unwrap();
        let hits = scan_with_allowlist(
            dir.path(),
            "DefaultToolExecutor",
            DEFAULT_TOOL_EXECUTOR_ALLOWLIST,
        )
        .unwrap();
        assert!(joined(&hits).contains("lattice-api"));
    }

    #[test]
    fn default_tool_definitions_allowlisted_fixture_dropped() {
        let dir = TempDir::new().unwrap();
        let tests = dir.path().join("crates/lattice-api/tests");
        fs::create_dir_all(&tests).unwrap();
        fs::write(
            tests.join("usage_turn.rs"),
            "fn x() { default_tool_definitions(); }\n",
        )
        .unwrap();
        let hits = scan_with_allowlist(
            dir.path(),
            "default_tool_definitions",
            DEFAULT_TOOL_DEFINITIONS_ALLOWLIST,
        )
        .unwrap();
        assert!(hits.is_empty(), "allowlisted fixture not dropped: {hits:?}");
    }

    #[test]
    fn lattice_agent_clean_manifest_no_hit() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/lattice-agent\"]\n",
        )
        .unwrap();
        let agent = dir.path().join("crates/lattice-agent");
        fs::create_dir_all(agent.join("src")).unwrap();
        fs::write(agent.join("src/lib.rs"), "").unwrap();
        fs::write(
            agent.join("Cargo.toml"),
            "[package]\nname = \"lattice-agent\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        let out = lattice_agent_forbidden_dependency_graph(dir.path()).unwrap();
        assert!(out.is_empty(), "clean lattice-agent flagged: {out:?}");
    }

    #[test]
    fn target_directory_excluded_from_scan() {
        let dir = TempDir::new().unwrap();
        let bad = dir.path().join("crates/foo/src/target/evil.rs");
        fs::create_dir_all(bad.parent().unwrap()).unwrap();
        fs::write(&bad, "DefaultToolExecutor\n").unwrap();
        let hits = scan_with_allowlist(dir.path(), "DefaultToolExecutor", &[]).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn planning_dir_excluded_from_scan() {
        let dir = TempDir::new().unwrap();
        let bad = dir.path().join("crates/foo/src/.planning/evil.rs");
        fs::create_dir_all(bad.parent().unwrap()).unwrap();
        fs::write(&bad, "DefaultToolExecutor\n").unwrap();
        let hits = scan_with_allowlist(dir.path(), "DefaultToolExecutor", &[]).unwrap();
        assert!(hits.is_empty());
    }
}
