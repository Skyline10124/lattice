// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Host capability probes and boundary gate helpers.

use lancet_authority::{
    allowed_primitives_for, EffectiveBoundaryCredit, LandlockApplied, MinimalMountViewApplied,
    MountNamespaceApplied, NetnsApplied,
};
use lancet_protocol::{
    BoundaryDimension, BoundaryPrimitive, EffectiveBoundaryEntry, EffectiveBoundaryReport,
    EffectiveBoundaryStatus, HostProbeStatus,
};
pub use lancet_protocol::{HostProbeEntry, HostProbeReport};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::executor::supervisor::landlock;

/// Runtime mode for command physical-boundary preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandIsolationMode {
    Enforce,
    Audit,
    Off,
}

impl CommandIsolationMode {
    pub const fn label(self) -> &'static str {
        match self {
            CommandIsolationMode::Enforce => "enforce",
            CommandIsolationMode::Audit => "audit",
            CommandIsolationMode::Off => "off",
        }
    }
}

/// Runtime Landlock behavior for command physical-boundary preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandIsolationLandlockBehavior {
    Auto,
    Off,
}

/// Resolved unsupported-host behavior for command physical-boundary preflight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandIsolationUnsupportedHostBehavior {
    FailClosed,
    AllowUnbounded,
    NotRequested,
}

/// Turn-scoped command physical-boundary runtime configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandIsolationRuntimeConfig {
    pub mode: CommandIsolationMode,
    pub required_boundaries: Vec<BoundaryDimension>,
    pub optional_boundaries: Vec<BoundaryDimension>,
    pub read_only_toolchain_paths: Vec<PathBuf>,
    pub scratch_paths: Vec<PathBuf>,
    pub landlock: CommandIsolationLandlockBehavior,
    pub unsupported_host: CommandIsolationUnsupportedHostBehavior,
    /// Optional test/embedder-supplied host probe. Production leaves this unset
    /// so command preflight evaluates the live host on each exec.command_run.
    pub host_probe: Option<HostProbeReport>,
}

/// Applied evidence reported by the native command supervisor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppliedBoundaryEvidence {
    NetworkNamespace(NetnsApplied),
    MountNamespace(MountNamespaceApplied),
    MinimalMountView(MinimalMountViewApplied),
    Landlock(LandlockApplied),
}

/// Stable setup status for supervisor stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorSetupStatus {
    NotAttempted,
    Applied,
    NotApplied,
    FailedClosed,
    Unsupported,
}

impl SupervisorSetupStatus {
    pub const fn label(self) -> &'static str {
        match self {
            SupervisorSetupStatus::NotAttempted => "not_attempted",
            SupervisorSetupStatus::Applied => "applied",
            SupervisorSetupStatus::NotApplied => "not_applied",
            SupervisorSetupStatus::FailedClosed => "failed_closed",
            SupervisorSetupStatus::Unsupported => "unsupported",
        }
    }
}

/// Stable status for the Phase 29 minimal mount-view setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinimalMountViewStatus {
    NotAttempted,
    Applied,
    NotApplied,
    FailedClosed,
}

impl MinimalMountViewStatus {
    pub const fn label(self) -> &'static str {
        match self {
            MinimalMountViewStatus::NotAttempted => "not_attempted",
            MinimalMountViewStatus::Applied => "applied",
            MinimalMountViewStatus::NotApplied => "not_applied",
            MinimalMountViewStatus::FailedClosed => "failed_closed",
        }
    }
}

/// Stable Landlock application status values carried in boundary diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandlockApplicationStatus {
    Unsupported,
    NotApplied,
    Applied,
}

impl LandlockApplicationStatus {
    pub const fn label(self) -> &'static str {
        match self {
            LandlockApplicationStatus::Unsupported => "unsupported",
            LandlockApplicationStatus::NotApplied => "not_applied",
            LandlockApplicationStatus::Applied => "applied",
        }
    }
}

/// Supervisor setup result used to build command effective-boundary metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryAttemptOutcome {
    pub evidence: Vec<AppliedBoundaryEvidence>,
    pub network_namespace_status: SupervisorSetupStatus,
    pub mount_namespace_status: SupervisorSetupStatus,
    pub minimal_mount_view_status: MinimalMountViewStatus,
    pub privilege_strategy: Option<String>,
    pub userns_status: Option<String>,
    pub setgroups_status: Option<String>,
    pub uid_map_status: Option<String>,
    pub gid_map_status: Option<String>,
    pub read_only_bind_scope: Option<String>,
    pub landlock_status: LandlockApplicationStatus,
    pub landlock_abi_version: Option<u32>,
    pub landlock_rights_mask: Option<u64>,
    pub landlock_ruleset_status: Option<String>,
    pub landlock_required: Option<bool>,
    pub supervisor_backend: Option<String>,
    pub project_view_path: Option<PathBuf>,
    pub cwd_view_path: Option<PathBuf>,
    pub fd_cleanup_status: Option<String>,
    pub fd_cleanup_method: Option<String>,
    pub env_policy: Option<String>,
    pub setup_failure_stage: Option<String>,
    pub setup_failure_reason: Option<String>,
    pub failed_closed: bool,
}

impl Default for BoundaryAttemptOutcome {
    fn default() -> Self {
        Self {
            evidence: Vec::new(),
            network_namespace_status: SupervisorSetupStatus::NotAttempted,
            mount_namespace_status: SupervisorSetupStatus::NotAttempted,
            minimal_mount_view_status: MinimalMountViewStatus::NotAttempted,
            privilege_strategy: None,
            userns_status: None,
            setgroups_status: None,
            uid_map_status: None,
            gid_map_status: None,
            read_only_bind_scope: None,
            landlock_status: LandlockApplicationStatus::NotApplied,
            landlock_abi_version: None,
            landlock_rights_mask: None,
            landlock_ruleset_status: None,
            landlock_required: None,
            supervisor_backend: None,
            project_view_path: None,
            cwd_view_path: None,
            fd_cleanup_status: None,
            fd_cleanup_method: None,
            env_policy: None,
            setup_failure_stage: None,
            setup_failure_reason: None,
            failed_closed: false,
        }
    }
}

/// One command isolation attempt. Later plans wire this to the live supervisor;
/// Wave 0 tests construct it directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandIsolationAttempt {
    pub outcome: BoundaryAttemptOutcome,
}

impl CommandIsolationAttempt {
    pub fn new(outcome: BoundaryAttemptOutcome) -> Self {
        Self { outcome }
    }
}

impl Default for CommandIsolationRuntimeConfig {
    fn default() -> Self {
        Self {
            mode: CommandIsolationMode::Off,
            required_boundaries: Vec::new(),
            optional_boundaries: Vec::new(),
            read_only_toolchain_paths: Vec::new(),
            scratch_paths: Vec::new(),
            landlock: CommandIsolationLandlockBehavior::Auto,
            unsupported_host: CommandIsolationUnsupportedHostBehavior::NotRequested,
            host_probe: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProbeOutcome {
    status: HostProbeStatus,
    reason: String,
    diagnostics: BTreeMap<String, String>,
}

impl ProbeOutcome {
    fn new(status: HostProbeStatus, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.into(),
            diagnostics: BTreeMap::new(),
        }
    }

    fn with_diagnostic(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.diagnostics.insert(key.into(), value.into());
        self
    }
}

trait PrimitiveProbeRunner {
    fn probe(&self, primitive: BoundaryPrimitive) -> Option<HostProbeEntry>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbePlatform {
    Linux,
    Unsupported(&'static str),
}

impl ProbePlatform {
    fn current() -> Self {
        if cfg!(target_os = "linux") {
            ProbePlatform::Linux
        } else {
            ProbePlatform::Unsupported("unsupported_platform")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildProbeKind {
    NetworkNamespace,
    MountNamespace,
    Overlayfs,
}

trait ChildProbeExecutor {
    fn run_child_probe(&self, kind: ChildProbeKind) -> ProbeOutcome;
}

impl<T: ChildProbeExecutor + ?Sized> ChildProbeExecutor for &T {
    fn run_child_probe(&self, kind: ChildProbeKind) -> ProbeOutcome {
        (**self).run_child_probe(kind)
    }
}

struct LiveProbeRunner<C = CommandChildProbe> {
    child: C,
}

impl LiveProbeRunner<CommandChildProbe> {
    fn new() -> Self {
        Self {
            child: CommandChildProbe,
        }
    }
}

impl<C> LiveProbeRunner<C> {
    #[cfg(test)]
    fn with_child_executor(child: C) -> Self {
        Self { child }
    }
}

impl<C: ChildProbeExecutor> PrimitiveProbeRunner for LiveProbeRunner<C> {
    fn probe(&self, primitive: BoundaryPrimitive) -> Option<HostProbeEntry> {
        let outcome = match primitive {
            BoundaryPrimitive::NetworkNamespace => {
                self.child.run_child_probe(ChildProbeKind::NetworkNamespace)
            }
            BoundaryPrimitive::MountNamespace => {
                self.child.run_child_probe(ChildProbeKind::MountNamespace)
            }
            BoundaryPrimitive::Overlayfs => self.child.run_child_probe(ChildProbeKind::Overlayfs),
            BoundaryPrimitive::Landlock => probe_landlock_abi(),
        };
        Some(entry_from_outcome(primitive, outcome))
    }
}

struct CommandChildProbe;

impl ChildProbeExecutor for CommandChildProbe {
    fn run_child_probe(&self, kind: ChildProbeKind) -> ProbeOutcome {
        let Some(unshare) = trusted_helper(TRUSTED_UNSHARE_PATHS) else {
            return missing_trusted_helper("command_boundary", "unshare");
        };
        match kind {
            ChildProbeKind::NetworkNamespace => {
                let Some(true_path) = trusted_helper(TRUSTED_TRUE_PATHS) else {
                    return missing_trusted_helper("network_namespace", "true")
                        .with_diagnostic("helper.unshare", unshare);
                };
                run_command_probe(
                    "network_namespace",
                    &[
                        CommandAttempt::new(
                            unshare,
                            vec!["--user", "--map-root-user", "--net", true_path],
                        )
                        .with_diagnostic("helper.unshare", unshare)
                        .with_diagnostic("helper.true", true_path),
                        CommandAttempt::new(unshare, vec!["--net", true_path])
                            .with_diagnostic("helper.unshare", unshare)
                            .with_diagnostic("helper.true", true_path),
                    ],
                )
            }
            ChildProbeKind::MountNamespace => {
                let Some(true_path) = trusted_helper(TRUSTED_TRUE_PATHS) else {
                    return missing_trusted_helper("mount_namespace", "true")
                        .with_diagnostic("helper.unshare", unshare);
                };
                run_command_probe(
                    "mount_namespace",
                    &[
                        CommandAttempt::new(
                            unshare,
                            vec!["--user", "--map-root-user", "--mount", true_path],
                        )
                        .with_diagnostic("helper.unshare", unshare)
                        .with_diagnostic("helper.true", true_path),
                        CommandAttempt::new(unshare, vec!["--mount", true_path])
                            .with_diagnostic("helper.unshare", unshare)
                            .with_diagnostic("helper.true", true_path),
                    ],
                )
            }
            ChildProbeKind::Overlayfs => {
                let Some(sh) = trusted_helper(TRUSTED_SH_PATHS) else {
                    return missing_trusted_helper("overlayfs", "sh")
                        .with_diagnostic("helper.unshare", unshare);
                };
                run_command_probe(
                    "overlayfs",
                    &[CommandAttempt::new(
                        unshare,
                        vec![
                            "--user",
                            "--map-root-user",
                            "--mount",
                            sh,
                            "-c",
                            OVERLAYFS_PROBE_SCRIPT,
                        ],
                    )
                    .with_diagnostic("helper.unshare", unshare)
                    .with_diagnostic("helper.sh", sh)],
                )
            }
        }
    }
}

struct CommandAttempt {
    program: &'static str,
    args: Vec<String>,
    diagnostics: BTreeMap<String, String>,
}

impl CommandAttempt {
    fn new(program: &'static str, args: Vec<&'static str>) -> Self {
        Self {
            program,
            args: args.into_iter().map(str::to_string).collect(),
            diagnostics: BTreeMap::new(),
        }
    }

    fn with_diagnostic(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.diagnostics.insert(key.into(), value.into());
        self
    }
}

const TRUSTED_UNSHARE_PATHS: &[&str] = &["/usr/bin/unshare", "/bin/unshare"];
const TRUSTED_SH_PATHS: &[&str] = &["/usr/bin/sh", "/bin/sh"];
const TRUSTED_TRUE_PATHS: &[&str] = &["/usr/bin/true", "/bin/true"];
const TRUSTED_PROBE_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

const OVERLAYFS_PROBE_SCRIPT: &str = r#"set -eu
base="$(mktemp -d)"
cleanup() {
  umount "$base/merged" 2>/dev/null || true
  rm -rf "$base"
}
trap cleanup EXIT
mkdir -p "$base/lower" "$base/upper" "$base/work" "$base/merged"
printf probe > "$base/lower/probe"
mount -t overlay overlay -o "lowerdir=$base/lower,upperdir=$base/upper,workdir=$base/work" "$base/merged"
test -f "$base/merged/probe"
umount "$base/merged"
"#;

fn run_command_probe(label: &str, attempts: &[CommandAttempt]) -> ProbeOutcome {
    let mut last_failure: Option<ProbeOutcome> = None;
    for attempt in attempts {
        match Command::new(attempt.program)
            .args(&attempt.args)
            .env("PATH", TRUSTED_PROBE_PATH)
            .output()
        {
            Ok(output) if output.status.success() => {
                return apply_attempt_diagnostics(
                    ProbeOutcome::new(
                        HostProbeStatus::Available,
                        format!("{label} child probe succeeded"),
                    ),
                    attempt,
                );
            }
            Ok(output) => {
                last_failure = Some(apply_attempt_diagnostics(
                    classify_command_failure(label, &output),
                    attempt,
                ));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                last_failure = Some(apply_attempt_diagnostics(
                    ProbeOutcome::new(
                        HostProbeStatus::Unknown,
                        format!(
                            "{label} trusted probe helper `{}` not found",
                            attempt.program
                        ),
                    ),
                    attempt,
                ));
            }
            Err(err) => {
                last_failure = Some(apply_attempt_diagnostics(
                    ProbeOutcome::new(
                        HostProbeStatus::Unknown,
                        format!("{label} child probe spawn failed: {err}"),
                    ),
                    attempt,
                ));
            }
        }
    }
    last_failure.unwrap_or_else(|| {
        ProbeOutcome::new(
            HostProbeStatus::Unknown,
            format!("{label} child probe had no attempts"),
        )
    })
}

fn apply_attempt_diagnostics(mut outcome: ProbeOutcome, attempt: &CommandAttempt) -> ProbeOutcome {
    for (key, value) in &attempt.diagnostics {
        outcome = outcome.with_diagnostic(key.clone(), value.clone());
    }
    outcome
}

fn trusted_helper(paths: &'static [&'static str]) -> Option<&'static str> {
    paths.iter().copied().find(|path| Path::new(path).is_file())
}

fn missing_trusted_helper(label: &str, helper: &str) -> ProbeOutcome {
    ProbeOutcome::new(
        HostProbeStatus::Unknown,
        format!("{label} trusted probe helper `{helper}` not found"),
    )
}

fn classify_command_failure(label: &str, output: &Output) -> ProbeOutcome {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let reason = if stderr.is_empty() {
        format!("{label} child probe exited with {}", output.status)
    } else {
        format!("{label} child probe failed: {stderr}")
    };

    let lowered = stderr.to_ascii_lowercase();
    let status = if lowered.contains("operation not permitted")
        || lowered.contains("permission denied")
        || lowered.contains("not permitted")
    {
        HostProbeStatus::Unavailable
    } else if lowered.contains("not supported")
        || lowered.contains("invalid argument")
        || lowered.contains("kernel does not support")
    {
        HostProbeStatus::Disabled
    } else {
        HostProbeStatus::Unknown
    };

    ProbeOutcome::new(status, reason)
}

#[cfg(target_os = "linux")]
fn probe_landlock_abi() -> ProbeOutcome {
    let probe = landlock::probe_landlock_abi();
    if let Some(abi) = probe.abi_version {
        ProbeOutcome::new(HostProbeStatus::Available, probe.reason)
            .with_diagnostic("landlock_abi_version", abi.to_string())
    } else {
        ProbeOutcome::new(HostProbeStatus::Disabled, probe.reason)
    }
}

#[cfg(not(target_os = "linux"))]
fn probe_landlock_abi() -> ProbeOutcome {
    ProbeOutcome::new(HostProbeStatus::Unavailable, "unsupported_platform")
}

fn entry_from_outcome(primitive: BoundaryPrimitive, outcome: ProbeOutcome) -> HostProbeEntry {
    let mut entry = HostProbeEntry::new(primitive, outcome.status, outcome.reason);
    for (key, value) in outcome.diagnostics {
        entry = entry.with_diagnostic(key, value);
    }
    entry
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingRequiredBoundary {
    pub boundary: BoundaryDimension,
    pub primitive_reasons: Vec<HostProbeEntry>,
}

pub fn unavailable_required_boundaries(
    report: &HostProbeReport,
    required: &[BoundaryDimension],
) -> Vec<MissingRequiredBoundary> {
    required
        .iter()
        .filter_map(|boundary| {
            let primitive_reasons: Vec<_> = allowed_primitives_for(*boundary)
                .iter()
                .map(|primitive| {
                    report.status_for(*primitive).cloned().unwrap_or_else(|| {
                        HostProbeEntry::new(
                            *primitive,
                            HostProbeStatus::Unknown,
                            "probe_result_missing",
                        )
                    })
                })
                .filter(|entry| entry.status != HostProbeStatus::Available)
                .collect();

            let any_available = allowed_primitives_for(*boundary).iter().any(|primitive| {
                report
                    .status_for(*primitive)
                    .map(|entry| entry.status == HostProbeStatus::Available)
                    .unwrap_or(false)
            });

            (!any_available).then_some(MissingRequiredBoundary {
                boundary: *boundary,
                primitive_reasons,
            })
        })
        .collect()
}

/// Command-boundary preflight outcome for a single `exec.command_run` attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandBoundaryPreflight {
    pub effective_boundary: EffectiveBoundaryReport,
    pub host_probe: HostProbeReport,
    pub boundary_setup_failed_closed: bool,
    pub failed_closed: bool,
    pub fail_reasons: Vec<String>,
}

impl CommandBoundaryPreflight {
    pub fn should_fail_closed(&self) -> bool {
        self.failed_closed
    }

    pub fn failure_message(&self) -> String {
        if self.fail_reasons.is_empty() {
            "command physical boundary preflight failed closed".to_string()
        } else {
            format!(
                "command physical boundary preflight failed closed: {}",
                self.fail_reasons.join("; ")
            )
        }
    }
}

/// Evaluate command isolation against the current host probe without claiming
/// physical protection unless later supervisor phases provide applied evidence.
pub fn evaluate_command_boundary_preflight(
    config: &CommandIsolationRuntimeConfig,
) -> CommandBoundaryPreflight {
    evaluate_command_boundary_attempt(config, None)
}

/// Build command effective-boundary metadata from host probes plus optional
/// applied supervisor evidence. Host probe availability alone never becomes
/// physically-bounded credit.
pub fn evaluate_command_boundary_attempt(
    config: &CommandIsolationRuntimeConfig,
    attempt: Option<&CommandIsolationAttempt>,
) -> CommandBoundaryPreflight {
    let host_probe = config
        .host_probe
        .clone()
        .unwrap_or_else(probe_host_capabilities);

    let entries: Vec<_> = BoundaryDimension::ALL
        .iter()
        .map(|boundary| effective_boundary_entry(config, &host_probe, *boundary, attempt))
        .collect();
    let effective_boundary = EffectiveBoundaryReport::from_entries(entries);
    let mut fail_reasons = Vec::new();

    if config.mode == CommandIsolationMode::Enforce {
        for boundary in &config.required_boundaries {
            if let Some(entry) = effective_boundary.status_for(*boundary) {
                if entry.status != EffectiveBoundaryStatus::PhysicallyBounded {
                    fail_reasons.push(entry.reason.clone().unwrap_or_else(|| {
                        format!("{} boundary lacks applied evidence", boundary.label())
                    }));
                }
            }
        }
    }

    CommandBoundaryPreflight {
        effective_boundary,
        host_probe,
        boundary_setup_failed_closed: attempt
            .map(|attempt| attempt.outcome.failed_closed)
            .unwrap_or(false),
        failed_closed: !fail_reasons.is_empty(),
        fail_reasons,
    }
}

fn effective_boundary_entry(
    config: &CommandIsolationRuntimeConfig,
    host_probe: &HostProbeReport,
    boundary: BoundaryDimension,
    attempt: Option<&CommandIsolationAttempt>,
) -> EffectiveBoundaryEntry {
    if config.mode == CommandIsolationMode::Off {
        return EffectiveBoundaryEntry::new(boundary, EffectiveBoundaryStatus::NotRequested)
            .with_reason("command isolation mode is off");
    }

    let required = config.required_boundaries.contains(&boundary);
    let optional = config.optional_boundaries.contains(&boundary);
    if !required && !optional {
        return EffectiveBoundaryEntry::new(boundary, EffectiveBoundaryStatus::NotRequested)
            .with_reason("boundary not requested");
    }

    if let Some(attempt) = attempt {
        if let Some(credit) = applied_credit_for_boundary(boundary, &attempt.outcome) {
            return with_attempt_diagnostics(
                EffectiveBoundaryEntry::new(boundary, credit.status())
                    .with_reason("applied supervisor evidence accepted")
                    .with_source_primitive(credit.source_primitive())
                    .with_diagnostic("request", if required { "required" } else { "optional" }),
                boundary,
                &attempt.outcome,
            );
        }

        let status = attempted_missing_status(config, boundary, required, &attempt.outcome);
        let request_kind = if required { "required" } else { "optional" };
        let reason = match status {
            EffectiveBoundaryStatus::FailedClosed => format!(
                "{request_kind} {} boundary requested but supervisor attempt lacked applied evidence",
                boundary.label()
            ),
            EffectiveBoundaryStatus::Unsupported => format!(
                "{request_kind} {} boundary unsupported by supervisor attempt",
                boundary.label()
            ),
            EffectiveBoundaryStatus::Unbounded => format!(
                "{request_kind} {} boundary requested but supervisor attempt did not apply it",
                boundary.label()
            ),
            EffectiveBoundaryStatus::Unknown => format!(
                "{request_kind} {} boundary attempt status unknown",
                boundary.label()
            ),
            EffectiveBoundaryStatus::NotRequested => "boundary not requested".to_string(),
            EffectiveBoundaryStatus::PhysicallyBounded => "boundary physically bounded".to_string(),
        };
        return with_attempt_diagnostics(
            EffectiveBoundaryEntry::new(boundary, status)
                .with_reason(reason)
                .with_diagnostic("request", request_kind),
            boundary,
            &attempt.outcome,
        );
    }

    let primitive_entries = primitive_entries_for_boundary(boundary, host_probe);
    let any_unknown = primitive_entries
        .iter()
        .any(|entry| entry.status == HostProbeStatus::Unknown);
    let all_unsupported = !primitive_entries.is_empty()
        && primitive_entries
            .iter()
            .all(|entry| entry.reason.as_deref() == Some("unsupported_platform"));
    let primitive_summary = primitive_entries
        .iter()
        .map(|entry| {
            format!(
                "{}={:?}:{}",
                entry.primitive.label(),
                entry.status,
                entry.reason.as_deref().unwrap_or("no_reason")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    let status = match (config.mode, required, any_unknown, all_unsupported) {
        (CommandIsolationMode::Enforce, true, true, _) => EffectiveBoundaryStatus::Unknown,
        (CommandIsolationMode::Enforce, true, false, true) => EffectiveBoundaryStatus::Unsupported,
        (CommandIsolationMode::Enforce, true, false, false) => {
            EffectiveBoundaryStatus::FailedClosed
        }
        (CommandIsolationMode::Audit, _, true, _) => EffectiveBoundaryStatus::Unknown,
        (CommandIsolationMode::Audit, _, _, _) | (CommandIsolationMode::Enforce, false, _, _) => {
            EffectiveBoundaryStatus::Unbounded
        }
        (CommandIsolationMode::Off, _, _, _) => EffectiveBoundaryStatus::NotRequested,
    };

    let request_kind = if required { "required" } else { "optional" };
    let reason = match status {
        EffectiveBoundaryStatus::FailedClosed => format!(
            "{request_kind} {} boundary requested but no applied primitive evidence exists ({primitive_summary})",
            boundary.label()
        ),
        EffectiveBoundaryStatus::Unsupported => format!(
            "{request_kind} {} boundary unsupported on this host ({primitive_summary})",
            boundary.label()
        ),
        EffectiveBoundaryStatus::Unknown => format!(
            "{request_kind} {} boundary probe unknown or missing ({primitive_summary})",
            boundary.label()
        ),
        EffectiveBoundaryStatus::Unbounded => format!(
            "{request_kind} {} boundary requested but command ran without applied primitive evidence ({primitive_summary})",
            boundary.label()
        ),
        EffectiveBoundaryStatus::NotRequested => "boundary not requested".to_string(),
        EffectiveBoundaryStatus::PhysicallyBounded => "boundary physically bounded".to_string(),
    };

    EffectiveBoundaryEntry::new(boundary, status)
        .with_reason(reason)
        .with_diagnostic("request", request_kind)
}

fn applied_credit_for_boundary(
    boundary: BoundaryDimension,
    outcome: &BoundaryAttemptOutcome,
) -> Option<EffectiveBoundaryCredit> {
    if !has_env_fd_hygiene(outcome) {
        return None;
    }

    match boundary {
        BoundaryDimension::Network => has_netns_evidence(outcome).then(|| {
            EffectiveBoundaryCredit::network_physically_bounded(
                NetnsApplied::from_supervisor_netns_success(),
            )
        }),
        BoundaryDimension::Filesystem => {
            if has_mount_namespace_evidence(outcome)
                && has_minimal_mount_view_evidence(outcome)
                && outcome.minimal_mount_view_status == MinimalMountViewStatus::Applied
            {
                EffectiveBoundaryCredit::filesystem_physically_bounded_from_supervisor_minimal_mount_view_success(
                    MountNamespaceApplied::from_supervisor_mount_namespace_success(),
                    MinimalMountViewApplied::from_supervisor_minimal_mount_view_success(),
                )
                .ok()
            } else {
                None
            }
        }
        BoundaryDimension::Landlock => {
            if has_landlock_evidence(outcome)
                && outcome.landlock_status == LandlockApplicationStatus::Applied
            {
                Some(EffectiveBoundaryCredit::landlock_physically_bounded(
                    LandlockApplied::from_supervisor_landlock_success(),
                ))
            } else {
                None
            }
        }
    }
}

fn attempted_missing_status(
    config: &CommandIsolationRuntimeConfig,
    boundary: BoundaryDimension,
    required: bool,
    outcome: &BoundaryAttemptOutcome,
) -> EffectiveBoundaryStatus {
    if boundary == BoundaryDimension::Landlock
        && outcome.landlock_status == LandlockApplicationStatus::Unsupported
    {
        return EffectiveBoundaryStatus::Unsupported;
    }
    if config.mode == CommandIsolationMode::Audit
        && config.unsupported_host == CommandIsolationUnsupportedHostBehavior::AllowUnbounded
    {
        return EffectiveBoundaryStatus::Unbounded;
    }
    if outcome.failed_closed || (config.mode == CommandIsolationMode::Enforce && required) {
        EffectiveBoundaryStatus::FailedClosed
    } else {
        EffectiveBoundaryStatus::Unbounded
    }
}

fn has_netns_evidence(outcome: &BoundaryAttemptOutcome) -> bool {
    outcome
        .evidence
        .iter()
        .any(|evidence| matches!(evidence, AppliedBoundaryEvidence::NetworkNamespace(_)))
}

fn has_mount_namespace_evidence(outcome: &BoundaryAttemptOutcome) -> bool {
    outcome
        .evidence
        .iter()
        .any(|evidence| matches!(evidence, AppliedBoundaryEvidence::MountNamespace(_)))
}

fn has_minimal_mount_view_evidence(outcome: &BoundaryAttemptOutcome) -> bool {
    outcome
        .evidence
        .iter()
        .any(|evidence| matches!(evidence, AppliedBoundaryEvidence::MinimalMountView(_)))
}

fn has_landlock_evidence(outcome: &BoundaryAttemptOutcome) -> bool {
    outcome
        .evidence
        .iter()
        .any(|evidence| matches!(evidence, AppliedBoundaryEvidence::Landlock(_)))
}

fn has_env_fd_hygiene(outcome: &BoundaryAttemptOutcome) -> bool {
    outcome.env_policy.as_deref() == Some("allowlist")
        && outcome.fd_cleanup_status.as_deref() == Some("applied")
}

fn with_attempt_diagnostics(
    mut entry: EffectiveBoundaryEntry,
    boundary: BoundaryDimension,
    outcome: &BoundaryAttemptOutcome,
) -> EffectiveBoundaryEntry {
    entry = entry
        .with_diagnostic(
            "supervisor_backend",
            diagnostic_or_unknown(&outcome.supervisor_backend),
        )
        .with_diagnostic(
            "fd_cleanup_status",
            diagnostic_or_unknown(&outcome.fd_cleanup_status),
        )
        .with_diagnostic("env_policy", diagnostic_or_unknown(&outcome.env_policy))
        .with_diagnostic(
            "boundary_setup_failed_closed",
            outcome.failed_closed.to_string(),
        );

    for (key, value) in [
        ("privilege_strategy", &outcome.privilege_strategy),
        ("userns_status", &outcome.userns_status),
        ("setgroups_status", &outcome.setgroups_status),
        ("uid_map_status", &outcome.uid_map_status),
        ("gid_map_status", &outcome.gid_map_status),
        ("read_only_bind_scope", &outcome.read_only_bind_scope),
        ("setup_failure_stage", &outcome.setup_failure_stage),
        ("setup_failure_reason", &outcome.setup_failure_reason),
    ] {
        if let Some(value) = value {
            entry = entry.with_diagnostic(key, value.clone());
        }
    }

    if let Some(path) = &outcome.project_view_path {
        entry = entry.with_diagnostic("project_view_path", path.display().to_string());
    }
    if let Some(path) = &outcome.cwd_view_path {
        entry = entry.with_diagnostic("cwd_view_path", path.display().to_string());
    }
    if let Some(method) = &outcome.fd_cleanup_method {
        entry = entry.with_diagnostic("fd_cleanup_method", method.clone());
    }

    match boundary {
        BoundaryDimension::Network => {
            entry.with_diagnostic("namespace_status", outcome.network_namespace_status.label())
        }
        BoundaryDimension::Filesystem => entry
            .with_diagnostic("namespace_status", outcome.mount_namespace_status.label())
            .with_diagnostic(
                "mount_view_status",
                outcome.minimal_mount_view_status.label(),
            ),
        BoundaryDimension::Landlock => {
            let abi = outcome
                .landlock_abi_version
                .map(|version| version.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            entry = entry
                .with_diagnostic("landlock_status", outcome.landlock_status.label())
                .with_diagnostic("landlock_abi_version", abi);
            if let Some(mask) = outcome.landlock_rights_mask {
                entry = entry.with_diagnostic("landlock_rights_mask", format!("0x{mask:x}"));
            }
            if let Some(status) = &outcome.landlock_ruleset_status {
                entry = entry.with_diagnostic("landlock_ruleset_status", status.clone());
            }
            if let Some(required) = outcome.landlock_required {
                entry = entry.with_diagnostic("landlock_required", required.to_string());
            }
            entry
        }
    }
}

fn diagnostic_or_unknown(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "unknown".to_string())
}

fn primitive_entries_for_boundary(
    boundary: BoundaryDimension,
    host_probe: &HostProbeReport,
) -> Vec<HostProbeEntry> {
    allowed_primitives_for(boundary)
        .iter()
        .map(|primitive| {
            host_probe
                .status_for(*primitive)
                .cloned()
                .unwrap_or_else(|| {
                    HostProbeEntry::new(
                        *primitive,
                        HostProbeStatus::Unknown,
                        "probe_result_missing",
                    )
                })
        })
        .collect()
}

pub fn command_boundary_metadata(
    config: &CommandIsolationRuntimeConfig,
    preflight: &CommandBoundaryPreflight,
) -> Value {
    json!({
        "command_isolation_mode": config.mode.label(),
        "effective_boundary": effective_boundary_metadata_json_with_observation(
            &preflight.effective_boundary,
            false,
            false,
        ),
        "host_probe_summary": host_probe_summary_json(&preflight.host_probe),
        "boundary_setup_failed_closed": preflight.boundary_setup_failed_closed,
        "boundary_preflight_failed_closed": preflight.failed_closed,
        "backend_invoked": false,
        "policy_enforced": true,
    })
}

pub fn effective_boundary_metadata_json_with_observation(
    report: &EffectiveBoundaryReport,
    overlay_applied: bool,
    artifact_manifest_generated: bool,
) -> Value {
    let mut map = Map::new();
    for entry in report.entries() {
        let mut entry = entry.clone();
        entry.observed =
            boundary_observed(entry.boundary, overlay_applied, artifact_manifest_generated);
        map.insert(
            entry.boundary.label().to_string(),
            serde_json::to_value(&entry).unwrap_or_else(|_| json!({})),
        );
    }
    Value::Object(map)
}

fn boundary_observed(
    boundary: BoundaryDimension,
    overlay_applied: bool,
    artifact_manifest_generated: bool,
) -> bool {
    match boundary {
        BoundaryDimension::Filesystem => overlay_applied && artifact_manifest_generated,
        BoundaryDimension::Network | BoundaryDimension::Landlock => false,
    }
}

fn host_probe_summary_json(report: &HostProbeReport) -> Value {
    serde_json::to_value(report).unwrap_or_else(|_| json!({}))
}

pub fn probe_host_capabilities() -> HostProbeReport {
    probe_host_capabilities_with_runner(&LiveProbeRunner::new(), ProbePlatform::current())
}

fn probe_host_capabilities_with_runner(
    runner: &impl PrimitiveProbeRunner,
    platform: ProbePlatform,
) -> HostProbeReport {
    if let ProbePlatform::Unsupported(reason) = platform {
        return HostProbeReport::from_entries(BoundaryPrimitive::ALL.iter().map(|primitive| {
            HostProbeEntry::new(*primitive, HostProbeStatus::Unavailable, reason)
        }));
    }

    HostProbeReport::from_entries(BoundaryPrimitive::ALL.iter().map(|primitive| {
        runner.probe(*primitive).unwrap_or_else(|| {
            HostProbeEntry::new(
                *primitive,
                HostProbeStatus::Unknown,
                "probe_runner_missing_result",
            )
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn report(entries: &[(BoundaryPrimitive, HostProbeStatus, &str)]) -> HostProbeReport {
        HostProbeReport::from_entries(
            entries.iter().map(|(primitive, status, reason)| {
                HostProbeEntry::new(*primitive, *status, *reason)
            }),
        )
    }

    #[derive(Default)]
    struct FakeProbeRunner {
        outcomes: BTreeMap<BoundaryPrimitive, ProbeOutcome>,
    }

    impl FakeProbeRunner {
        fn with(
            mut self,
            primitive: BoundaryPrimitive,
            status: HostProbeStatus,
            reason: &'static str,
        ) -> Self {
            self.outcomes
                .insert(primitive, ProbeOutcome::new(status, reason));
            self
        }
    }

    impl PrimitiveProbeRunner for FakeProbeRunner {
        fn probe(&self, primitive: BoundaryPrimitive) -> Option<HostProbeEntry> {
            self.outcomes
                .get(&primitive)
                .map(|outcome| entry_from_outcome(primitive, outcome.clone()))
        }
    }

    #[test]
    fn fake_runner_status_mapping_reports_every_primitive() {
        let report = probe_host_capabilities_with_runner(
            &FakeProbeRunner::default()
                .with(
                    BoundaryPrimitive::Overlayfs,
                    HostProbeStatus::Available,
                    "overlay ok",
                )
                .with(
                    BoundaryPrimitive::NetworkNamespace,
                    HostProbeStatus::Unavailable,
                    "netns unavailable",
                )
                .with(
                    BoundaryPrimitive::MountNamespace,
                    HostProbeStatus::Disabled,
                    "mount ns disabled",
                )
                .with(
                    BoundaryPrimitive::Landlock,
                    HostProbeStatus::Unknown,
                    "landlock unknown",
                ),
            ProbePlatform::Linux,
        );

        assert_eq!(report.entries().count(), BoundaryPrimitive::ALL.len());
        assert_eq!(
            report
                .status_for(BoundaryPrimitive::Overlayfs)
                .expect("overlayfs")
                .status,
            HostProbeStatus::Available
        );
        assert_eq!(
            report
                .status_for(BoundaryPrimitive::NetworkNamespace)
                .expect("netns")
                .status,
            HostProbeStatus::Unavailable
        );
        assert_eq!(
            report
                .status_for(BoundaryPrimitive::MountNamespace)
                .expect("mount ns")
                .status,
            HostProbeStatus::Disabled
        );
        assert_eq!(
            report
                .status_for(BoundaryPrimitive::Landlock)
                .expect("landlock")
                .status,
            HostProbeStatus::Unknown
        );
    }

    #[test]
    fn missing_fake_runner_entry_becomes_unknown_probe_result() {
        let report = probe_host_capabilities_with_runner(
            &FakeProbeRunner::default().with(
                BoundaryPrimitive::NetworkNamespace,
                HostProbeStatus::Available,
                "netns ok",
            ),
            ProbePlatform::Linux,
        );

        let overlay = report
            .status_for(BoundaryPrimitive::Overlayfs)
            .expect("missing result filled");
        assert_eq!(overlay.status, HostProbeStatus::Unknown);
        assert_eq!(
            overlay.reason.as_deref(),
            Some("probe_runner_missing_result")
        );
    }

    #[test]
    fn non_linux_probe_report_marks_all_primitives_unavailable() {
        let report = probe_host_capabilities_with_runner(
            &FakeProbeRunner::default().with(
                BoundaryPrimitive::NetworkNamespace,
                HostProbeStatus::Available,
                "should be ignored",
            ),
            ProbePlatform::Unsupported("unsupported_platform"),
        );

        assert_eq!(report.entries().count(), BoundaryPrimitive::ALL.len());
        for primitive in BoundaryPrimitive::ALL {
            let entry = report.status_for(*primitive).expect("primitive entry");
            assert_eq!(entry.status, HostProbeStatus::Unavailable);
            assert_eq!(entry.reason.as_deref(), Some("unsupported_platform"));
        }
    }

    struct RecordingChildProbe {
        calls: RefCell<Vec<ChildProbeKind>>,
    }

    impl RecordingChildProbe {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl ChildProbeExecutor for RecordingChildProbe {
        fn run_child_probe(&self, kind: ChildProbeKind) -> ProbeOutcome {
            self.calls.borrow_mut().push(kind);
            ProbeOutcome::new(HostProbeStatus::Unavailable, "child path exercised")
        }
    }

    #[test]
    fn namespace_and_overlay_probes_use_child_process_seam_without_parent_ns_change() {
        let net_before = namespace_handle("net");
        let mnt_before = namespace_handle("mnt");
        let child = RecordingChildProbe::new();
        let runner = LiveProbeRunner::with_child_executor(&child);

        for primitive in [
            BoundaryPrimitive::NetworkNamespace,
            BoundaryPrimitive::MountNamespace,
            BoundaryPrimitive::Overlayfs,
        ] {
            let entry = runner.probe(primitive).expect("probe entry");
            assert_eq!(entry.status, HostProbeStatus::Unavailable);
        }

        assert_eq!(
            child.calls.borrow().as_slice(),
            &[
                ChildProbeKind::NetworkNamespace,
                ChildProbeKind::MountNamespace,
                ChildProbeKind::Overlayfs
            ]
        );
        assert_eq!(namespace_handle("net"), net_before);
        assert_eq!(namespace_handle("mnt"), mnt_before);
    }

    #[test]
    fn command_probe_uses_trusted_absolute_helper_paths() {
        let unshare = trusted_helper(TRUSTED_UNSHARE_PATHS);
        let true_path = trusted_helper(TRUSTED_TRUE_PATHS);
        if let (Some(unshare), Some(true_path)) = (unshare, true_path) {
            let outcome = CommandChildProbe.run_child_probe(ChildProbeKind::NetworkNamespace);
            assert_eq!(
                outcome
                    .diagnostics
                    .get("helper.unshare")
                    .map(String::as_str),
                Some(unshare)
            );
            if outcome.status == HostProbeStatus::Available {
                assert_eq!(
                    outcome.diagnostics.get("helper.true").map(String::as_str),
                    Some(true_path)
                );
            } else {
                assert!(outcome.reason.contains("network_namespace"));
            }
        } else {
            let outcome = CommandChildProbe.run_child_probe(ChildProbeKind::NetworkNamespace);
            assert_eq!(outcome.status, HostProbeStatus::Unknown);
            assert!(outcome.reason.contains("trusted probe helper"));
        }
    }

    fn namespace_handle(name: &str) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/self/ns/{name}")).ok()
    }

    #[test]
    fn network_required_boundary_requires_netns_availability() {
        let probe = report(&[
            (
                BoundaryPrimitive::NetworkNamespace,
                HostProbeStatus::Unavailable,
                "missing CAP_SYS_ADMIN",
            ),
            (
                BoundaryPrimitive::Landlock,
                HostProbeStatus::Available,
                "abi v3",
            ),
        ]);
        let missing = unavailable_required_boundaries(&probe, &[BoundaryDimension::Network]);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].boundary, BoundaryDimension::Network);
        assert_eq!(
            missing[0].primitive_reasons[0].primitive,
            BoundaryPrimitive::NetworkNamespace
        );
    }

    #[test]
    fn landlock_availability_does_not_satisfy_network_gate() {
        let probe = report(&[(
            BoundaryPrimitive::Landlock,
            HostProbeStatus::Available,
            "abi v3",
        )]);
        let missing = unavailable_required_boundaries(&probe, &[BoundaryDimension::Network]);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].boundary, BoundaryDimension::Network);
    }

    fn enforce_config(
        required_boundaries: Vec<BoundaryDimension>,
        host_probe: HostProbeReport,
    ) -> CommandIsolationRuntimeConfig {
        CommandIsolationRuntimeConfig {
            mode: CommandIsolationMode::Enforce,
            required_boundaries,
            host_probe: Some(host_probe),
            ..CommandIsolationRuntimeConfig::default()
        }
    }

    fn audit_config(
        optional_boundaries: Vec<BoundaryDimension>,
        host_probe: HostProbeReport,
    ) -> CommandIsolationRuntimeConfig {
        CommandIsolationRuntimeConfig {
            mode: CommandIsolationMode::Audit,
            optional_boundaries,
            host_probe: Some(host_probe),
            ..CommandIsolationRuntimeConfig::default()
        }
    }

    fn fake_attempt(evidence: Vec<AppliedBoundaryEvidence>) -> CommandIsolationAttempt {
        CommandIsolationAttempt::new(BoundaryAttemptOutcome {
            evidence,
            supervisor_backend: Some("fake-supervisor".to_string()),
            project_view_path: Some(PathBuf::from("/project")),
            cwd_view_path: Some(PathBuf::from("/project/crates")),
            fd_cleanup_status: Some("applied".to_string()),
            fd_cleanup_method: Some("close_range".to_string()),
            env_policy: Some("allowlist".to_string()),
            ..BoundaryAttemptOutcome::default()
        })
    }

    #[test]
    fn command_boundary_host_probe_available_without_attempt_is_not_physically_bounded() {
        let config = enforce_config(
            vec![BoundaryDimension::Network],
            report(&[(
                BoundaryPrimitive::NetworkNamespace,
                HostProbeStatus::Available,
                "netns probe ok",
            )]),
        );

        let preflight = evaluate_command_boundary_preflight(&config);
        let network = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Network)
            .expect("network boundary");

        assert_eq!(network.status, EffectiveBoundaryStatus::FailedClosed);
        assert!(network.source_primitives.is_empty());
        assert!(preflight.should_fail_closed());
    }

    #[test]
    fn command_boundary_fake_netns_attempt_yields_network_physical_credit() {
        let config = enforce_config(
            vec![BoundaryDimension::Network],
            report(&[(
                BoundaryPrimitive::NetworkNamespace,
                HostProbeStatus::Unavailable,
                "host probe does not matter without applied evidence",
            )]),
        );
        let mut attempt = fake_attempt(vec![AppliedBoundaryEvidence::NetworkNamespace(
            NetnsApplied::from_supervisor_netns_success(),
        )]);
        attempt.outcome.network_namespace_status = SupervisorSetupStatus::Applied;

        let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
        let network = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Network)
            .expect("network boundary");

        assert_eq!(network.status, EffectiveBoundaryStatus::PhysicallyBounded);
        assert_eq!(
            network.source_primitives,
            vec![BoundaryPrimitive::NetworkNamespace]
        );
        assert_eq!(
            network
                .diagnostics
                .get("namespace_status")
                .map(String::as_str),
            Some("applied")
        );
        assert!(!preflight.should_fail_closed());
    }

    #[test]
    fn command_boundary_fake_mount_view_requires_mount_namespace_and_view_applied() {
        let config = enforce_config(
            vec![BoundaryDimension::Filesystem],
            report(&[(
                BoundaryPrimitive::MountNamespace,
                HostProbeStatus::Available,
                "mount ns probe ok",
            )]),
        );
        let mut attempt = fake_attempt(vec![
            AppliedBoundaryEvidence::MountNamespace(
                MountNamespaceApplied::from_supervisor_mount_namespace_success(),
            ),
            AppliedBoundaryEvidence::MinimalMountView(
                MinimalMountViewApplied::from_supervisor_minimal_mount_view_success(),
            ),
        ]);
        attempt.outcome.mount_namespace_status = SupervisorSetupStatus::Applied;
        attempt.outcome.minimal_mount_view_status = MinimalMountViewStatus::Applied;

        let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
        let filesystem = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Filesystem)
            .expect("filesystem boundary");

        assert_eq!(
            filesystem.status,
            EffectiveBoundaryStatus::PhysicallyBounded
        );
        assert_eq!(
            filesystem.source_primitives,
            vec![BoundaryPrimitive::MountNamespace]
        );
        assert_eq!(
            filesystem
                .diagnostics
                .get("mount_view_status")
                .map(String::as_str),
            Some("applied")
        );
        assert_eq!(
            filesystem.diagnostics.get("env_policy").map(String::as_str),
            Some("allowlist")
        );
    }

    #[test]
    fn command_boundary_metadata_preserves_compact_env_fd_hygiene_diagnostics() {
        let config = enforce_config(
            vec![BoundaryDimension::Filesystem],
            report(&[(
                BoundaryPrimitive::MountNamespace,
                HostProbeStatus::Available,
                "mount ns probe ok",
            )]),
        );
        let mut attempt = fake_attempt(vec![
            AppliedBoundaryEvidence::MountNamespace(
                MountNamespaceApplied::from_supervisor_mount_namespace_success(),
            ),
            AppliedBoundaryEvidence::MinimalMountView(
                MinimalMountViewApplied::from_supervisor_minimal_mount_view_success(),
            ),
        ]);
        attempt.outcome.mount_namespace_status = SupervisorSetupStatus::Applied;
        attempt.outcome.minimal_mount_view_status = MinimalMountViewStatus::Applied;
        attempt.outcome.fd_cleanup_method = Some("rlimit_loop".to_string());

        let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
        let metadata = command_boundary_metadata(&config, &preflight);
        let diagnostics = &metadata["effective_boundary"]["filesystem"]["diagnostics"];

        assert_eq!(diagnostics["env_policy"], "allowlist");
        assert_eq!(diagnostics["fd_cleanup_status"], "applied");
        assert_eq!(diagnostics["fd_cleanup_method"], "rlimit_loop");
        assert!(metadata.to_string().len() < 4096);
        assert!(!metadata.to_string().contains("OPENAI_API_KEY"));
        assert!(!metadata.to_string().contains("AWS_ACCESS_KEY_ID"));
    }

    #[test]
    fn command_boundary_metadata_observed_is_centralized_and_conservative() {
        let config = enforce_config(
            vec![
                BoundaryDimension::Network,
                BoundaryDimension::Filesystem,
                BoundaryDimension::Landlock,
            ],
            report(&[
                (
                    BoundaryPrimitive::NetworkNamespace,
                    HostProbeStatus::Available,
                    "netns probe ok",
                ),
                (
                    BoundaryPrimitive::MountNamespace,
                    HostProbeStatus::Available,
                    "mount ns probe ok",
                ),
                (
                    BoundaryPrimitive::Landlock,
                    HostProbeStatus::Available,
                    "landlock probe ok",
                ),
            ]),
        );
        let mut attempt = fake_attempt(vec![
            AppliedBoundaryEvidence::NetworkNamespace(NetnsApplied::from_supervisor_netns_success()),
            AppliedBoundaryEvidence::MountNamespace(
                MountNamespaceApplied::from_supervisor_mount_namespace_success(),
            ),
            AppliedBoundaryEvidence::MinimalMountView(
                MinimalMountViewApplied::from_supervisor_minimal_mount_view_success(),
            ),
            AppliedBoundaryEvidence::Landlock(LandlockApplied::from_supervisor_landlock_success()),
        ]);
        attempt.outcome.network_namespace_status = SupervisorSetupStatus::Applied;
        attempt.outcome.mount_namespace_status = SupervisorSetupStatus::Applied;
        attempt.outcome.minimal_mount_view_status = MinimalMountViewStatus::Applied;
        attempt.outcome.landlock_status = LandlockApplicationStatus::Applied;
        attempt.outcome.landlock_abi_version = Some(5);

        let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
        let preflight_metadata = command_boundary_metadata(&config, &preflight);
        for boundary in ["network", "filesystem", "landlock"] {
            assert_eq!(
                preflight_metadata["effective_boundary"][boundary]["observed"],
                false
            );
        }
        assert_eq!(
            preflight_metadata["effective_boundary"]["network"]["status"],
            "physically_bounded"
        );
        assert_eq!(
            preflight_metadata["effective_boundary"]["landlock"]["status"],
            "physically_bounded"
        );

        let no_manifest = effective_boundary_metadata_json_with_observation(
            &preflight.effective_boundary,
            true,
            false,
        );
        assert_eq!(no_manifest["filesystem"]["observed"], false);

        let artifact_metadata = effective_boundary_metadata_json_with_observation(
            &preflight.effective_boundary,
            true,
            true,
        );
        assert_eq!(artifact_metadata["filesystem"]["observed"], true);
        assert_eq!(artifact_metadata["network"]["observed"], false);
        assert_eq!(artifact_metadata["landlock"]["observed"], false);
        assert_eq!(artifact_metadata["network"]["status"], "physically_bounded");
        assert_eq!(
            artifact_metadata["landlock"]["status"],
            "physically_bounded"
        );
    }

    #[test]
    fn command_boundary_mount_namespace_without_mount_view_is_failed_not_physical() {
        let config = enforce_config(
            vec![BoundaryDimension::Filesystem],
            report(&[(
                BoundaryPrimitive::MountNamespace,
                HostProbeStatus::Available,
                "mount ns probe ok",
            )]),
        );
        let mut attempt = fake_attempt(vec![AppliedBoundaryEvidence::MountNamespace(
            MountNamespaceApplied::from_supervisor_mount_namespace_success(),
        )]);
        attempt.outcome.mount_namespace_status = SupervisorSetupStatus::Applied;
        attempt.outcome.minimal_mount_view_status = MinimalMountViewStatus::FailedClosed;
        attempt.outcome.failed_closed = true;

        let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
        let filesystem = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Filesystem)
            .expect("filesystem boundary");

        assert_eq!(filesystem.status, EffectiveBoundaryStatus::FailedClosed);
        assert!(filesystem.source_primitives.is_empty());
        assert_eq!(
            filesystem
                .diagnostics
                .get("mount_view_status")
                .map(String::as_str),
            Some("failed_closed")
        );
        assert!(preflight.should_fail_closed());
    }

    #[test]
    fn command_boundary_landlock_success_does_not_backfill_failed_mount_view() {
        let config = enforce_config(
            vec![BoundaryDimension::Filesystem],
            report(&[
                (
                    BoundaryPrimitive::MountNamespace,
                    HostProbeStatus::Available,
                    "mount ns probe ok",
                ),
                (
                    BoundaryPrimitive::Landlock,
                    HostProbeStatus::Available,
                    "landlock probe ok",
                ),
            ]),
        );
        let mut attempt = fake_attempt(vec![
            AppliedBoundaryEvidence::MountNamespace(
                MountNamespaceApplied::from_supervisor_mount_namespace_success(),
            ),
            AppliedBoundaryEvidence::Landlock(LandlockApplied::from_supervisor_landlock_success()),
        ]);
        attempt.outcome.mount_namespace_status = SupervisorSetupStatus::Applied;
        attempt.outcome.minimal_mount_view_status = MinimalMountViewStatus::FailedClosed;
        attempt.outcome.landlock_status = LandlockApplicationStatus::Applied;
        attempt.outcome.landlock_abi_version = Some(3);
        attempt.outcome.failed_closed = true;

        let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
        let filesystem = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Filesystem)
            .expect("filesystem boundary");

        assert_eq!(filesystem.status, EffectiveBoundaryStatus::FailedClosed);
        assert!(filesystem.source_primitives.is_empty());
        assert_eq!(
            filesystem
                .diagnostics
                .get("mount_view_status")
                .map(String::as_str),
            Some("failed_closed")
        );
    }

    #[test]
    fn command_boundary_landlock_evidence_never_backfills_network_credit() {
        let config = enforce_config(
            vec![BoundaryDimension::Network],
            report(&[
                (
                    BoundaryPrimitive::NetworkNamespace,
                    HostProbeStatus::Unavailable,
                    "netns setup unavailable",
                ),
                (
                    BoundaryPrimitive::Landlock,
                    HostProbeStatus::Available,
                    "landlock probe ok",
                ),
            ]),
        );
        let mut attempt = fake_attempt(vec![AppliedBoundaryEvidence::Landlock(
            LandlockApplied::from_supervisor_landlock_success(),
        )]);
        attempt.outcome.landlock_status = LandlockApplicationStatus::Applied;
        attempt.outcome.landlock_abi_version = Some(3);

        let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
        let network = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Network)
            .expect("network boundary");

        assert_eq!(network.status, EffectiveBoundaryStatus::FailedClosed);
        assert!(network.source_primitives.is_empty());
        assert!(preflight.should_fail_closed());
    }

    #[test]
    fn command_boundary_host_inherited_env_cannot_satisfy_required_physical_credit() {
        let config = enforce_config(
            vec![BoundaryDimension::Network],
            report(&[(
                BoundaryPrimitive::NetworkNamespace,
                HostProbeStatus::Available,
                "netns probe ok",
            )]),
        );
        let mut attempt = fake_attempt(vec![AppliedBoundaryEvidence::NetworkNamespace(
            NetnsApplied::from_supervisor_netns_success(),
        )]);
        attempt.outcome.network_namespace_status = SupervisorSetupStatus::Applied;
        attempt.outcome.env_policy = Some("host_inherited".to_string());

        let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
        let network = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Network)
            .expect("network boundary");

        assert_eq!(network.status, EffectiveBoundaryStatus::FailedClosed);
        assert!(network.source_primitives.is_empty());
        assert_eq!(
            network.diagnostics.get("env_policy").map(String::as_str),
            Some("host_inherited")
        );
        assert!(preflight.should_fail_closed());
    }

    #[test]
    fn command_boundary_failed_fd_cleanup_cannot_satisfy_required_physical_credit() {
        let config = enforce_config(
            vec![BoundaryDimension::Filesystem],
            report(&[(
                BoundaryPrimitive::MountNamespace,
                HostProbeStatus::Available,
                "mount ns probe ok",
            )]),
        );
        let mut attempt = fake_attempt(vec![
            AppliedBoundaryEvidence::MountNamespace(
                MountNamespaceApplied::from_supervisor_mount_namespace_success(),
            ),
            AppliedBoundaryEvidence::MinimalMountView(
                MinimalMountViewApplied::from_supervisor_minimal_mount_view_success(),
            ),
        ]);
        attempt.outcome.mount_namespace_status = SupervisorSetupStatus::Applied;
        attempt.outcome.minimal_mount_view_status = MinimalMountViewStatus::Applied;
        attempt.outcome.fd_cleanup_status = Some("failed".to_string());
        attempt.outcome.failed_closed = true;

        let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
        let filesystem = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Filesystem)
            .expect("filesystem boundary");

        assert_eq!(filesystem.status, EffectiveBoundaryStatus::FailedClosed);
        assert!(filesystem.source_primitives.is_empty());
        assert_eq!(
            filesystem
                .diagnostics
                .get("fd_cleanup_status")
                .map(String::as_str),
            Some("failed")
        );
        assert!(preflight.should_fail_closed());
    }

    #[test]
    fn command_boundary_landlock_diagnostics_use_stable_status_labels() {
        for (status, expected) in [
            (LandlockApplicationStatus::Unsupported, "unsupported"),
            (LandlockApplicationStatus::NotApplied, "not_applied"),
            (LandlockApplicationStatus::Applied, "applied"),
        ] {
            let config = audit_config(
                vec![BoundaryDimension::Landlock],
                report(&[(
                    BoundaryPrimitive::Landlock,
                    HostProbeStatus::Available,
                    "abi ok",
                )]),
            );
            let mut attempt = fake_attempt(Vec::new());
            attempt.outcome.landlock_status = status;
            attempt.outcome.landlock_abi_version = Some(3);
            attempt.outcome.landlock_rights_mask = Some(0x7fff);
            attempt.outcome.landlock_ruleset_status = Some(expected.to_string());
            attempt.outcome.landlock_required = Some(false);
            if status == LandlockApplicationStatus::Applied {
                attempt
                    .outcome
                    .evidence
                    .push(AppliedBoundaryEvidence::Landlock(
                        LandlockApplied::from_supervisor_landlock_success(),
                    ));
            }

            let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
            let landlock = preflight
                .effective_boundary
                .status_for(BoundaryDimension::Landlock)
                .expect("landlock boundary");

            assert_eq!(
                landlock
                    .diagnostics
                    .get("landlock_status")
                    .map(String::as_str),
                Some(expected)
            );
            assert_eq!(
                landlock
                    .diagnostics
                    .get("landlock_abi_version")
                    .map(String::as_str),
                Some("3")
            );
            assert_eq!(
                landlock
                    .diagnostics
                    .get("landlock_rights_mask")
                    .map(String::as_str),
                Some("0x7fff")
            );
            assert_eq!(
                landlock
                    .diagnostics
                    .get("landlock_ruleset_status")
                    .map(String::as_str),
                Some(expected)
            );
            assert_eq!(
                landlock
                    .diagnostics
                    .get("landlock_required")
                    .map(String::as_str),
                Some("false")
            );
            if status == LandlockApplicationStatus::Applied {
                assert_eq!(landlock.status, EffectiveBoundaryStatus::PhysicallyBounded);
            }
        }
    }

    #[test]
    fn command_boundary_landlock_required_optional_matrix_is_mode_specific() {
        let probe = report(&[(
            BoundaryPrimitive::Landlock,
            HostProbeStatus::Available,
            "abi ok",
        )]);

        let mut unsupported = fake_attempt(Vec::new());
        unsupported.outcome.landlock_status = LandlockApplicationStatus::Unsupported;
        unsupported.outcome.landlock_required = Some(true);

        let mut not_applied = fake_attempt(Vec::new());
        not_applied.outcome.landlock_status = LandlockApplicationStatus::NotApplied;
        not_applied.outcome.landlock_abi_version = Some(3);
        not_applied.outcome.landlock_required = Some(false);

        let enforce_required = evaluate_command_boundary_attempt(
            &enforce_config(vec![BoundaryDimension::Landlock], probe.clone()),
            Some(&unsupported),
        );
        let enforce_optional = evaluate_command_boundary_attempt(
            &CommandIsolationRuntimeConfig {
                mode: CommandIsolationMode::Enforce,
                optional_boundaries: vec![BoundaryDimension::Landlock],
                host_probe: Some(probe.clone()),
                ..CommandIsolationRuntimeConfig::default()
            },
            Some(&not_applied),
        );
        let audit_required = evaluate_command_boundary_attempt(
            &CommandIsolationRuntimeConfig {
                mode: CommandIsolationMode::Audit,
                required_boundaries: vec![BoundaryDimension::Landlock],
                host_probe: Some(probe.clone()),
                ..CommandIsolationRuntimeConfig::default()
            },
            Some(&unsupported),
        );
        let audit_optional = evaluate_command_boundary_attempt(
            &audit_config(vec![BoundaryDimension::Landlock], probe),
            Some(&not_applied),
        );

        assert_eq!(
            enforce_required
                .effective_boundary
                .status_for(BoundaryDimension::Landlock)
                .unwrap()
                .status,
            EffectiveBoundaryStatus::Unsupported
        );
        assert!(enforce_required.should_fail_closed());
        assert_eq!(
            enforce_optional
                .effective_boundary
                .status_for(BoundaryDimension::Landlock)
                .unwrap()
                .status,
            EffectiveBoundaryStatus::Unbounded
        );
        assert!(!enforce_optional.should_fail_closed());
        assert_eq!(
            audit_required
                .effective_boundary
                .status_for(BoundaryDimension::Landlock)
                .unwrap()
                .status,
            EffectiveBoundaryStatus::Unsupported
        );
        assert!(!audit_required.should_fail_closed());
        assert_eq!(
            audit_optional
                .effective_boundary
                .status_for(BoundaryDimension::Landlock)
                .unwrap()
                .status,
            EffectiveBoundaryStatus::Unbounded
        );
        assert!(!audit_optional.should_fail_closed());
    }

    #[test]
    fn command_boundary_metadata_preserves_landlock_matrix_statuses() {
        let probe = report(&[(
            BoundaryPrimitive::Landlock,
            HostProbeStatus::Available,
            "abi ok",
        )]);
        let cases = [
            (
                CommandIsolationRuntimeConfig {
                    mode: CommandIsolationMode::Enforce,
                    required_boundaries: vec![BoundaryDimension::Landlock],
                    host_probe: Some(probe.clone()),
                    ..CommandIsolationRuntimeConfig::default()
                },
                LandlockApplicationStatus::Unsupported,
                "unsupported",
                true,
            ),
            (
                CommandIsolationRuntimeConfig {
                    mode: CommandIsolationMode::Enforce,
                    optional_boundaries: vec![BoundaryDimension::Landlock],
                    host_probe: Some(probe.clone()),
                    ..CommandIsolationRuntimeConfig::default()
                },
                LandlockApplicationStatus::NotApplied,
                "not_applied",
                false,
            ),
            (
                CommandIsolationRuntimeConfig {
                    mode: CommandIsolationMode::Audit,
                    required_boundaries: vec![BoundaryDimension::Landlock],
                    host_probe: Some(probe.clone()),
                    ..CommandIsolationRuntimeConfig::default()
                },
                LandlockApplicationStatus::NotApplied,
                "not_applied",
                true,
            ),
            (
                CommandIsolationRuntimeConfig {
                    mode: CommandIsolationMode::Audit,
                    optional_boundaries: vec![BoundaryDimension::Landlock],
                    host_probe: Some(probe),
                    ..CommandIsolationRuntimeConfig::default()
                },
                LandlockApplicationStatus::Unsupported,
                "unsupported",
                false,
            ),
        ];

        for (config, status, expected_status, required) in cases {
            let mut attempt = fake_attempt(Vec::new());
            attempt.outcome.landlock_status = status;
            attempt.outcome.landlock_abi_version = Some(5);
            attempt.outcome.landlock_rights_mask = Some(0xffff);
            attempt.outcome.landlock_ruleset_status = Some(expected_status.to_string());
            attempt.outcome.landlock_required = Some(required);

            let preflight = evaluate_command_boundary_attempt(&config, Some(&attempt));
            let metadata = command_boundary_metadata(&config, &preflight);
            let diagnostics = &metadata["effective_boundary"]["landlock"]["diagnostics"];
            assert_eq!(diagnostics["landlock_status"], expected_status);
            assert_eq!(diagnostics["landlock_abi_version"], "5");
            assert_eq!(diagnostics["landlock_rights_mask"], "0xffff");
            assert_eq!(diagnostics["landlock_required"], required.to_string());
        }
    }

    #[test]
    fn command_boundary_enforce_fails_closed_but_audit_reports_unbounded_without_evidence() {
        let probe = report(&[(
            BoundaryPrimitive::NetworkNamespace,
            HostProbeStatus::Available,
            "netns probe ok",
        )]);
        let attempt = fake_attempt(Vec::new());

        let enforce = evaluate_command_boundary_attempt(
            &enforce_config(vec![BoundaryDimension::Network], probe.clone()),
            Some(&attempt),
        );
        let audit = evaluate_command_boundary_attempt(
            &audit_config(vec![BoundaryDimension::Network], probe),
            Some(&attempt),
        );

        assert_eq!(
            enforce
                .effective_boundary
                .status_for(BoundaryDimension::Network)
                .expect("enforce network")
                .status,
            EffectiveBoundaryStatus::FailedClosed
        );
        assert!(enforce.should_fail_closed());
        assert_eq!(
            audit
                .effective_boundary
                .status_for(BoundaryDimension::Network)
                .expect("audit network")
                .status,
            EffectiveBoundaryStatus::Unbounded
        );
        assert!(!audit.should_fail_closed());
    }

    #[test]
    fn live_probe_reports_every_primitive_with_reason() {
        let probe = probe_host_capabilities();
        assert_eq!(probe.entries().count(), BoundaryPrimitive::ALL.len());
        for primitive in BoundaryPrimitive::ALL {
            let entry = probe.status_for(*primitive).expect("primitive entry");
            assert!(matches!(
                entry.status,
                HostProbeStatus::Available
                    | HostProbeStatus::Unavailable
                    | HostProbeStatus::Disabled
                    | HostProbeStatus::Unknown
            ));
            assert!(entry
                .reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty()));
        }
    }
}
