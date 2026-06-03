//! Host capability probes and boundary gate helpers.

use lancet_authority::allowed_primitives_for;
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
    args: Vec<&'static str>,
    diagnostics: BTreeMap<String, String>,
}

impl CommandAttempt {
    fn new(program: &'static str, args: Vec<&'static str>) -> Self {
        Self {
            program,
            args,
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
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi > 0 {
        return ProbeOutcome::new(HostProbeStatus::Available, format!("landlock ABI v{abi}"));
    }

    let err = io::Error::last_os_error();
    let raw = err.raw_os_error();
    let status = match raw {
        Some(code) if code == libc::ENOSYS => HostProbeStatus::Disabled,
        Some(code) if code == libc::EOPNOTSUPP => HostProbeStatus::Disabled,
        Some(code) if code == libc::EINVAL => HostProbeStatus::Unknown,
        _ => HostProbeStatus::Unknown,
    };
    ProbeOutcome::new(status, format!("landlock ABI probe failed: {err}"))
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
    let host_probe = config
        .host_probe
        .clone()
        .unwrap_or_else(probe_host_capabilities);

    let entries: Vec<_> = BoundaryDimension::ALL
        .iter()
        .map(|boundary| effective_boundary_entry(config, &host_probe, *boundary))
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
        failed_closed: !fail_reasons.is_empty(),
        fail_reasons,
    }
}

fn effective_boundary_entry(
    config: &CommandIsolationRuntimeConfig,
    host_probe: &HostProbeReport,
    boundary: BoundaryDimension,
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
        "effective_boundary": effective_boundary_metadata_json(&preflight.effective_boundary),
        "host_probe_summary": host_probe_summary_json(&preflight.host_probe),
        "boundary_preflight_failed_closed": preflight.failed_closed,
        "backend_invoked": false,
        "policy_enforced": true,
    })
}

fn effective_boundary_metadata_json(report: &EffectiveBoundaryReport) -> Value {
    let mut map = Map::new();
    for entry in report.entries() {
        map.insert(
            entry.boundary.label().to_string(),
            serde_json::to_value(entry).unwrap_or_else(|_| json!({})),
        );
    }
    Value::Object(map)
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
            assert_eq!(
                outcome.diagnostics.get("helper.true").map(String::as_str),
                Some(true_path)
            );
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
