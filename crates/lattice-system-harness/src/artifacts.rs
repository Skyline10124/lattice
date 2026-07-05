// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use std::process::Output;

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
/// byte-level file-content diff — the workspace Upper lives inside a
/// microsandbox, not on the host filesystem, so the host-side listing is
/// the observable artifact. A path appearing/disappearing or a size change
/// shows up as a `-`/`+` line.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_llm::CapturedRequest;
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
}
