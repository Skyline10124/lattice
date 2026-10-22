// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::driver::CommandOutcome;
use crate::world::ScenarioWorld;

const REDACTED: &str = "[REDACTED]";
const CLAIMS_DIR: &str = ".claims";
const FAILURE_DIR_ENV: &str = "LATTICE_HARNESS_FAILURE_DIR";

pub(crate) fn checked_failure_dir(name: &str) -> Result<PathBuf, EvidenceError> {
    validate_artifact_name(OsStr::new(name))?;
    let base = std::env::var(FAILURE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("harness-failures")
        });
    Ok(base.join(name))
}

/// Redacted failure bundle written when a scenario fails.
#[derive(Debug, Clone)]
pub(crate) struct FailureArtifacts {
    pub(crate) dir: PathBuf,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) command_text: String,
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

fn redact_io_error(error: std::io::Error, secrets: &[&str]) -> std::io::Error {
    std::io::Error::new(error.kind(), redact(&error.to_string(), secrets))
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

pub(crate) fn capture_from_output(
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

pub(crate) fn write_failure_artifacts_collecting(
    artifacts: &FailureArtifacts,
    secrets: &[&str],
    report: &mut FailureReport,
) {
    attempt(
        "write stdout.txt",
        std::fs::write(artifacts.dir.join("stdout.txt"), &artifacts.stdout),
        secrets,
        &mut report.evidence_errors,
    );
    attempt(
        "write stderr.txt",
        std::fs::write(artifacts.dir.join("stderr.txt"), &artifacts.stderr),
        secrets,
        &mut report.evidence_errors,
    );
    attempt(
        "write command.txt",
        std::fs::write(artifacts.dir.join("command.txt"), &artifacts.command_text),
        secrets,
        &mut report.evidence_errors,
    );
}

fn attempt<T>(
    stage: &str,
    result: std::io::Result<T>,
    secrets: &[&str],
    errors: &mut Vec<String>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(redact(&format!("{stage}: {error}"), secrets));
            None
        }
    }
}

/// Recursively scan persisted artifacts for byte-level secret sentinels.
#[derive(Debug, Error)]
#[error("secret scan failed at {}: {source}", path.display())]
pub struct ArtifactScanError {
    pub path: PathBuf,
    #[source]
    pub source: std::io::Error,
}

#[derive(Default)]
struct ScanCollection {
    leak_paths: Vec<PathBuf>,
    errors: Vec<ArtifactScanError>,
}

/// Recursively scan regular files for byte-level sentinels. Any I/O failure is
/// returned: a bundle that could not be completely inspected is not clean.
pub fn scan_artifacts_for_secrets(
    dir: &Path,
    secrets: &[&str],
) -> Result<Vec<PathBuf>, ArtifactScanError> {
    let mut collection = scan_artifacts_collecting(dir, secrets);
    if collection.errors.is_empty() {
        Ok(collection.leak_paths)
    } else {
        Err(collection.errors.remove(0))
    }
}

fn scan_artifacts_collecting(dir: &Path, secrets: &[&str]) -> ScanCollection {
    let secret_bytes: Vec<&[u8]> = secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .map(|secret| secret.as_bytes())
        .collect();
    if secret_bytes.is_empty() {
        return ScanCollection::default();
    }

    let mut collection = ScanCollection::default();
    scan_dir_collecting(dir, dir, &secret_bytes, secrets, &mut collection);
    collection.leak_paths.sort();
    collection.leak_paths.dedup();
    collection.errors.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.source.to_string().cmp(&right.source.to_string()))
    });
    collection
}

fn scan_dir_collecting(
    root: &Path,
    dir: &Path,
    secret_bytes: &[&[u8]],
    secrets: &[&str],
    collection: &mut ScanCollection,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) => {
            collection
                .errors
                .push(scan_error(root, dir, source, secrets));
            return;
        }
    };

    let mut sorted_entries = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => sorted_entries.push(entry),
            Err(source) => collection
                .errors
                .push(scan_error(root, dir, source, secrets)),
        }
    }
    sorted_entries.sort_by_key(std::fs::DirEntry::path);

    for entry in sorted_entries {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(source) => {
                collection
                    .errors
                    .push(scan_error(root, &path, source, secrets));
                continue;
            }
        };

        if file_type.is_dir() {
            scan_dir_collecting(root, &path, secret_bytes, secrets, collection);
            continue;
        }
        if !file_type.is_file() {
            collection.errors.push(scan_error(
                root,
                &path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "artifact is neither a regular file nor a directory",
                ),
                secrets,
            ));
            continue;
        }

        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) => {
                collection
                    .errors
                    .push(scan_error(root, &path, source, secrets));
                continue;
            }
        };
        if secret_bytes
            .iter()
            .any(|secret| bytes.windows(secret.len()).any(|window| window == *secret))
        {
            collection.leak_paths.push(path.clone());
            let redacted = redact_bytes(bytes, secret_bytes);
            if let Err(source) = std::fs::write(&path, redacted) {
                collection
                    .errors
                    .push(scan_error(root, &path, source, secrets));
            }
        }
    }
}

fn scan_error(
    root: &Path,
    path: &Path,
    source: std::io::Error,
    secrets: &[&str],
) -> ArtifactScanError {
    let kind = source.kind();
    let message = redact(&source.to_string(), secrets);
    ArtifactScanError {
        path: PathBuf::from(safe_artifact_location(root, path, secrets)),
        source: std::io::Error::new(kind, message),
    }
}

fn safe_artifact_location(root: &Path, path: &Path, secrets: &[&str]) -> String {
    let location = match path.strip_prefix(root) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_string(),
        Ok(relative) => relative.display().to_string(),
        Err(_) => "<artifact>".to_string(),
    };
    redact(&location, secrets)
}

pub(crate) fn scan_bundle_into_report(dir: &Path, secrets: &[&str], report: &mut FailureReport) {
    let collection = scan_artifacts_collecting(dir, secrets);
    let locations: Vec<String> = collection
        .leak_paths
        .iter()
        .map(|path| safe_artifact_location(dir, path, secrets))
        .collect();
    for location in &locations {
        report.add_secret_location(location, secrets);
    }
    for error in collection.errors {
        report.add_evidence_error("secret scan", error, secrets);
    }
    if !locations.is_empty() {
        let text = format!(
            "post-write sentinel scan redacted {} file(s):\n{}\n",
            locations.len(),
            locations
                .iter()
                .map(|location| format!("- {location}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        attempt(
            "write sentinel_scan.txt",
            std::fs::write(dir.join("sentinel_scan.txt"), text),
            secrets,
            &mut report.evidence_errors,
        );
    }
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
    #[error("invalid artifact name")]
    InvalidArtifactName,
    #[error("artifact target is already claimed")]
    ArtifactClaimBusy,
    #[error("evidence I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    FailureReport(String),
}

pub(crate) fn validate_artifact_name(name: &OsStr) -> Result<(), EvidenceError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.contains(&b'/') || bytes.contains(&0) {
        return Err(EvidenceError::InvalidArtifactName);
    }
    let mut components = Path::new(name).components();
    let normal = match (components.next(), components.next()) {
        (Some(Component::Normal(normal)), None) => normal,
        _ => return Err(EvidenceError::InvalidArtifactName),
    };
    if normal == OsStr::new(CLAIMS_DIR) {
        return Err(EvidenceError::InvalidArtifactName);
    }
    Ok(())
}

pub(crate) struct ArtifactClaim {
    file: File,
}

impl ArtifactClaim {
    pub(crate) fn acquire(target: &Path) -> Result<Self, EvidenceError> {
        let name = target
            .file_name()
            .ok_or(EvidenceError::InvalidArtifactName)?;
        validate_artifact_name(name)?;
        let base = target.parent().ok_or(EvidenceError::InvalidArtifactName)?;
        let claims_dir = base.join(CLAIMS_DIR);
        std::fs::create_dir_all(&claims_dir)?;
        let mut lock_name = OsString::from(name);
        lock_name.push(".lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(claims_dir.join(lock_name))?;
        // SAFETY: `file` owns a valid descriptor for the duration of the call and
        // remains alive in `ArtifactClaim` until the lock should be released.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Err(EvidenceError::ArtifactClaimBusy);
            }
            return Err(EvidenceError::Io(error));
        }
        Ok(Self { file })
    }
}

impl Drop for ArtifactClaim {
    fn drop(&mut self) {
        // flock 锁属于打开文件描述（open file description），而不是某个 fd。同一进程中
        // 其他测试并行启动子进程时，子进程在 fork 之后、exec 之前持有该描述的副本
        // （O_CLOEXEC 要到 exec 才关闭），只关闭本 fd 不会释放锁，紧接着的重新获取会
        // 得到 ArtifactClaimBusy（#228）。显式 LOCK_UN 作用于整个描述，副本也随之解锁。
        // SAFETY: `self.file` 在调用期间持有有效描述符。
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub(crate) fn clear_stale_bundle(target: &Path) -> std::io::Result<()> {
    if target.exists() {
        std::fs::remove_dir_all(target)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct RecordedCommand {
    summary: String,
    outcome: CommandOutcome,
}

/// Failure-only scenario recorder. Construction claims the target and clears
/// stale artifacts so a passing rerun cannot leave an older failure current.
pub struct ScenarioEvidence {
    failure_dir: PathBuf,
    _claim: ArtifactClaim,
    daemon: std::sync::Arc<crate::daemon::ScenarioDaemon>,
    /// 与世界共享的场景预算槽：收尾时普通 stop 只用同一起点的剩余量。
    budget: std::sync::Arc<crate::world::BudgetSlot>,
    project_dir: PathBuf,
    before: WorkspaceSnapshot,
    commands: Vec<RecordedCommand>,
    captures: Vec<crate::fake_llm::CapturedRequest>,
    sentinels: Vec<String>,
    pty_output: Vec<Vec<u8>>,
    attachments: Vec<(String, serde_json::Value)>,
}

impl ScenarioEvidence {
    pub fn new(
        world: &ScenarioWorld,
        scenario_name: &str,
        sentinels: impl IntoIterator<Item = String>,
    ) -> Result<Self, EvidenceError> {
        Self::new_at_dir(world, checked_failure_dir(scenario_name)?, sentinels)
    }

    fn new_at_dir(
        world: &ScenarioWorld,
        failure_dir: PathBuf,
        sentinels: impl IntoIterator<Item = String>,
    ) -> Result<Self, EvidenceError> {
        let sentinels: Vec<String> = sentinels.into_iter().collect();
        let secrets: Vec<&str> = sentinels.iter().map(String::as_str).collect();
        let claim = ArtifactClaim::acquire(&failure_dir).map_err(|error| match error {
            EvidenceError::Io(source) => EvidenceError::Io(redact_io_error(source, &secrets)),
            other => other,
        })?;
        clear_stale_bundle(&failure_dir)
            .map_err(|error| EvidenceError::Io(redact_io_error(error, &secrets)))?;
        let before = WorkspaceSnapshot::capture(world.project_dir())
            .map_err(|error| EvidenceError::Io(redact_io_error(error, &secrets)))?;
        Ok(Self {
            failure_dir,
            _claim: claim,
            daemon: world.daemon.clone(),
            budget: world.budget_slot(),
            project_dir: world.project_dir().to_path_buf(),
            before,
            commands: Vec::new(),
            captures: Vec::new(),
            sentinels,
            pty_output: Vec::new(),
            attachments: Vec::new(),
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
    /// 保存场景的公开读取、屏障和产物身份；与其他证据共用脱敏及扫描。
    pub fn record_json(
        &mut self,
        name: &str,
        value: &serde_json::Value,
    ) -> Result<(), EvidenceError> {
        validate_artifact_name(OsStr::new(name))?;
        self.attachments.push((name.to_owned(), value.clone()));
        Ok(())
    }

    /// PTY 字节单独记录，不伪造管道命令的退出或 EOF 证据。
    pub fn record_pty(&mut self, bytes: &[u8]) {
        self.pty_output.push(bytes.to_vec());
    }

    pub fn finish_report(self, primary_assertion: Option<String>) -> Result<(), FailureReport> {
        let persist = primary_assertion.is_some();
        self.finish_internal(primary_assertion, persist)
    }

    pub fn finish(self, failed: bool) -> Result<(), EvidenceError> {
        self.finish_internal(None, failed)
            .map_err(|report| EvidenceError::FailureReport(report.to_string()))
    }

    fn finish_internal(
        self,
        primary_assertion: Option<String>,
        persist: bool,
    ) -> Result<(), FailureReport> {
        let secrets: Vec<&str> = self.sentinels.iter().map(String::as_str).collect();
        let mut report = FailureReport::new(primary_assertion, &secrets);
        let mut persist = persist;
        // 产物副本已经脱敏，必须在业务断言之外扫描原始证据。
        for (index, command) in self.commands.iter().enumerate() {
            for (stream, bytes) in [
                ("stdout", &command.outcome.stdout),
                ("stderr", &command.outcome.stderr),
            ] {
                if crate::assert::contains_secret(bytes, &secrets) {
                    report.add_secret_location(format!("command {index} {stream}"), &secrets);
                    persist = true;
                }
            }
        }
        for (index, bytes) in self.pty_output.iter().enumerate() {
            if crate::assert::contains_secret(bytes, &secrets) {
                report.add_secret_location(format!("PTY {index}"), &secrets);
                persist = true;
            }
        }
        for (name, value) in &self.attachments {
            if crate::assert::contains_secret(value.to_string().as_bytes(), &secrets) {
                report.add_secret_location(format!("JSON {name}"), &secrets);
                persist = true;
            }
        }
        // Captures omit credential headers; bodies and retained client identity
        // headers have no legitimate use for a scenario's secret sentinels.
        for (index, capture) in self.captures.iter().enumerate() {
            let bytes = serde_json::to_vec(capture).expect("serialize captured request");
            if crate::assert::contains_secret(&bytes, &secrets) {
                report.add_secret_location(format!("capture {index}"), &secrets);
                persist = true;
            }
        }
        let cleanup = self.daemon.cleanup(self.budget.normal_stop_deadline());
        let persist = persist || cleanup.is_err();
        if let Err(errors) = cleanup {
            for error in errors {
                report.add_evidence_error("daemon cleanup", error, &secrets);
            }
        }
        for command in &self.commands {
            report.add_command_diagnostic(
                render_command_text(&command.summary, &command.outcome, &secrets),
                &secrets,
            );
        }
        if !persist {
            return report.finish();
        }
        if let Err(error) = std::fs::create_dir_all(&self.failure_dir) {
            report.add_evidence_error("create artifact root", error, &secrets);
            return report.finish();
        }

        for (name, value) in &self.attachments {
            attempt(
                "write JSON evidence",
                std::fs::write(
                    self.failure_dir.join(format!("{name}.json")),
                    redact(&value.to_string(), &secrets),
                ),
                &secrets,
                &mut report.evidence_errors,
            );
        }
        for (index, bytes) in self.pty_output.iter().enumerate() {
            attempt(
                "write PTY output",
                std::fs::write(
                    self.failure_dir.join(format!("{index}-pty.txt")),
                    redact(&String::from_utf8_lossy(bytes), &secrets),
                ),
                &secrets,
                &mut report.evidence_errors,
            );
        }

        for (index, command) in self.commands.iter().enumerate() {
            attempt(
                &format!("write {index}-command.txt"),
                std::fs::write(
                    self.failure_dir.join(format!("{index}-command.txt")),
                    render_command_text(&command.summary, &command.outcome, &secrets),
                ),
                &secrets,
                &mut report.evidence_errors,
            );
            attempt(
                &format!("write {index}-stdout.txt"),
                std::fs::write(
                    self.failure_dir.join(format!("{index}-stdout.txt")),
                    redact(&String::from_utf8_lossy(&command.outcome.stdout), &secrets),
                ),
                &secrets,
                &mut report.evidence_errors,
            );
            attempt(
                &format!("write {index}-stderr.txt"),
                std::fs::write(
                    self.failure_dir.join(format!("{index}-stderr.txt")),
                    redact(&String::from_utf8_lossy(&command.outcome.stderr), &secrets),
                ),
                &secrets,
                &mut report.evidence_errors,
            );
        }

        attempt(
            "write fake LLM log",
            write_fake_llm_log(&self.failure_dir, &self.captures, &secrets),
            &secrets,
            &mut report.evidence_errors,
        );
        if let Some(after) = attempt(
            "capture workspace after",
            WorkspaceSnapshot::capture(&self.project_dir),
            &secrets,
            &mut report.evidence_errors,
        ) {
            write_workspace_diff_collecting(
                &self.failure_dir,
                &self.before,
                &after,
                &secrets,
                &mut report,
            );
        }
        scan_bundle_into_report(&self.failure_dir, &secrets, &mut report);
        report.finish()
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
    let mut report = FailureReport::new(None, secrets);
    write_workspace_diff_collecting(failure_dir, before, after, secrets, &mut report);
    match report.evidence_errors.into_iter().next() {
        Some(error) => Err(std::io::Error::other(error)),
        None => Ok(()),
    }
}

fn write_workspace_diff_collecting(
    failure_dir: &Path,
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
    secrets: &[&str],
    report: &mut FailureReport,
) {
    let before_text = redact(&before.render(), secrets);
    let after_text = redact(&after.render(), secrets);
    let diff = redact(&unified_listing_diff(before, after), secrets);
    attempt(
        "write workspace_before.txt",
        std::fs::write(failure_dir.join("workspace_before.txt"), before_text),
        secrets,
        &mut report.evidence_errors,
    );
    attempt(
        "write workspace_after.txt",
        std::fs::write(failure_dir.join("workspace_after.txt"), after_text),
        secrets,
        &mut report.evidence_errors,
    );
    attempt(
        "write workspace_diff.txt",
        std::fs::write(failure_dir.join("workspace_diff.txt"), diff),
        secrets,
        &mut report.evidence_errors,
    );
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
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some(".git") {
                continue;
            }
            out.push((format!("{rel}/"), 0));
            walk(root, &path, out)?;
        } else if file_type.is_symlink() {
            out.push((rel, std::fs::symlink_metadata(&path)?.len()));
        } else {
            out.push((rel, entry.metadata()?.len()));
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
    std::fs::write(dir.join("world.txt"), redact(&context, secrets))
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
                ..Default::default()
            },
            CapturedRequest {
                model: "compat:test".into(),
                message_roles: vec!["user".into()],
                tool_names: vec![],
                raw: json!({"model": "compat:test"}),
                ..Default::default()
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
    fn write_fake_llm_log_redacts_secrets() {
        let dir = TempDir::new().unwrap();
        let captures = vec![CapturedRequest {
            model: "compat:test".into(),
            message_roles: vec![],
            tool_names: vec![],
            raw: json!({"secret": "sk-super-secret-value"}),
            ..Default::default()
        }];
        write_fake_llm_log(dir.path(), &captures, &["sk-super-secret-value"]).unwrap();
        let text = std::fs::read_to_string(dir.path().join("fake_llm_requests.txt")).unwrap();
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("sk-super-secret-value"));
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
        assert_eq!(error.path, Path::new("."));
    }

    #[test]
    fn scenario_evidence_clears_stale_bundle_and_writes_only_on_failure() {
        let world = ScenarioWorld::new("evidence-stale").unwrap();
        let root = TempDir::new().unwrap();
        let failure_dir = root.path().join("scenario");
        std::fs::create_dir_all(&failure_dir).unwrap();
        std::fs::write(failure_dir.join("stale.txt"), "old failure").unwrap();
        let evidence =
            ScenarioEvidence::new_at_dir(&world, failure_dir.clone(), Vec::<String>::new())
                .unwrap();
        assert!(
            !failure_dir.exists(),
            "constructor must clear stale evidence"
        );
        evidence.finish(false).unwrap();
        assert!(!failure_dir.exists(), "passing scenarios write no bundle");

        let evidence =
            ScenarioEvidence::new_at_dir(&world, failure_dir.clone(), Vec::<String>::new())
                .unwrap();
        evidence.finish(true).unwrap();
        assert!(failure_dir.join("workspace_diff.txt").is_file());
    }

    #[test]
    fn scenario_evidence_records_complete_command_outcome() {
        let world = ScenarioWorld::new("evidence-command-outcome").unwrap();
        let root = TempDir::new().unwrap();
        let failure_dir = root.path().join("scenario");
        let mut evidence =
            ScenarioEvidence::new_at_dir(&world, failure_dir.clone(), Vec::<String>::new())
                .unwrap();
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
            ScenarioEvidence::new_at_dir(&world, failure_dir.clone(), [secret.to_string()])
                .unwrap();
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
        let report = evidence.finish(true).unwrap_err().to_string();
        assert!(report.contains("- command 0 stdout"), "{report}");
        assert!(report.contains("- command 0 stderr"), "{report}");
        assert!(!report.contains(secret), "{report}");

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

    #[test]
    fn artifact_name_rejects_non_component_and_reserved_inputs() {
        for invalid in ["", ".", "..", "a/b", "a/../b", "/tmp/x", ".claims", "a\0b"] {
            assert!(
                matches!(
                    validate_artifact_name(OsStr::new(invalid)),
                    Err(EvidenceError::InvalidArtifactName)
                ),
                "accepted invalid artifact name {invalid:?}"
            );
        }
        validate_artifact_name(OsStr::new("scenario-name")).unwrap();
    }

    #[test]
    fn scenario_evidence_rejects_arbitrary_target_paths() {
        let world = ScenarioWorld::new("checked-evidence-target").unwrap();
        let root = TempDir::new().unwrap();
        let parent = root.path().join("nested");
        let target = parent.join("scenario");
        std::fs::create_dir_all(&target).unwrap();
        let marker = target.join("keep.txt");
        std::fs::write(&marker, "keep").unwrap();

        let result = ScenarioEvidence::new(
            &world,
            target.to_string_lossy().as_ref(),
            Vec::<String>::new(),
        );

        assert!(matches!(result, Err(EvidenceError::InvalidArtifactName)));
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "keep");
    }

    #[test]
    fn artifact_claim_precedes_cleanup_and_releases_after_drop() {
        let root = TempDir::new().unwrap();
        let target = root.path().join("scenario");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("stale.txt"), "stale").unwrap();

        let first = ArtifactClaim::acquire(&target).unwrap();
        clear_stale_bundle(&target).unwrap();
        let lock = root.path().join(".claims/scenario.lock");
        assert!(root.path().join(".claims").is_dir());
        assert!(lock.is_file());
        assert!(matches!(
            ArtifactClaim::acquire(&target),
            Err(EvidenceError::ArtifactClaimBusy)
        ));
        drop(first);
        let second = ArtifactClaim::acquire(&target).unwrap();
        assert!(lock.is_file());
        drop(second);
    }

    /// 失败方式（#228）：同一进程里另一个线程 fork 出的子进程在 exec 之前共享锁文件的
    /// 打开描述，claim 释放后锁仍被子进程的副本持有，紧接着的重新获取得到
    /// ArtifactClaimBusy。这里用一个只 fork、不 exec 的子进程确定性地制造该窗口。
    #[test]
    fn released_claim_is_free_while_a_forked_child_still_shares_the_descriptor() {
        let root = TempDir::new().unwrap();
        let target = root.path().join("scenario");
        let first = ArtifactClaim::acquire(&target).unwrap();

        // fork 返回时子进程已经复制了全部描述符，包括锁文件的打开描述；子进程只
        // pause()，由父进程在断言前 SIGKILL 并回收，所以不存在等待它自行退出的路径。
        // SAFETY: 子进程只调用 async-signal-safe 的 pause 与 _exit。
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            // SAFETY: 见上。
            unsafe {
                libc::pause();
                libc::_exit(0);
            }
        }

        drop(first);
        let second = ArtifactClaim::acquire(&target);

        // SAFETY: `child` 是本测试刚 fork 出的子进程。
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        }
        assert!(
            second.is_ok(),
            "released claim must be reacquirable while a forked child shares the lock descriptor"
        );
    }

    #[test]
    fn scan_collection_preserves_leak_and_later_scan_error() {
        let dir = TempDir::new().unwrap();
        let leaked_file = dir.path().join("leak.txt");
        std::fs::write(&leaked_file, "value scan-secret").unwrap();
        std::os::unix::fs::symlink(&leaked_file, dir.path().join("invalid-link")).unwrap();

        let collection = scan_artifacts_collecting(dir.path(), &["scan-secret"]);
        assert_eq!(collection.leak_paths, vec![leaked_file.clone()]);
        assert!(
            collection
                .errors
                .iter()
                .any(|error| error.path == Path::new("invalid-link"))
        );
        let patched = std::fs::read_to_string(leaked_file).unwrap();
        assert_eq!(patched, "value [REDACTED]");
    }

    #[test]
    fn scenario_evidence_preserves_primary_two_write_errors_and_scan_results() {
        let secret = "direct-report-secret";
        let world = ScenarioWorld::new("direct-report-errors").unwrap();
        let root = TempDir::new().unwrap();
        let failure_dir = root.path().join("scenario");
        let mut evidence =
            ScenarioEvidence::new_at_dir(&world, failure_dir.clone(), [secret.to_string()])
                .unwrap();
        evidence.record_command(format!("lattice {secret}"), &report_test_outcome(secret));

        std::fs::create_dir_all(&failure_dir).unwrap();
        std::fs::create_dir(failure_dir.join("0-command.txt")).unwrap();
        std::fs::create_dir(failure_dir.join("0-stdout.txt")).unwrap();
        std::fs::write(failure_dir.join("leak.txt"), format!("leak {secret}")).unwrap();
        std::os::unix::fs::symlink(
            failure_dir.join("leak.txt"),
            failure_dir.join("invalid-link"),
        )
        .unwrap();

        let report = evidence
            .finish_report(Some(format!("primary {secret}")))
            .unwrap_err();
        let text = report.to_string();
        assert!(!text.contains(secret));
        assert!(text.contains("primary [REDACTED]"));
        assert!(text.contains("leak.txt"));
        assert!(text.contains("command diagnostics:"));
        assert!(text.contains("outcome: CommandOutcome"));
        let command_error = text.find("write 0-command.txt").unwrap();
        let stdout_error = text.find("write 0-stdout.txt").unwrap();
        assert!(command_error < stdout_error);
        assert!(text.contains("invalid-link"));
        assert_eq!(
            std::fs::read_to_string(failure_dir.join("leak.txt")).unwrap(),
            "leak [REDACTED]"
        );
    }

    #[test]
    fn failure_bundle_redacts_pty_and_json_attachments() {
        let world = ScenarioWorld::new("attachment-evidence").unwrap();
        let root = TempDir::new().unwrap();
        let failure = root.path().join("failure");
        let mut evidence =
            ScenarioEvidence::new_at_dir(&world, failure.clone(), ["secret-token".into()]).unwrap();
        evidence.record_pty(b"answer secret-token");
        evidence
            .record_json("cut", &json!({"marker":"secret-token","sequence":65}))
            .unwrap();
        assert!(evidence.record_json("../escape", &json!({})).is_err());
        assert!(evidence.finish_report(Some("primary".into())).is_err());
        assert!(
            !std::fs::read_to_string(failure.join("0-pty.txt"))
                .unwrap()
                .contains("secret-token")
        );
        let cut: serde_json::Value =
            serde_json::from_slice(&std::fs::read(failure.join("cut.json")).unwrap()).unwrap();
        assert_eq!(cut, json!({"marker":"[REDACTED]","sequence":65}));
    }

    #[test]
    fn direct_finalizer_scans_raw_evidence_whatever_the_assertion_result() {
        let secret = "direct-finalizer-secret";
        let world = ScenarioWorld::new("direct-finalizer-raw-scan").unwrap();
        let root = TempDir::new().unwrap();
        for primary in [None, Some("business failed")] {
            for location in [
                "command 0 stdout",
                "command 0 stderr",
                "PTY 0",
                "JSON cut",
                "capture 0",
            ] {
                let failure = root
                    .path()
                    .join(format!("{}-{location}", primary.is_some()));
                let mut evidence =
                    ScenarioEvidence::new_at_dir(&world, failure.clone(), [secret.to_string()])
                        .unwrap();
                let mut command = report_test_outcome("clean");
                let mut bytes = vec![0xff];
                bytes.extend_from_slice(secret.as_bytes());
                match location {
                    "command 0 stdout" => command.stdout = bytes,
                    "command 0 stderr" => command.stderr = bytes,
                    "PTY 0" => evidence.record_pty(&bytes),
                    "JSON cut" => evidence
                        .record_json("cut", &json!({"secret":secret}))
                        .unwrap(),
                    "capture 0" => evidence.record_captures(vec![CapturedRequest {
                        raw: json!({"messages":[{"role":"tool","content":secret}]}),
                        ..Default::default()
                    }]),
                    _ => unreachable!(),
                }
                evidence.record_command("lattice test", &command);
                let report = evidence
                    .finish_report(primary.map(str::to_string))
                    .unwrap_err();
                let text = report.to_string();
                assert!(text.contains(&format!("- {location}\n")), "{text}");
                assert_eq!(
                    text.contains("business failed"),
                    primary.is_some(),
                    "{text}"
                );
                assert!(!text.contains(secret), "{text}");
                assert!(failure.join("0-stdout.txt").is_file());
                assert!(
                    scan_artifacts_for_secrets(&failure, &[secret])
                        .unwrap()
                        .is_empty()
                );
            }
        }
    }

    #[test]
    fn scenario_evidence_records_command_diagnostics_before_persistence() {
        let world = ScenarioWorld::new("direct-report-create-error").unwrap();
        let root = TempDir::new().unwrap();
        let failure_dir = root.path().join("scenario");
        let mut evidence =
            ScenarioEvidence::new_at_dir(&world, failure_dir.clone(), Vec::<String>::new())
                .unwrap();
        evidence.record_command("lattice test", &report_test_outcome("clean"));
        std::fs::write(&failure_dir, "root type conflict").unwrap();

        let report = evidence
            .finish_report(Some("primary assertion".into()))
            .unwrap_err();
        let text = report.to_string();
        assert!(text.contains("primary assertion"));
        assert!(text.contains("command diagnostics:"));
        assert!(text.contains("outcome: CommandOutcome"));
        assert!(text.contains("create artifact root"));
    }

    #[test]
    fn workspace_snapshot_records_symlink_without_following_target() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("project");
        let target = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("child.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(&target, root.join("linked-dir")).unwrap();

        let snapshot = WorkspaceSnapshot::capture(&root).unwrap();
        let paths: Vec<&str> = snapshot
            .entries
            .iter()
            .map(|(path, _)| path.as_str())
            .collect();
        assert!(paths.contains(&"linked-dir"));
        assert!(!paths.iter().any(|path| path.contains("child.txt")));
    }

    #[test]
    fn world_context_final_buffer_redacts_dynamic_fields() {
        let secret = "world-dynamic-secret";
        let world = ScenarioWorld::new(&format!("world-{secret}")).unwrap();
        let out_dir = TempDir::new().unwrap();
        write_world_context(
            out_dir.path(),
            &world,
            &format!("scenario-{secret}"),
            &format!("lattice {secret}"),
            &[secret],
        )
        .unwrap();

        let text = std::fs::read_to_string(out_dir.path().join("world.txt")).unwrap();
        assert!(!text.contains(secret));
        assert!(text.contains("scenario-[REDACTED]"));
        assert!(text.contains("lattice [REDACTED]"));
        assert!(text.contains(REDACTED));
    }
}
