// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! PR check selection for `.github/workflows/ci.yml`.
//!
//! Maps the files a PR changes to the workspace members whose clippy and test
//! results those changes can affect, then prints `key=value` lines for
//! `$GITHUB_OUTPUT`:
//!
//! - `rust`: `false` when no change can affect a Rust check (docs only).
//! - `harness`: whether the external-binary harness and its binaries run.
//! - `packages`: cargo package selection, `--workspace` or `-p a -p b ...`.
//!
//! Selection is conservative: a path that does not belong to exactly one
//! member (root manifests, lockfile, toolchain, `.cargo/`, `.github/`, ...)
//! selects the whole workspace. main push never uses this plan, so the full
//! gate still runs on every merge.

use std::collections::BTreeSet;
use std::process::{Command, Stdio};

use anyhow::Context as _;
use cargo_metadata::MetadataCommand;
use clap::Args;

use crate::repo_root;

#[derive(Args, Debug)]
pub struct CiPlanArgs {
    /// Git revision the PR is compared against (in CI: the merge commit's
    /// base parent, `HEAD^1`).
    #[arg(long)]
    base: String,
}

/// Whether no Rust check reads `path`. Agent instruction files are never
/// compiled or read by tests, wherever they sit.
fn no_rust_effect(path: &str) -> bool {
    const DIRS: &[&str] = &["docs/", ".agents/", ".pi/", ".githooks/", "benchmarks/"];
    const ROOT_FILES: &[&str] = &["LICENSE", ".gitignore"];
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    DIRS.iter().any(|d| path.starts_with(d))
        || matches!(file, "AGENTS.md" | "CLAUDE.md")
        || (dir.is_empty() && (file.ends_with(".md") || ROOT_FILES.contains(&file)))
}

/// The harness has no Cargo edge to product crates (harness-boundary forbids
/// one) but runs the `lattice` binary, so it depends on lattice-cli.
const HARNESS: &str = "lattice-system-harness";
const CLI: &str = "lattice-cli";
/// Embeds the committed `tools/*/generated` artifacts via `include_bytes!`,
/// a dependency Cargo metadata does not record.
const RUNTIME: &str = "lancet-runtime";
/// Its tests and `tool-codegen --check` read `tools/*/manifest.toml`.
const XTASK: &str = "xtask";

#[derive(Debug)]
struct Member {
    name: String,
    /// Repo-relative directory with a trailing `/`.
    dir: String,
    /// Workspace members this one depends on, any dependency kind.
    deps: Vec<String>,
}

#[derive(Debug, PartialEq)]
enum Plan {
    NoRust,
    /// The path that forced the whole workspace.
    Full(String),
    Packages(BTreeSet<String>),
}

pub fn run(args: CiPlanArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let out = Command::new("git")
        .current_dir(&root)
        // `-z` keeps non-ASCII paths unquoted; `--no-renames` lists both
        // sides of a rename so the old location is attributed too.
        .args([
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &args.base,
            "HEAD",
        ])
        .stderr(Stdio::inherit())
        .output()
        .context("failed to run `git diff`")?;
    anyhow::ensure!(out.status.success(), "`git diff {} HEAD` failed", args.base);
    let changed: Vec<String> = String::from_utf8(out.stdout)
        .context("git diff printed non-UTF-8 paths")?
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();

    let members = load_members(&root)?;
    for required in [HARNESS, CLI, RUNTIME, XTASK] {
        // A rename would otherwise drop the extra edges silently.
        anyhow::ensure!(
            members.iter().any(|m| m.name == required),
            "ci-plan expects workspace member `{required}`; update xtask/src/ci_plan.rs"
        );
    }

    let plan = plan(&changed, &members);
    eprintln!(
        "ci-plan: {} changed file(s) since {}: {plan:?}",
        changed.len(),
        args.base
    );
    print!("{}", render(&plan));
    Ok(())
}

fn load_members(root: &std::path::Path) -> anyhow::Result<Vec<Member>> {
    let metadata = MetadataCommand::new()
        .current_dir(root)
        .no_deps()
        .exec()
        .map_err(|e| anyhow::anyhow!("failed to read workspace metadata: {e}"))?;
    let names: BTreeSet<&str> = metadata.packages.iter().map(|p| p.name.as_str()).collect();
    metadata
        .packages
        .iter()
        .map(|p| {
            let dir = p
                .manifest_path
                .parent()
                .and_then(|d| d.strip_prefix(&metadata.workspace_root).ok())
                .with_context(|| format!("{} is outside the workspace root", p.manifest_path))?;
            Ok(Member {
                name: p.name.to_string(),
                dir: format!("{dir}/"),
                deps: p
                    .dependencies
                    .iter()
                    .filter(|d| d.path.is_some() && names.contains(d.name.as_str()))
                    .map(|d| d.name.clone())
                    .collect(),
            })
        })
        .collect()
}

fn plan(changed: &[String], members: &[Member]) -> Plan {
    let mut affected = BTreeSet::new();
    for path in changed {
        if no_rust_effect(path) {
            continue;
        }
        // xtask holds this planner and the CI orchestration; changing either
        // must not be judged by the changed planner alone.
        if path.starts_with("xtask/") {
            return Plan::Full(path.clone());
        }
        let Some(member) = members.iter().find(|m| path.starts_with(&m.dir)) else {
            return Plan::Full(path.clone());
        };
        affected.insert(member.name.clone());
        if path.starts_with("tools/") {
            affected.insert(RUNTIME.to_owned());
            affected.insert(XTASK.to_owned());
        }
    }
    if affected.is_empty() {
        return Plan::NoRust;
    }

    // Reverse dependency closure, plus the harness's binary edge.
    loop {
        let before = affected.len();
        for m in members {
            let depends = m.deps.iter().any(|d| affected.contains(d))
                || (m.name == HARNESS && affected.contains(CLI));
            if depends {
                affected.insert(m.name.clone());
            }
        }
        if affected.len() == before {
            return Plan::Packages(affected);
        }
    }
}

fn render(plan: &Plan) -> String {
    let (rust, harness, packages) = match plan {
        Plan::NoRust => (false, false, String::new()),
        Plan::Full(_) => (true, true, "--workspace".to_owned()),
        Plan::Packages(set) => (
            true,
            set.contains(HARNESS),
            set.iter()
                .map(|p| format!("-p {p}"))
                .collect::<Vec<_>>()
                .join(" "),
        ),
    };
    format!("rust={rust}\nharness={harness}\npackages={packages}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_for(changed: &[&str]) -> Plan {
        let member = |name: &str, dir: &str, deps: &[&str]| Member {
            name: name.to_owned(),
            dir: dir.to_owned(),
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
        };
        let members = [
            member("lancet-protocol", "crates/lancet-protocol/", &[]),
            member(RUNTIME, "crates/lancet-runtime/", &["lancet-protocol"]),
            member("lattice-tui", "crates/lattice-tui/", &[]),
            member(CLI, "crates/lattice-cli/", &["lattice-tui", RUNTIME]),
            member(HARNESS, "crates/lattice-system-harness/", &[]),
            member("noop-echo", "tools/noop/", &[]),
            member(XTASK, "xtask/", &["lancet-protocol"]),
        ];
        let changed: Vec<String> = changed.iter().map(|p| (*p).to_owned()).collect();
        plan(&changed, &members)
    }

    fn packages(names: &[&str]) -> Plan {
        Plan::Packages(names.iter().map(|n| (*n).to_owned()).collect())
    }

    #[test]
    fn selects_changed_members_and_everything_that_depends_on_them() {
        assert_eq!(
            plan_for(&[
                "docs/a.md",
                "README.md",
                ".pi/agents/x.md",
                "crates/lattice-tui/AGENTS.md"
            ]),
            Plan::NoRust
        );
        assert_eq!(plan_for(&[]), Plan::NoRust);

        assert_eq!(
            plan_for(&["crates/lattice-tui/src/lib.rs", "docs/a.md"]),
            packages(&["lattice-tui", CLI, HARNESS])
        );
        assert_eq!(
            plan_for(&["crates/lattice-system-harness/tests/x.rs"]),
            packages(&[HARNESS])
        );
        assert_eq!(
            plan_for(&["tools/noop/generated/descriptor.json"]),
            packages(&["noop-echo", RUNTIME, XTASK, CLI, HARNESS])
        );

        for full in [
            "Cargo.lock",
            ".github/workflows/ci.yml",
            "xtask/src/ci_plan.rs",
            "crates/new/x.rs",
        ] {
            assert_eq!(
                plan_for(&[full, "crates/lattice-tui/src/lib.rs"]),
                Plan::Full(full.to_owned())
            );
        }
    }

    #[test]
    fn renders_github_output_lines() {
        assert_eq!(
            render(&Plan::NoRust),
            "rust=false\nharness=false\npackages=\n"
        );
        assert_eq!(
            render(&Plan::Full("Cargo.lock".into())),
            "rust=true\nharness=true\npackages=--workspace\n"
        );
        assert_eq!(
            render(&packages(&["b", "a"])),
            "rust=true\nharness=false\npackages=-p a -p b\n"
        );
    }
}
