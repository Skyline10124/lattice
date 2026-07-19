// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use std::process::Output;

use crate::world::ScenarioWorld;

/// Redacted failure bundle written when a scenario fails (minimal tracer bullet).
#[derive(Debug, Clone)]
pub struct FailureArtifacts {
    pub dir: PathBuf,
    pub stdout: String,
    pub stderr: String,
    pub command_summary: String,
}

const REDACTED: &str = "[REDACTED]";

pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

pub fn capture_from_output(
    dir: PathBuf,
    output: &Output,
    command_summary: impl Into<String>,
    secrets: &[&str],
) -> FailureArtifacts {
    let stdout = redact(&String::from_utf8_lossy(&output.stdout), secrets);
    let stderr = redact(&String::from_utf8_lossy(&output.stderr), secrets);
    // Redact the command summary too: a caller may interpolate a secret-bearing
    // argument (e.g. an API key passed on the command line) into the summary.
    let command_summary = redact(&command_summary.into(), secrets);
    FailureArtifacts {
        dir,
        stdout,
        stderr,
        command_summary,
    }
}

pub fn write_failure_artifacts(artifacts: &FailureArtifacts) -> std::io::Result<()> {
    std::fs::create_dir_all(&artifacts.dir)?;
    std::fs::write(artifacts.dir.join("stdout.txt"), &artifacts.stdout)?;
    std::fs::write(artifacts.dir.join("stderr.txt"), &artifacts.stderr)?;
    std::fs::write(
        artifacts.dir.join("command.txt"),
        &artifacts.command_summary,
    )?;
    Ok(())
}

/// RFC #19 decision eight: failure artifacts are redacted **before** writing,
/// then **scanned after writing** as defense-in-depth. This function walks
/// `dir`, reads every regular file, and checks whether any `secrets` sentinel
/// survived into the on-disk bytes. Any file that still contains a secret is
/// best-effort redacted in place (the secret replaced with `[REDACTED]`) and
/// its path is returned so the caller can record the catch. A read/decode
/// error on a single file is skipped (best-effort), not fatal.
///
/// Returns the list of files that leaked and were patched. An empty list
/// means the post-write scan found no secret in any written artifact.
///
/// Only UTF-8-decodable files are scanned/rewritten: a secret sentinel is a
/// text substring, so a non-UTF8 file cannot carry one in a form the redactor
/// would have produced, and rewriting it via a lossy conversion would corrupt
/// it. Such files are skipped, not treated as leaks.
pub fn scan_artifacts_for_secrets(dir: &std::path::Path, secrets: &[&str]) -> Vec<PathBuf> {
    let mut leaked = Vec::new();
    if secrets.iter().all(|s| s.is_empty()) {
        return leaked;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return leaked,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if !secrets.iter().any(|s| !s.is_empty() && text.contains(s)) {
            continue;
        }
        // Leak found: redact and rewrite in place, then record the path so
        // the caller can note which file the pre-write redaction missed.
        let redacted = redact(text, secrets);
        let _ = std::fs::write(&path, redacted);
        leaked.push(path);
    }
    leaked
}

/// Persist the fake LLM's captured request log into a failure bundle.
///
/// RFC #19 decision eight + PRD user story 21 require that a failure artifact
/// bundle include the requests the fake LLM received, so the post-mortem can
/// see what the product actually sent. Each capture is written as one
/// pretty-printed JSON object on a separated block; secrets listed in
/// `secrets` are substring-redacted from the rendered bodies (the fake LLM
/// typically receives only the `Bearer <FAKE_API_KEY>` header, which never
/// reaches the body, but the redaction defense is applied regardless).
pub fn write_fake_llm_log(
    failure_dir: &std::path::Path,
    captures: &[crate::fake_llm::CapturedRequest],
    secrets: &[&str],
) -> std::io::Result<()> {
    if captures.is_empty() {
        return Ok(());
    }
    let mut text = String::new();
    for (i, cap) in captures.iter().enumerate() {
        text.push_str(&format!("--- request {i} ---\n"));
        text.push_str(&format!("model: {}\n", cap.model));
        text.push_str(&format!(
            "message_roles: {}\n",
            cap.message_roles.join(", ")
        ));
        text.push_str(&format!("tool_names: {}\n", cap.tool_names.join(", ")));
        let raw = serde_json::to_string_pretty(&cap.raw).unwrap_or_default();
        text.push_str("body:\n");
        text.push_str(&redact(&raw, secrets));
        text.push_str("\n\n");
    }
    std::fs::write(failure_dir.join("fake_llm_requests.txt"), text)?;
    Ok(())
}

/// A captured workspace listing: `(relative_path, size_bytes)` entries,
/// sorted by path. Used to compute a before/after diff on failure (RFC #19
/// decision eight, PRD user story 21: "工作区差异").
#[derive(Debug, Clone)]
pub struct WorkspaceSnapshot {
    pub entries: Vec<(String, u64)>,
}

impl WorkspaceSnapshot {
    /// Capture the current host-side project tree (skipping `.git`
    /// internals). Call this **before** the turn starts so a failure can
    /// diff it against the post-turn tree.
    pub fn capture(root: &std::path::Path) -> std::io::Result<Self> {
        let mut entries = Vec::new();
        walk(root, root, &mut entries)?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Self { entries })
    }

    fn render(&self) -> String {
        let mut text = String::new();
        for (rel, size) in &self.entries {
            text.push_str(&format!("{rel}\t{size}\n"));
        }
        text
    }
}

/// Write a shallow recursive listing of a directory tree (relative paths +
/// sizes) into `<failure_dir>/workspace_listing.txt`. The listing is
/// redacted against `secrets` in case a project file name or path embeds a
/// sentinel. This is the single-snapshot variant; for the before/after diff
/// the spec asks for, use [`write_workspace_diff`].
pub fn write_workspace_listing(
    failure_dir: &std::path::Path,
    root: &std::path::Path,
    secrets: &[&str],
) -> std::io::Result<()> {
    let snapshot = WorkspaceSnapshot::capture(root)?;
    let mut text = format!("workspace listing (root: {}):\n", root.display());
    for (rel, size) in &snapshot.entries {
        text.push_str(&format!("  {rel} ({size} bytes)\n"));
    }
    let text = redact(&text, secrets);
    std::fs::write(failure_dir.join("workspace_listing.txt"), text)?;
    Ok(())
}

/// Write the before/after workspace diff (RFC #19 decision eight, PRD user
/// story 21: "工作区差异"). Writes `workspace_before.txt`,
/// `workspace_after.txt`, and `workspace_diff.txt` (a unified diff of the
/// two listings) into `failure_dir`, all redacted against `secrets`.
///
/// The diff is over the host-side project tree listing (path + size), not a
/// byte-level file-content diff — the Direct-Local workspace IS the host
/// project tree, so the listing captures exactly what a turn observed and
/// mutated. A path appearing/disappearing or a size change shows up as a
/// `-`/`+` line.
pub fn write_workspace_diff(
    failure_dir: &std::path::Path,
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
    secrets: &[&str],
) -> std::io::Result<()> {
    let before_text = redact(&before.render(), secrets);
    let after_text = redact(&after.render(), secrets);
    std::fs::write(failure_dir.join("workspace_before.txt"), &before_text)?;
    std::fs::write(failure_dir.join("workspace_after.txt"), &after_text)?;
    let diff = unified_listing_diff(before, after);
    let diff = redact(&diff, secrets);
    std::fs::write(failure_dir.join("workspace_diff.txt"), diff)?;
    Ok(())
}

/// Produce a minimal unified-diff-style rendering of two workspace listings:
/// lines present in `before` but not `after` become `-`, lines in `after`
/// but not `before` become `+`, lines unchanged are omitted (the diff is
/// just the changes, not a full unified diff with context — that is what a
/// post-mortem actually wants).
fn unified_listing_diff(before: &WorkspaceSnapshot, after: &WorkspaceSnapshot) -> String {
    use std::collections::BTreeMap;
    let before_map: BTreeMap<&String, u64> = before.entries.iter().map(|(p, s)| (p, *s)).collect();
    let after_map: BTreeMap<&String, u64> = after.entries.iter().map(|(p, s)| (p, *s)).collect();
    let mut out = String::new();
    let mut keys: Vec<&String> = before_map.keys().chain(after_map.keys()).copied().collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let b = before_map.get(key);
        let a = after_map.get(key);
        match (b, a) {
            (Some(bs), Some(as_)) if bs == as_ => {}
            (Some(bs), Some(as_)) => {
                out.push_str(&format!("- {key}\t{bs}\n"));
                out.push_str(&format!("+ {key}\t{as_}\n"));
            }
            (Some(bs), None) => out.push_str(&format!("- {key}\t{bs}\n")),
            (None, Some(as_)) => out.push_str(&format!("+ {key}\t{as_}\n")),
            (None, None) => {}
        }
    }
    out
}

fn walk(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<(String, u64)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let meta = entry.metadata()?;
        if meta.is_dir() {
            // Skip `.git` internals to keep the listing readable; the harness
            // git project is small and `.git` is not product state.
            if path.file_name().and_then(|n| n.to_str()) == Some(".git") {
                continue;
            }
            out.push((format!("{rel}/"), 0));
            walk(root, &path, out)?;
        } else {
            out.push((rel, meta.len()));
        }
    }
    Ok(())
}

/// Render an `(env name, env value)` pair for the `world.txt` context file,
/// with secret leakage defenses:
///
/// - If the variable's *name* matches a secret-name pattern (contains `KEY`,
///   `SECRET`, `TOKEN`, `PASSWORD`, `CREDENTIAL`, `AUTH`, `PRIVATE`), the
///   value is replaced with `[REDACTED-ENV]` regardless of the `secrets`
///   list — a test that `set_env("ANTHROPIC_API_KEY", "sk-...")` must not
///   leak the key into a persisted artifact even if the caller forgot to add
///   it to `secrets`.
/// - Otherwise the value is substring-redacted against the caller-supplied
///   `secrets` list (e.g. a secret embedded in a `PATH` or custom var).
///
/// The name itself is never redacted (it carries no secret and is needed for
/// post-mortem reproduction).
pub fn redact_env_pair(name: &str, value: &str, secrets: &[&str]) -> String {
    const SECRET_NAME_MARKERS: &[&str] = &[
        "KEY",
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "CREDENTIAL",
        "AUTH",
        "PRIVATE",
    ];
    let upper = name.to_ascii_uppercase();
    let looks_secret = SECRET_NAME_MARKERS.iter().any(|m| upper.contains(m));
    if looks_secret {
        return format!("  {name}=[REDACTED-ENV]");
    }
    format!("  {name}={}", redact(value, secrets))
}

/// Build and write the `world.txt` context — scenario name, redacted command
/// summary, project/home/port, and the redacted env allow-list — into `dir`.
/// Shared by every failure-artifact path so the world-context block is not
/// duplicated per caller (the copies had drifted across
/// `assert_with_failure_artifacts` and the retired KVM-lane full-turn bundle).
///
/// `command_summary` must already be redacted (the caller passes
/// `artifacts.command_summary`, which `capture_from_output` redacts). Env
/// values are redacted via [`redact_env_pair`]: secret-name vars are masked
/// entirely, others are substring-redacted against `secrets`.
pub fn write_world_context(
    dir: &std::path::Path,
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    secrets: &[&str],
) -> std::io::Result<()> {
    let mut ctx = format!("scenario: {scenario_name}\ncommand: {command_summary}\n");
    ctx.push_str(&format!("project_dir: {}\n", world.project_dir().display()));
    ctx.push_str(&format!("home: {}\n", world.home().display()));
    ctx.push_str(&format!("port: {}\n", world.port()));
    ctx.push_str("env:\n");
    for (k, v) in world.env_summary() {
        ctx.push_str(&redact_env_pair(&k, &v, secrets));
        ctx.push('\n');
    }
    std::fs::write(dir.join("world.txt"), ctx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_llm::CapturedRequest;
    use crate::world::ScenarioWorld;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn write_fake_llm_log_renders_one_block_per_capture() {
        let dir = TempDir::new().unwrap();
        let captures = vec![
            CapturedRequest {
                model: "compat:test".into(),
                message_roles: vec!["system".into(), "user".into()],
                tool_names: vec!["observe__workspace".into()],
                raw: json!({"model": "compat:test", "messages": []}),
            },
            CapturedRequest {
                model: "compat:test".into(),
                message_roles: vec!["user".into()],
                tool_names: vec![],
                raw: json!({"model": "compat:test"}),
            },
        ];
        write_fake_llm_log(dir.path(), &captures, &[]).unwrap();
        let text = std::fs::read_to_string(dir.path().join("fake_llm_requests.txt")).unwrap();
        assert!(text.contains("--- request 0 ---"));
        assert!(text.contains("--- request 1 ---"));
        assert!(text.contains("model: compat:test"));
        assert!(text.contains("observe__workspace"));
        assert!(text.contains("body:"));
    }

    #[test]
    fn write_fake_llm_log_empty_is_noop() {
        let dir = TempDir::new().unwrap();
        write_fake_llm_log(dir.path(), &[], &[]).unwrap();
        assert!(!dir.path().join("fake_llm_requests.txt").exists());
    }

    #[test]
    fn write_fake_llm_log_redacts_secrets() {
        let dir = TempDir::new().unwrap();
        let captures = vec![CapturedRequest {
            model: "compat:test".into(),
            message_roles: vec![],
            tool_names: vec![],
            raw: json!({"secret": "sk-super-secret-value"}),
        }];
        write_fake_llm_log(dir.path(), &captures, &["sk-super-secret-value"]).unwrap();
        let text = std::fs::read_to_string(dir.path().join("fake_llm_requests.txt")).unwrap();
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("sk-super-secret-value"));
    }

    #[test]
    fn write_workspace_listing_lists_files_and_dirs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), "world!").unwrap();
        std::fs::create_dir_all(root.join(".git").join("objects")).unwrap();
        std::fs::write(root.join(".git").join("HEAD"), "ref: x").unwrap();

        let out_dir = TempDir::new().unwrap();
        write_workspace_listing(out_dir.path(), &root, &[]).unwrap();
        let text = std::fs::read_to_string(out_dir.path().join("workspace_listing.txt")).unwrap();
        assert!(text.contains("a.txt (5 bytes)"));
        assert!(text.contains("sub/b.txt (6 bytes)"));
        assert!(text.contains("sub/"));
        // .git internals are skipped.
        assert!(!text.contains(".git/"));
        assert!(!text.contains("HEAD"));
    }

    #[test]
    fn workspace_snapshot_capture_is_sorted() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("z.txt"), "z").unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::create_dir_all(root.join("m")).unwrap();
        std::fs::write(root.join("m").join("mid.txt"), "m").unwrap();
        let snap = WorkspaceSnapshot::capture(&root).unwrap();
        let paths: Vec<&String> = snap.entries.iter().map(|(p, _)| p).collect();
        assert_eq!(
            paths,
            vec!["a.txt", "m/", "m/mid.txt", "z.txt"],
            "snapshot entries must be sorted by path"
        );
    }

    #[test]
    fn write_workspace_diff_reports_added_removed_and_changed() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("kept.txt"), "same").unwrap();
        std::fs::write(root.join("gone.txt"), "old").unwrap();
        std::fs::write(root.join("changed.txt"), "v1").unwrap();
        let before = WorkspaceSnapshot::capture(&root).unwrap();

        // Mutate the tree: remove gone.txt, change changed.txt, add new.txt.
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::write(root.join("changed.txt"), "v2-longer").unwrap();
        std::fs::write(root.join("new.txt"), "new").unwrap();
        let after = WorkspaceSnapshot::capture(&root).unwrap();

        let out_dir = TempDir::new().unwrap();
        write_workspace_diff(out_dir.path(), &before, &after, &[]).unwrap();
        let diff = std::fs::read_to_string(out_dir.path().join("workspace_diff.txt")).unwrap();
        // changed.txt size changed (2 -> 9): both - and + lines.
        assert!(diff.contains("- changed.txt\t2"));
        assert!(diff.contains("+ changed.txt\t9"));
        // gone.txt removed: only - line.
        assert!(diff.contains("- gone.txt\t3"));
        assert!(!diff.contains("+ gone.txt"));
        // new.txt added: only + line.
        assert!(diff.contains("+ new.txt\t3"));
        assert!(!diff.contains("- new.txt"));
        // kept.txt unchanged: no line at all.
        assert!(!diff.contains("kept.txt"));
        // before/after listings were also written.
        assert!(out_dir.path().join("workspace_before.txt").is_file());
        assert!(out_dir.path().join("workspace_after.txt").is_file());
    }

    #[test]
    fn write_workspace_diff_redacts_secrets() {
        let before = WorkspaceSnapshot { entries: vec![] };
        let after = WorkspaceSnapshot {
            entries: vec![("sk-super-secret-path.txt".into(), 4)],
        };
        let out_dir = TempDir::new().unwrap();
        write_workspace_diff(out_dir.path(), &before, &after, &["sk-super-secret"]).unwrap();
        let after_text =
            std::fs::read_to_string(out_dir.path().join("workspace_after.txt")).unwrap();
        assert!(after_text.contains("[REDACTED]"));
        assert!(!after_text.contains("sk-super-secret-path.txt"));
        let diff = std::fs::read_to_string(out_dir.path().join("workspace_diff.txt")).unwrap();
        assert!(diff.contains("[REDACTED]"));
        assert!(!diff.contains("sk-super-secret-path.txt"));
    }

    #[test]
    fn write_world_context_writes_scenario_command_paths_and_env() {
        let world = ScenarioWorld::new("lattice-harness-ctx").expect("world");
        let out_dir = TempDir::new().unwrap();
        write_world_context(
            out_dir.path(),
            &world,
            "scenario-x",
            "lattice --json session send id ping",
            &[],
        )
        .unwrap();
        let text = std::fs::read_to_string(out_dir.path().join("world.txt")).unwrap();
        assert!(text.contains("scenario: scenario-x"));
        assert!(text.contains("command: lattice --json session send id ping"));
        assert!(text.contains(&format!("project_dir: {}", world.project_dir().display())));
        assert!(text.contains(&format!("home: {}", world.home().display())));
        assert!(text.contains(&format!("port: {}", world.port())));
        assert!(text.contains("env:"));
    }

    #[test]
    fn write_world_context_redacts_secret_env_value() {
        let mut world = ScenarioWorld::new("lattice-harness-ctx-redact").expect("world");
        world.set_env("CUSTOM_BEARER", "sk-leak-me");
        let out_dir = TempDir::new().unwrap();
        write_world_context(out_dir.path(), &world, "s", "c", &["sk-leak-me"]).unwrap();
        let text = std::fs::read_to_string(out_dir.path().join("world.txt")).unwrap();
        assert!(text.contains("CUSTOM_BEARER="));
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("sk-leak-me"));
    }

    #[test]
    fn scan_artifacts_for_secrets_no_leak_returns_empty() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("stdout.txt"), "clean output").unwrap();
        std::fs::write(dir.path().join("world.txt"), "scenario: s\n").unwrap();
        let leaked = scan_artifacts_for_secrets(dir.path(), &["sk-secret"]);
        assert!(leaked.is_empty());
    }

    #[test]
    fn scan_artifacts_for_secrets_patches_leaked_file_in_place() {
        let dir = TempDir::new().unwrap();
        // Simulate a pre-write redaction that missed an edge: a secret
        // survives into the on-disk stdout.txt.
        std::fs::write(dir.path().join("stdout.txt"), "ok sk-leak-survived more").unwrap();
        std::fs::write(dir.path().join("clean.txt"), "no secret here").unwrap();
        let leaked = scan_artifacts_for_secrets(dir.path(), &["sk-leak-survived"]);
        assert_eq!(leaked.len(), 1);
        assert!(leaked[0].file_name().unwrap() == "stdout.txt");
        let after = std::fs::read_to_string(dir.path().join("stdout.txt")).unwrap();
        assert!(after.contains("[REDACTED]"));
        assert!(!after.contains("sk-leak-survived"));
        // clean.txt untouched.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("clean.txt")).unwrap(),
            "no secret here"
        );
    }

    #[test]
    fn scan_artifacts_for_secrets_skips_non_utf8_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("bin.dat"), b"\xff\xfe not utf8 sk-secret").unwrap();
        let leaked = scan_artifacts_for_secrets(dir.path(), &["sk-secret"]);
        assert!(
            leaked.is_empty(),
            "non-UTF8 file should be skipped, not patched"
        );
        // The non-UTF8 bytes are preserved unchanged.
        assert_eq!(
            std::fs::read(dir.path().join("bin.dat")).unwrap(),
            b"\xff\xfe not utf8 sk-secret"
        );
    }

    #[test]
    fn scan_artifacts_for_secrets_empty_secret_list_is_noop() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("stdout.txt"), "anything").unwrap();
        assert!(scan_artifacts_for_secrets(dir.path(), &[]).is_empty());
        assert!(scan_artifacts_for_secrets(dir.path(), &[""]).is_empty());
    }
}
