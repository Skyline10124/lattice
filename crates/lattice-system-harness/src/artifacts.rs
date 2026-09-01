// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::driver::CommandOutcome;
use crate::world::ScenarioWorld;

const REDACTED: &str = "[REDACTED]";

/// Redacted failure bundle written when a scenario fails.
#[derive(Debug, Clone)]
pub struct FailureArtifacts {
    pub dir: PathBuf,
    pub stdout: String,
    pub stderr: String,
    pub command_text: String,
}

/// One safe, composable account of a failed scenario and its evidence pipeline.
pub struct FailureReport {
    primary_assertion: Option<String>,
    secret_leaks: Vec<String>,
    command_diagnostics: Vec<String>,
    evidence_errors: Vec<String>,
}

impl FailureReport {
    pub(crate) fn new(primary_assertion: Option<String>, secrets: &[&str]) -> Self {
        Self {
            primary_assertion: primary_assertion.map(|value| redact(&value, secrets)),
            secret_leaks: Vec::new(),
            command_diagnostics: Vec::new(),
            evidence_errors: Vec::new(),
        }
    }

    pub(crate) fn add_secret_location(&mut self, location: impl AsRef<str>, secrets: &[&str]) {
        self.secret_leaks.push(redact(location.as_ref(), secrets));
    }

    pub(crate) fn add_command_diagnostic(&mut self, diagnostic: impl AsRef<str>, secrets: &[&str]) {
        self.command_diagnostics
            .push(redact(diagnostic.as_ref(), secrets));
    }

    pub(crate) fn add_evidence_error(
        &mut self,
        stage: &'static str,
        error: impl fmt::Display,
        secrets: &[&str],
    ) {
        self.evidence_errors
            .push(redact(&format!("{stage}: {error}"), secrets));
    }

    pub(crate) fn finish(mut self) -> Result<(), Self> {
        self.secret_leaks.sort();
        self.secret_leaks.dedup();
        if self.primary_assertion.is_some()
            || !self.secret_leaks.is_empty()
            || !self.evidence_errors.is_empty()
        {
            Err(self)
        } else {
            Ok(())
        }
    }
}

impl fmt::Display for FailureReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut wrote_section = false;

        if !self.secret_leaks.is_empty() {
            writeln!(formatter, "secret leaks:")?;
            for location in &self.secret_leaks {
                writeln!(formatter, "- {location}")?;
            }
            wrote_section = true;
        }

        if let Some(primary) = &self.primary_assertion {
            if wrote_section {
                writeln!(formatter)?;
            }
            writeln!(formatter, "primary assertion:")?;
            write!(formatter, "{primary}")?;
            if !primary.ends_with('\n') {
                writeln!(formatter)?;
            }
            wrote_section = true;
        }

        if !self.command_diagnostics.is_empty() {
            if wrote_section {
                writeln!(formatter)?;
            }
            writeln!(formatter, "command diagnostics:")?;
            for (index, diagnostic) in self.command_diagnostics.iter().enumerate() {
                writeln!(formatter, "[command {index}]")?;
                write!(formatter, "{diagnostic}")?;
                if !diagnostic.ends_with('\n') {
                    writeln!(formatter)?;
                }
            }
            wrote_section = true;
        }

        if !self.evidence_errors.is_empty() {
            if wrote_section {
                writeln!(formatter)?;
            }
            writeln!(formatter, "evidence errors:")?;
            for error in &self.evidence_errors {
                writeln!(formatter, "- {error}")?;
            }
            wrote_section = true;
        }

        if !wrote_section {
            write!(formatter, "failure report")?;
        }
        Ok(())
    }
}

impl fmt::Debug for FailureReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for FailureReport {}

pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

pub(crate) fn render_command_text(
    summary: &str,
    outcome: &CommandOutcome,
    secrets: &[&str],
) -> String {
    redact(
        &format!("{summary}\noutcome: {}\n", outcome.summary()),
        secrets,
    )
}

pub fn capture_from_output(
    dir: PathBuf,
    output: &CommandOutcome,
    command_summary: impl Into<String>,
    secrets: &[&str],
) -> FailureArtifacts {
    let command_summary = command_summary.into();
    FailureArtifacts {
        dir,
        stdout: redact(&String::from_utf8_lossy(&output.stdout), secrets),
        stderr: redact(&String::from_utf8_lossy(&output.stderr), secrets),
        command_text: render_command_text(&command_summary, output, secrets),
    }
}

pub fn write_failure_artifacts(artifacts: &FailureArtifacts) -> std::io::Result<()> {
    if artifacts.dir.exists() {
        std::fs::remove_dir_all(&artifacts.dir)?;
    }
    std::fs::create_dir_all(&artifacts.dir)?;
    std::fs::write(artifacts.dir.join("stdout.txt"), &artifacts.stdout)?;
    std::fs::write(artifacts.dir.join("stderr.txt"), &artifacts.stderr)?;
    std::fs::write(artifacts.dir.join("command.txt"), &artifacts.command_text)?;
    Ok(())
}

/// Recursively scan persisted artifacts for byte-level secret sentinels.
#[derive(Debug, Error)]
#[error("secret scan failed at {}: {source}", path.display())]
pub struct ArtifactScanError {
    pub path: PathBuf,
    #[source]
    pub source: std::io::Error,
}

/// Recursively scan regular files for byte-level sentinels. Any I/O failure is
/// returned: a bundle that could not be completely inspected is not clean.
pub fn scan_artifacts_for_secrets(
    dir: &Path,
    secrets: &[&str],
) -> Result<Vec<PathBuf>, ArtifactScanError> {
    let secrets: Vec<&[u8]> = secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .map(|secret| secret.as_bytes())
        .collect();
    if secrets.is_empty() {
        return Ok(Vec::new());
    }
    let mut leaked = Vec::new();
    scan_dir(dir, &secrets, &mut leaked)?;
    Ok(leaked)
}

fn scan_dir(
    dir: &Path,
    secrets: &[&[u8]],
    leaked: &mut Vec<PathBuf>,
) -> Result<(), ArtifactScanError> {
    let entries = std::fs::read_dir(dir).map_err(|source| ArtifactScanError {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ArtifactScanError {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| ArtifactScanError {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            scan_dir(&path, secrets, leaked)?;
        } else if file_type.is_file() {
            let bytes = std::fs::read(&path).map_err(|source| ArtifactScanError {
                path: path.clone(),
                source,
            })?;
            if secrets
                .iter()
                .any(|secret| bytes.windows(secret.len()).any(|window| window == *secret))
            {
                let redacted = redact_bytes(bytes, secrets);
                std::fs::write(&path, redacted).map_err(|source| ArtifactScanError {
                    path: path.clone(),
                    source,
                })?;
                leaked.push(path);
            }
        } else {
            return Err(ArtifactScanError {
                path,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "artifact is neither a regular file nor a directory",
                ),
            });
        }
    }
    Ok(())
}

fn redact_bytes(mut bytes: Vec<u8>, secrets: &[&[u8]]) -> Vec<u8> {
    for secret in secrets {
        let mut redacted = Vec::with_capacity(bytes.len());
        let mut rest = bytes.as_slice();
        while let Some(index) = rest
            .windows(secret.len())
            .position(|window| window == *secret)
        {
            redacted.extend_from_slice(&rest[..index]);
            redacted.extend_from_slice(REDACTED.as_bytes());
            rest = &rest[index + secret.len()..];
        }
        redacted.extend_from_slice(rest);
        bytes = redacted;
    }
    bytes
}

#[derive(Debug, Error)]
pub enum EvidenceError {
    #[error("evidence I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Scan(#[from] ArtifactScanError),
    #[error("post-write sentinel found in: {0:?}")]
    PostWriteSentinel(Vec<PathBuf>),
}

#[derive(Debug, Clone)]
struct RecordedCommand {
    summary: String,
    outcome: CommandOutcome,
}

/// Failure-only scenario recorder. Construction clears stale artifacts so a
/// passing rerun cannot leave an older failure looking current.
pub struct ScenarioEvidence {
    failure_dir: PathBuf,
    project_dir: PathBuf,
    before: WorkspaceSnapshot,
    commands: Vec<RecordedCommand>,
    captures: Vec<crate::fake_llm::CapturedRequest>,
    sentinels: Vec<String>,
}

impl ScenarioEvidence {
    pub fn new(
        world: &ScenarioWorld,
        failure_dir: impl Into<PathBuf>,
        sentinels: impl IntoIterator<Item = String>,
    ) -> Result<Self, EvidenceError> {
        let failure_dir = failure_dir.into();
        if failure_dir.exists() {
            std::fs::remove_dir_all(&failure_dir)?;
        }
        Ok(Self {
            failure_dir,
            project_dir: world.project_dir().to_path_buf(),
            before: WorkspaceSnapshot::capture(world.project_dir())?,
            commands: Vec::new(),
            captures: Vec::new(),
            sentinels: sentinels.into_iter().collect(),
        })
    }

    pub fn record_command(&mut self, summary: impl Into<String>, outcome: &CommandOutcome) {
        self.commands.push(RecordedCommand {
            summary: summary.into(),
            outcome: outcome.clone(),
        });
    }

    pub fn record_captures(&mut self, captures: Vec<crate::fake_llm::CapturedRequest>) {
        self.captures = captures;
    }

    pub fn finish(self, failed: bool) -> Result<(), EvidenceError> {
        if !failed {
            return Ok(());
        }
        std::fs::create_dir_all(&self.failure_dir)?;
        let secrets: Vec<&str> = self.sentinels.iter().map(String::as_str).collect();
        for (index, command) in self.commands.iter().enumerate() {
            std::fs::write(
                self.failure_dir.join(format!("{index}-command.txt")),
                render_command_text(&command.summary, &command.outcome, &secrets),
            )?;
            std::fs::write(
                self.failure_dir.join(format!("{index}-stdout.txt")),
                redact(&String::from_utf8_lossy(&command.outcome.stdout), &secrets),
            )?;
            std::fs::write(
                self.failure_dir.join(format!("{index}-stderr.txt")),
                redact(&String::from_utf8_lossy(&command.outcome.stderr), &secrets),
            )?;
        }
        write_fake_llm_log(&self.failure_dir, &self.captures, &secrets)?;
        let after = WorkspaceSnapshot::capture(&self.project_dir)?;
        write_workspace_diff(&self.failure_dir, &self.before, &after, &secrets)?;
        verify_persisted_bundle(&self.failure_dir, &secrets)
    }
}

fn verify_persisted_bundle(dir: &Path, secrets: &[&str]) -> Result<(), EvidenceError> {
    let leaked = scan_artifacts_for_secrets(dir, secrets)?;
    if leaked.is_empty() {
        Ok(())
    } else {
        Err(EvidenceError::PostWriteSentinel(leaked))
    }
}

/// Persist the fake LLM's captured request log into a failure bundle.
pub fn write_fake_llm_log(
    failure_dir: &Path,
    captures: &[crate::fake_llm::CapturedRequest],
    secrets: &[&str],
) -> std::io::Result<()> {
    if captures.is_empty() {
        return Ok(());
    }
    let mut text = String::new();
    for (i, cap) in captures.iter().enumerate() {
        text.push_str(&format!("--- request {i} ---\n"));
        text.push_str(&format!("model: {}\n", redact(&cap.model, secrets)));
        text.push_str(&format!(
            "message_roles: {}\n",
            redact(&cap.message_roles.join(", "), secrets)
        ));
        text.push_str(&format!(
            "tool_names: {}\n",
            redact(&cap.tool_names.join(", "), secrets)
        ));
        let raw = serde_json::to_string_pretty(&cap.raw).unwrap_or_default();
        text.push_str("body:\n");
        text.push_str(&redact(&raw, secrets));
        text.push_str("\n\n");
    }
    std::fs::write(failure_dir.join("fake_llm_requests.txt"), text)
}

/// A captured workspace listing: `(relative_path, size_bytes)` entries, sorted by path.
#[derive(Debug, Clone)]
pub struct WorkspaceSnapshot {
    pub entries: Vec<(String, u64)>,
}

impl WorkspaceSnapshot {
    pub fn capture(root: &Path) -> std::io::Result<Self> {
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

pub fn write_workspace_listing(
    failure_dir: &Path,
    root: &Path,
    secrets: &[&str],
) -> std::io::Result<()> {
    let snapshot = WorkspaceSnapshot::capture(root)?;
    let mut text = format!("workspace listing (root: {}):\n", root.display());
    for (rel, size) in &snapshot.entries {
        text.push_str(&format!("  {rel} ({size} bytes)\n"));
    }
    std::fs::write(
        failure_dir.join("workspace_listing.txt"),
        redact(&text, secrets),
    )
}

pub fn write_workspace_diff(
    failure_dir: &Path,
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
    secrets: &[&str],
) -> std::io::Result<()> {
    let before_text = redact(&before.render(), secrets);
    let after_text = redact(&after.render(), secrets);
    std::fs::write(failure_dir.join("workspace_before.txt"), &before_text)?;
    std::fs::write(failure_dir.join("workspace_after.txt"), &after_text)?;
    let diff = redact(&unified_listing_diff(before, after), secrets);
    std::fs::write(failure_dir.join("workspace_diff.txt"), diff)?;
    Ok(())
}

fn unified_listing_diff(before: &WorkspaceSnapshot, after: &WorkspaceSnapshot) -> String {
    use std::collections::BTreeMap;

    let before_map: BTreeMap<&String, u64> = before.entries.iter().map(|(p, s)| (p, *s)).collect();
    let after_map: BTreeMap<&String, u64> = after.entries.iter().map(|(p, s)| (p, *s)).collect();
    let mut out = String::new();
    let mut keys: Vec<&String> = before_map.keys().chain(after_map.keys()).copied().collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        match (before_map.get(key), after_map.get(key)) {
            (Some(before_size), Some(after_size)) if before_size == after_size => {}
            (Some(before_size), Some(after_size)) => {
                out.push_str(&format!("- {key}\t{before_size}\n"));
                out.push_str(&format!("+ {key}\t{after_size}\n"));
            }
            (Some(before_size), None) => {
                out.push_str(&format!("- {key}\t{before_size}\n"));
            }
            (None, Some(after_size)) => {
                out.push_str(&format!("+ {key}\t{after_size}\n"));
            }
            (None, None) => {}
        }
    }
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .ok()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some(".git") {
                continue;
            }
            out.push((format!("{rel}/"), 0));
            walk(root, &path, out)?;
        } else {
            out.push((rel, metadata.len()));
        }
    }
    Ok(())
}

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
    if SECRET_NAME_MARKERS
        .iter()
        .any(|marker| upper.contains(marker))
    {
        return format!("  {name}=[REDACTED-ENV]");
    }
    format!("  {name}={}", redact(value, secrets))
}

pub fn write_world_context(
    dir: &Path,
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    secrets: &[&str],
) -> std::io::Result<()> {
    let mut context = format!("scenario: {scenario_name}\ncommand: {command_summary}\n");
    context.push_str(&format!("project_dir: {}\n", world.project_dir().display()));
    context.push_str(&format!("home: {}\n", world.home().display()));
    context.push_str(&format!("port: {}\n", world.port()));
    context.push_str("env:\n");
    for (name, value) in world.env_summary() {
        context.push_str(&redact_env_pair(&name, &value, secrets));
        context.push('\n');
    }
    std::fs::write(dir.join("world.txt"), context)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{
        CleanupError, CleanupOperation, HarnessIntervention, ProcessGroupCleanup,
        ProcessInspectionOperation, ProcessTermination,
    };
    use crate::fake_llm::CapturedRequest;
    use crate::world::ScenarioWorld;
    use serde_json::json;
    use std::time::Duration;
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
        let world = ScenarioWorld::new("lattice-harness-ctx-redact").expect("world");
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
        let leaked = scan_artifacts_for_secrets(dir.path(), &["sk-secret"]).unwrap();
        assert!(leaked.is_empty());
    }

    #[test]
    fn scan_artifacts_for_secrets_patches_leaked_file_in_place() {
        let dir = TempDir::new().unwrap();
        // Simulate a pre-write redaction that missed an edge: a secret
        // survives into the on-disk stdout.txt.
        std::fs::write(dir.path().join("stdout.txt"), "ok sk-leak-survived more").unwrap();
        std::fs::write(dir.path().join("clean.txt"), "no secret here").unwrap();
        let leaked = scan_artifacts_for_secrets(dir.path(), &["sk-leak-survived"]).unwrap();
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
    fn scan_artifacts_for_secrets_is_recursive_and_byte_level() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("nested/deeper");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("bin.dat");
        std::fs::write(&file, b"\xff\xfe not utf8 sk-secret").unwrap();
        let leaked = scan_artifacts_for_secrets(dir.path(), &["sk-secret"]).unwrap();
        assert_eq!(leaked, vec![file.clone()]);
        assert_eq!(
            std::fs::read(file).unwrap(),
            b"\xff\xfe not utf8 [REDACTED]"
        );
    }

    #[test]
    fn scan_artifacts_for_secrets_fails_closed_on_io_error() {
        let missing = TempDir::new().unwrap().path().join("gone");
        let error = scan_artifacts_for_secrets(&missing, &["secret"]).unwrap_err();
        assert_eq!(error.path, missing);
    }

    #[test]
    fn scan_artifacts_for_secrets_empty_secret_list_is_noop() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("stdout.txt"), "anything").unwrap();
        assert!(
            scan_artifacts_for_secrets(dir.path(), &[])
                .unwrap()
                .is_empty()
        );
        assert!(
            scan_artifacts_for_secrets(dir.path(), &[""])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn scenario_evidence_clears_stale_bundle_and_writes_only_on_failure() {
        let world = ScenarioWorld::new("evidence-stale").unwrap();
        let root = TempDir::new().unwrap();
        let failure_dir = root.path().join("scenario");
        std::fs::create_dir_all(&failure_dir).unwrap();
        std::fs::write(failure_dir.join("stale.txt"), "old failure").unwrap();
        let evidence = ScenarioEvidence::new(&world, &failure_dir, Vec::<String>::new()).unwrap();
        assert!(
            !failure_dir.exists(),
            "constructor must clear stale evidence"
        );
        evidence.finish(false).unwrap();
        assert!(!failure_dir.exists(), "passing scenarios write no bundle");

        let evidence = ScenarioEvidence::new(&world, &failure_dir, Vec::<String>::new()).unwrap();
        evidence.finish(true).unwrap();
        assert!(failure_dir.join("workspace_diff.txt").is_file());
    }

    #[test]
    fn scenario_evidence_records_complete_command_outcome() {
        let world = ScenarioWorld::new("evidence-command-outcome").unwrap();
        let root = TempDir::new().unwrap();
        let failure_dir = root.path().join("scenario");
        let mut evidence =
            ScenarioEvidence::new(&world, &failure_dir, Vec::<String>::new()).unwrap();
        let cleanup_errors = [
            CleanupOperation::SignalProcessGroup(libc::SIGKILL),
            CleanupOperation::KillChild,
            CleanupOperation::WaitChild,
            CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadProc),
        ]
        .into_iter()
        .map(|operation| CleanupError {
            operation,
            kind: std::io::ErrorKind::Other,
            raw_os_error: None,
            message: "synthetic cleanup failure".into(),
        })
        .collect();
        let outcome = CommandOutcome {
            stdout: b"partial stdout".to_vec(),
            stderr: b"partial stderr".to_vec(),
            stdout_complete: true,
            stderr_complete: true,
            termination: Some(ProcessTermination::Signaled(libc::SIGTERM)),
            interventions: vec![
                HarnessIntervention::InterruptRequested,
                HarnessIntervention::TimedOut(Duration::from_secs(1)),
                HarnessIntervention::ForcedKill,
            ],
            cleanup: ProcessGroupCleanup {
                forced: true,
                errors: cleanup_errors,
                surviving_pids: vec![11, 22],
            },
        };

        evidence.record_command("lattice test", &outcome);
        evidence.finish(true).unwrap();

        let command = std::fs::read_to_string(failure_dir.join("0-command.txt")).unwrap();
        for fact in [
            "Signaled(15)",
            "InterruptRequested",
            "TimedOut(1s)",
            "ForcedKill",
            "SignalProcessGroup(9)",
            "KillChild",
            "WaitChild",
            "InspectProcessGroup(ReadProc)",
            "surviving_pids: [11, 22]",
        ] {
            assert!(command.contains(fact), "missing {fact:?} in {command}");
        }
        assert_eq!(
            std::fs::read(failure_dir.join("0-stdout.txt")).unwrap(),
            b"partial stdout"
        );
        assert_eq!(
            std::fs::read(failure_dir.join("0-stderr.txt")).unwrap(),
            b"partial stderr"
        );
    }

    #[test]
    fn scenario_evidence_excludes_output_payload_encodings() {
        let secret = "sk-issue185-secret";
        let world = ScenarioWorld::new("evidence-command-secret").unwrap();
        let root = TempDir::new().unwrap();
        let failure_dir = root.path().join("scenario");
        let mut evidence =
            ScenarioEvidence::new(&world, &failure_dir, [secret.to_string()]).unwrap();
        let outcome = CommandOutcome {
            stdout: secret.as_bytes().to_vec(),
            stderr: secret.as_bytes().to_vec(),
            stdout_complete: true,
            stderr_complete: true,
            termination: Some(ProcessTermination::Exited(1)),
            interventions: Vec::new(),
            cleanup: ProcessGroupCleanup::default(),
        };

        evidence.record_command("lattice test", &outcome);
        evidence.finish(true).unwrap();

        for entry in std::fs::read_dir(&failure_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
                for form in [
                    secret,
                    "[115, 107, 45, 105, 115, 115, 117, 101, 49, 56, 53, 45, 115, 101, 99, 114, 101, 116]",
                    "736b2d69737375653138352d736563726574",
                    "c2staXNzdWUxODUtc2VjcmV0",
                ] {
                    assert!(
                        !text.contains(form),
                        "artifact {} leaked {form:?}: {text}",
                        path.display()
                    );
                }
            }
        }
        let command = std::fs::read_to_string(failure_dir.join("0-command.txt")).unwrap();
        assert!(command.contains("stdout_bytes: 18"));
        assert!(command.contains("stderr_bytes: 18"));
    }

    #[test]
    fn persisted_bundle_verification_fails_after_redacting_a_sentinel_hit() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("leak.bin"), b"\xffsecret-value").unwrap();
        let error = verify_persisted_bundle(dir.path(), &["secret-value"]).unwrap_err();
        assert!(matches!(error, EvidenceError::PostWriteSentinel(_)));
        assert_eq!(
            std::fs::read(dir.path().join("leak.bin")).unwrap(),
            b"\xff[REDACTED]"
        );
    }

    fn report_test_outcome(secret: &str) -> CommandOutcome {
        CommandOutcome {
            stdout: format!("stdout {secret}").into_bytes(),
            stderr: format!("stderr {secret}").into_bytes(),
            stdout_complete: false,
            stderr_complete: true,
            termination: Some(ProcessTermination::Signaled(libc::SIGTERM)),
            interventions: vec![HarnessIntervention::ForcedKill],
            cleanup: ProcessGroupCleanup {
                forced: true,
                errors: vec![CleanupError {
                    operation: CleanupOperation::WaitChild,
                    kind: std::io::ErrorKind::Other,
                    raw_os_error: None,
                    message: format!("cleanup {secret}"),
                }],
                surviving_pids: vec![41],
            },
        }
    }

    #[test]
    fn failure_report_preserves_primary_leaks_commands_and_multiple_evidence_errors() {
        let mut report = FailureReport::new(Some("primary assertion".into()), &[]);
        report.add_secret_location("z/leak.txt", &[]);
        report.add_secret_location("a/leak.txt", &[]);
        report.add_secret_location("a/leak.txt", &[]);
        report.add_command_diagnostic("command facts", &[]);
        report.add_evidence_error("first stage", "first failure", &[]);
        report.add_evidence_error("second stage", "second failure", &[]);

        let report = report.finish().unwrap_err();
        let text = report.to_string();
        assert!(text.contains("primary assertion"));
        assert!(text.contains("a/leak.txt"));
        assert!(text.contains("z/leak.txt"));
        assert_eq!(text.matches("a/leak.txt").count(), 1);
        assert!(text.contains("command facts"));
        assert!(text.contains("first stage: first failure"));
        assert!(text.contains("second stage: second failure"));
        assert_eq!(format!("{report:?}"), text);
    }

    #[test]
    fn failure_report_display_is_redacted_and_stably_ordered() {
        let secret = "failure-report-secret";
        let mut report = FailureReport::new(Some(format!("primary {secret}")), &[secret]);
        report.add_secret_location(format!("z-{secret}"), &[secret]);
        report.add_secret_location(format!("a-{secret}"), &[secret]);
        report.add_command_diagnostic(format!("command {secret}"), &[secret]);
        report.add_evidence_error("persist", format!("error {secret}"), &[secret]);
        let report = report.finish().unwrap_err();
        let text = report.to_string();

        assert!(!text.contains(secret));
        assert!(text.contains(REDACTED));
        let leaks = text.find("secret leaks:").unwrap();
        let primary = text.find("primary assertion:").unwrap();
        let commands = text.find("command diagnostics:").unwrap();
        let evidence = text.find("evidence errors:").unwrap();
        assert!(leaks < primary && primary < commands && commands < evidence);
        assert!(text.find("a-[REDACTED]").unwrap() < text.find("z-[REDACTED]").unwrap());
    }

    #[test]
    fn capture_from_output_redacts_stdout_stderr_and_shared_command_text() {
        let secret = "capture-secret";
        let outcome = report_test_outcome(secret);
        let artifacts = capture_from_output(
            PathBuf::from("bundle"),
            &outcome,
            format!("lattice --token {secret}"),
            &[secret],
        );

        for text in [
            artifacts.stdout.as_str(),
            artifacts.stderr.as_str(),
            artifacts.command_text.as_str(),
        ] {
            assert!(!text.contains(secret), "artifact leaked sentinel: {text}");
        }
        assert!(
            artifacts
                .command_text
                .contains("lattice --token [REDACTED]")
        );
        assert!(artifacts.command_text.contains("outcome: CommandOutcome"));
        assert!(artifacts.command_text.contains("Signaled(15)"));
        assert!(artifacts.command_text.contains("stdout_complete: false"));
        assert!(artifacts.command_text.contains("surviving_pids: [41]"));
        assert!(artifacts.command_text.contains("cleanup [REDACTED]"));
    }
}
