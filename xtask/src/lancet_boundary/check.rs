// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use cargo_metadata::MetadataCommand;
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

pub fn scan_with_allowlist(
    repo_root: &Path,
    needle: &str,
    allowlist: &[&str],
) -> anyhow::Result<Vec<LineHit>> {
    let hits = scan_crates_source_and_tests(repo_root, needle)?;
    Ok(filter_allowlist(&hits, allowlist))
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
