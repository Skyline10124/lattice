//! Ignored live Linux canaries for the command isolation supervisor.
//!
//! These tests are diagnostics unless they pass on the current host. Unsupported
//! kernel/user-namespace/mount/Landlock behavior is classified explicitly and is
//! never counted as proof of physical bounds.

use std::env;
use std::fs;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use super::fd::{cleanup_inherited_fds, set_cloexec, LiveFdCleanupBackend};
use super::landlock::{LandlockRefinementPlan, LandlockRightsMask};
use super::syscall::{
    assert_proc_net_dev_has_only_loopback, assert_proc_net_route_has_no_default_route,
    LiveSupervisorSyscalls, SupervisorSyscalls,
};
use crate::enforcement::isolation::{
    CommandIsolationLandlockBehavior, CommandIsolationMode, CommandIsolationRuntimeConfig,
    CommandIsolationUnsupportedHostBehavior,
};
use crate::handlers::register_command_methods;
use crate::server::registry::{Cancellation, EventSink, HandlerContext, MethodRegistry};
use lancet_authority::envelope_store::ActiveEnvelopeStore;
use lancet_authority::policy::{CepPolicyConfig, CepTurnContext};
use lancet_catalog::methods::command::{Run, RunParams, RunResult};
use lancet_catalog::CepMethod;
use lancet_protocol::envelope::{
    BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact, ImpactConfidence,
    ImpactKind, Intent, OnFailure, Scope, VerifyCommand,
};
use tokio::sync::{mpsc, watch};

const LIVE_ENV: &str = "LATTICE_LINUX_ISOLATION_LIVE";
const HELPER_ENV: &str = "LATTICE_LIVE_CANARY_HELPER";
const HELPER_NETNS: &str = "netns";
const HELPER_MOUNT_VIEW: &str = "mount_view";
const HELPER_FD_CLEANUP: &str = "fd_cleanup";
const HELPER_FD_FINAL: &str = "fd_final";
const HELPER_LANDLOCK: &str = "landlock";
const PHASE30_VERIFY_SENTINEL: &str = "LATTICE_PHASE30_VERIFY_SENTINEL";

#[derive(Clone, Debug, PartialEq, Eq)]
enum CanaryResult {
    Passed(String),
    SkippedUnsupported(String),
    Failed(String),
}

impl CanaryResult {
    fn status(&self) -> &'static str {
        match self {
            CanaryResult::Passed(_) => "passed",
            CanaryResult::SkippedUnsupported(_) => "skipped_unsupported",
            CanaryResult::Failed(_) => "failed",
        }
    }

    fn detail(&self) -> &str {
        match self {
            CanaryResult::Passed(detail)
            | CanaryResult::SkippedUnsupported(detail)
            | CanaryResult::Failed(detail) => detail,
        }
    }
}

#[test]
#[ignore]
fn linux_isolation_live_canary_suite() {
    if env::var(LIVE_ENV).as_deref() != Ok("1") {
        print_live_summary(&[(
            "live_canary_gate",
            CanaryResult::SkippedUnsupported(format!("{LIVE_ENV} is not set to 1")),
        )]);
        return;
    }

    let results = vec![
        ("no_egress_netns", canary_no_egress_netns()),
        ("netns_topology", canary_netns_topology()),
        ("minimal_mount_view", canary_minimal_mount_view()),
        (
            "fd_and_setup_pipe_cleanup",
            canary_fd_and_setup_pipe_cleanup(),
        ),
        ("landlock_status", canary_landlock_status()),
        (
            "diagnostic_overlay_discard",
            canary_diagnostic_overlay_discard(),
        ),
        ("artifact_overlay_commit", canary_artifact_overlay_commit()),
        (
            "artifact_unsupported_kind_denial",
            canary_artifact_unsupported_kind_denial(),
        ),
        (
            "artifact_lower_drift_denial",
            canary_artifact_lower_drift_denial(),
        ),
        ("artifact_verify_discard", canary_artifact_verify_discard()),
    ];
    print_live_summary(&results);

    let failed = results
        .iter()
        .filter(|(_, result)| matches!(result, CanaryResult::Failed(_)))
        .map(|(name, result)| format!("{name}: {}", result.detail()))
        .collect::<Vec<_>>();
    assert!(
        failed.is_empty(),
        "live isolation canary failure(s): {}",
        failed.join("; ")
    );
}

fn print_live_summary(results: &[(&str, CanaryResult)]) {
    let mut passed = 0usize;
    let mut skipped_unsupported = 0usize;
    let mut failed = 0usize;

    for (name, result) in results {
        match result {
            CanaryResult::Passed(_) => passed += 1,
            CanaryResult::SkippedUnsupported(_) => skipped_unsupported += 1,
            CanaryResult::Failed(_) => failed += 1,
        }
        println!(
            "live_canary {name}: {} — {}",
            result.status(),
            result.detail()
        );
    }

    println!(
        "live_canary_summary: passed={passed} skipped_unsupported={skipped_unsupported} failed={failed}"
    );
}

fn canary_no_egress_netns() -> CanaryResult {
    run_netns_helper()
        .map(|_| {
            CanaryResult::Passed(
                "parent loopback TCP listener was unreachable from fresh netns".to_string(),
            )
        })
        .unwrap_or_else(|result| result)
}

fn canary_netns_topology() -> CanaryResult {
    run_netns_helper()
        .map(|_| {
            CanaryResult::Passed(
                "child netns had no veth device, no default route, and did not join host netns"
                    .to_string(),
            )
        })
        .unwrap_or_else(|result| result)
}

fn run_netns_helper() -> Result<(), CanaryResult> {
    let Some(unshare) = find_executable(&["/usr/bin/unshare", "/bin/unshare"]) else {
        return Err(CanaryResult::SkippedUnsupported(
            "network namespace unavailable: unshare helper not found".to_string(),
        ));
    };
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| {
        CanaryResult::SkippedUnsupported(format!(
            "network namespace unavailable: parent loopback listener failed: {err}"
        ))
    })?;
    let port = listener.local_addr().map_err(|err| {
        CanaryResult::SkippedUnsupported(format!(
            "network namespace unavailable: parent listener address failed: {err}"
        ))
    })?;
    let parent_netns = fs::read_link("/proc/self/ns/net")
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let output = helper_command(HELPER_NETNS)
        .env("LATTICE_LIVE_PARENT_PORT", port.port().to_string())
        .env("LATTICE_LIVE_PARENT_NETNS", parent_netns)
        .pipe_through_unshare(&unshare, &["--user", "--map-root-user", "--net"])
        .output();

    classify_helper_output("network namespace unavailable", output)
}

fn canary_minimal_mount_view() -> CanaryResult {
    let Some(unshare) = find_executable(&["/usr/bin/unshare", "/bin/unshare"]) else {
        return CanaryResult::SkippedUnsupported(
            "mount namespace unavailable: unshare helper not found".to_string(),
        );
    };

    let temp = match tempfile::TempDir::new() {
        Ok(temp) => temp,
        Err(err) => {
            return CanaryResult::SkippedUnsupported(format!(
                "mount namespace unavailable: tempdir failed: {err}"
            ))
        }
    };
    let root = temp.path().join("root");
    let project = temp.path().join("project");
    let toolchain = temp.path().join("toolchain");
    let secret = temp.path().join("host-secret");
    if let Err(err) = prepare_mount_view_fixture(&root, &project, &toolchain, &secret) {
        return CanaryResult::SkippedUnsupported(format!(
            "mount namespace unavailable: fixture setup failed: {err}"
        ));
    }

    let output = helper_command(HELPER_MOUNT_VIEW)
        .env("LATTICE_LIVE_ROOT", &root)
        .env("LATTICE_LIVE_PROJECT", &project)
        .env("LATTICE_LIVE_TOOLCHAIN", &toolchain)
        .env("LATTICE_LIVE_HOST_SECRET", &secret)
        .pipe_through_unshare(&unshare, &["--user", "--map-root-user", "--mount"])
        .output();

    match classify_helper_output("mount namespace unavailable", output) {
        Ok(()) => CanaryResult::Passed(
            "/project visible; broad HOME, /run, sockets, host temp secret, old root hidden; /tmp scratch works; read-only toolchain scope=top_level"
                .to_string(),
        ),
        Err(result) => result,
    }
}

fn prepare_mount_view_fixture(
    root: &Path,
    project: &Path,
    toolchain: &Path,
    secret: &Path,
) -> io::Result<()> {
    fs::create_dir_all(root.join("project"))?;
    fs::create_dir_all(root.join("tmp"))?;
    fs::create_dir_all(root.join("dev"))?;
    fs::create_dir_all(root.join("toolchain/ro0"))?;
    fs::create_dir_all(project)?;
    fs::create_dir_all(toolchain)?;
    fs::write(project.join("visible.txt"), b"project")?;
    fs::write(toolchain.join("tool.txt"), b"toolchain")?;
    fs::write(secret, b"host secret")?;
    for dev in ["null", "zero", "urandom"] {
        fs::File::create(root.join("dev").join(dev))?;
    }
    Ok(())
}

fn canary_fd_and_setup_pipe_cleanup() -> CanaryResult {
    let temp = match tempfile::NamedTempFile::new() {
        Ok(file) => file,
        Err(err) => {
            return CanaryResult::SkippedUnsupported(format!(
                "fd cleanup unavailable: temp file failed: {err}"
            ))
        }
    };
    let leak_fd = temp.as_file().as_raw_fd();
    if let Err(err) = clear_cloexec(leak_fd) {
        return CanaryResult::SkippedUnsupported(format!(
            "fd cleanup unavailable: cannot clear CLOEXEC on test fd: {err}"
        ));
    }

    let mut pipe = [0; 2];
    let pipe_rc = unsafe { libc::pipe(pipe.as_mut_ptr()) };
    if pipe_rc != 0 {
        return CanaryResult::SkippedUnsupported(format!(
            "fd cleanup unavailable: setup-report pipe failed: {}",
            io::Error::last_os_error()
        ));
    }
    let report_write = pipe[1];
    let output = helper_command(HELPER_FD_CLEANUP)
        .env("LATTICE_LIVE_LEAK_FD", leak_fd.to_string())
        .env("LATTICE_LIVE_REPORT_FD", report_write.to_string())
        .output();
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }

    match output {
        Ok(output) if output.status.success() => CanaryResult::Passed(
            "non-CLOEXEC inherited fd closed before final exec; setup-report pipe marked CLOEXEC and not leaked"
                .to_string(),
        ),
        Ok(output) => CanaryResult::Failed(output_text(&output)),
        Err(err) => CanaryResult::SkippedUnsupported(format!(
            "fd cleanup unavailable: helper spawn failed: {err}"
        )),
    }
}

fn canary_landlock_status() -> CanaryResult {
    let probe = super::landlock::probe_landlock_abi();
    if !probe.supported {
        return CanaryResult::SkippedUnsupported(format!(
            "Landlock ABI unsupported: {}",
            probe.reason
        ));
    }

    match helper_command(HELPER_LANDLOCK).output() {
        Ok(output) => classify_landlock_helper_output(&output),
        Err(err) => CanaryResult::SkippedUnsupported(format!(
            "Landlock ABI unsupported: helper spawn failed: {err}"
        )),
    }
}

fn canary_diagnostic_overlay_discard() -> CanaryResult {
    run_phase30_canary("diagnostic_overlay_discard", |rt| {
        rt.block_on(async {
            let fixture = LiveCommandFixture::new()?;
            let before = workspace_listing(fixture.root());
            let success = fixture
                .run_diagnostic(vec!["touch".into(), "diagnostic-success.txt".into()])
                .await?;
            require_success(&success.result, "diagnostic success command")?;
            if fixture.root().join("diagnostic-success.txt").exists() {
                return Err(CanaryResult::Failed(
                    "diagnostic overlay success left diagnostic-success.txt in real workspace"
                        .to_string(),
                ));
            }

            let failure = fixture
                .run_diagnostic(vec!["false".into()], )
                .await?;
            if failure.result.exit_code == 0 {
                return Err(CanaryResult::Failed(
                    "diagnostic failure path unexpectedly exited 0".to_string(),
                ));
            }
            if workspace_listing(fixture.root()) != before {
                return Err(CanaryResult::Failed(
                    "diagnostic overlay failure path changed the real workspace".to_string(),
                ));
            }
            require_metadata(&success.metadata, "overlay_status", "applied")?;
            require_metadata(&success.metadata, "overlay_disposition", "discarded")?;
            Ok("diagnostic overlay discarded upper writes after success and preserved the real workspace after a nonzero terminal path".to_string())
        })
    })
}

fn canary_artifact_overlay_commit() -> CanaryResult {
    run_phase30_canary("artifact_overlay_commit", |rt| {
        rt.block_on(async {
            let fixture = LiveCommandFixture::new()?;
            let output = fixture
                .run_artifact(
                    vec!["touch".into(), "artifact-commit.txt".into()],
                    ".",
                    Some("true"),
                )
                .await?;
            require_success(&output.result, "artifact command")?;
            if !fixture.root().join("artifact-commit.txt").is_file() {
                return Err(CanaryResult::Failed(
                    "accepted artifact did not reach the real workspace".to_string(),
                ));
            }
            require_metadata(&output.metadata, "artifact_manifest_status", "accepted")?;
            require_metadata(&output.metadata, "artifact_adjudication", "accepted")?;
            require_metadata(&output.metadata, "verify_layer", "discarded")?;
            require_metadata(&output.metadata, "commit_outcome", "committed")?;
            Ok("accepted artifact reached the real workspace only after adjudication, verify, cleanup, lower recheck, and commit".to_string())
        })
    })
}

fn canary_artifact_unsupported_kind_denial() -> CanaryResult {
    run_phase30_canary("artifact_unsupported_kind_denial", |rt| {
        rt.block_on(async {
            let fixture = LiveCommandFixture::new()?;
            let output = fixture
                .run_artifact_expect_error(
                    vec!["mkfifo".into(), "unsupported.pipe".into()],
                    ".",
                    Some("true"),
                )
                .await?;
            if fixture.root().join("unsupported.pipe").exists() {
                return Err(CanaryResult::Failed(
                    "unsupported FIFO artifact reached the real workspace".to_string(),
                ));
            }
            require_metadata(&output.metadata, "artifact_manifest_status", "denied")?;
            require_metadata(
                &output.metadata,
                "artifact_adjudication",
                "denied_unsupported_kind",
            )?;
            require_metadata(&output.metadata, "overlay_disposition", "denied")?;
            if output.metadata["rejected_kinds"]
                .as_array()
                .is_none_or(Vec::is_empty)
            {
                return Err(CanaryResult::Failed(
                    "unsupported artifact denial did not record rejected_kinds".to_string(),
                ));
            }
            Ok(
                "unsupported FIFO artifact was denied and discarded before real workspace mutation"
                    .to_string(),
            )
        })
    })
}

fn canary_artifact_lower_drift_denial() -> CanaryResult {
    run_phase30_canary("artifact_lower_drift_denial", |rt| {
        rt.block_on(async {
            let fixture = LiveCommandFixture::new()?;
            fs::write(fixture.root().join("baseline.txt"), b"before").map_err(|err| {
                CanaryResult::SkippedUnsupported(format!(
                    "artifact lower drift unavailable: fixture write failed: {err}"
                ))
            })?;
            let output = fixture
                .run_artifact_with_verify_drift(
                    vec!["touch".into(), "drift-artifact.txt".into()],
                    ".",
                    "baseline.txt",
                )
                .await?;
            if fixture.root().join("drift-artifact.txt").exists() {
                return Err(CanaryResult::Failed(
                    "lower drift denial still committed drift-artifact.txt".to_string(),
                ));
            }
            let baseline = fs::read_to_string(fixture.root().join("baseline.txt"))
                .unwrap_or_else(|_| String::new());
            if baseline != "drift" {
                return Err(CanaryResult::Failed(
                    "drift fixture did not mutate baseline during verify".to_string(),
                ));
            }
            require_metadata(&output.metadata, "artifact_adjudication", "denied_lower_drift")?;
            require_metadata(&output.metadata, "lower_consistency", "drift_detected")?;
            require_metadata(&output.metadata, "overlay_disposition", "denied")?;
            Ok("lower drift after verify denied artifact commit and left the real artifact path absent".to_string())
        })
    })
}

fn canary_artifact_verify_discard() -> CanaryResult {
    run_phase30_canary("artifact_verify_discard", |rt| {
        rt.block_on(async {
            let fixture = LiveCommandFixture::new()?;
            let output = fixture
                .run_artifact_with_verify_sentinel(
                    vec!["touch".into(), "verified-artifact.txt".into()],
                    ".",
                    "verify-only.txt",
                )
                .await?;
            require_success(&output.result, "artifact verify-discard command")?;
            if !fixture.root().join("verified-artifact.txt").is_file() {
                return Err(CanaryResult::Failed(
                    "verified artifact did not commit".to_string(),
                ));
            }
            if fixture.root().join("verify-only.txt").exists() {
                return Err(CanaryResult::Failed(
                    "verify-only write reached the committed artifact set".to_string(),
                ));
            }
            require_metadata(&output.metadata, "verify_layer", "discarded")?;
            require_metadata(&output.metadata, "commit_outcome", "committed")?;
            Ok("verify-only write was discarded while the accepted artifact committed".to_string())
        })
    })
}

fn classify_landlock_helper_output(output: &Output) -> CanaryResult {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && stdout.contains("landlock_status=applied") {
        CanaryResult::Passed(output_text(output))
    } else if stdout.contains("landlock_status=unsupported") {
        CanaryResult::SkippedUnsupported(output_text(output))
    } else {
        CanaryResult::Failed(output_text(output))
    }
}

struct LiveCommandFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

struct LiveCommandOutput {
    result: RunResult,
    metadata: serde_json::Value,
}

struct LiveCommandError {
    metadata: serde_json::Value,
}

impl LiveCommandFixture {
    fn new() -> Result<Self, CanaryResult> {
        let temp = tempfile::TempDir::new().map_err(|err| {
            CanaryResult::SkippedUnsupported(format!(
                "phase30 overlay canary unavailable: tempdir failed: {err}"
            ))
        })?;
        let root = temp.path().join("project");
        fs::create_dir_all(&root).map_err(|err| {
            CanaryResult::SkippedUnsupported(format!(
                "phase30 overlay canary unavailable: project fixture failed: {err}"
            ))
        })?;
        Ok(Self { _temp: temp, root })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    async fn run_diagnostic(&self, argv: Vec<String>) -> Result<LiveCommandOutput, CanaryResult> {
        let (ctx, durable) =
            live_command_ctx(&self.root, argv_allowlist(&argv), None, Vec::new(), None).await?;
        dispatch_live_command(ctx, durable, argv, None).await
    }

    async fn run_artifact(
        &self,
        argv: Vec<String>,
        write_pattern: &str,
        verify: Option<&str>,
    ) -> Result<LiveCommandOutput, CanaryResult> {
        let (ctx, durable) = live_command_ctx(
            &self.root,
            argv_allowlist(&argv),
            Some(write_pattern),
            Vec::new(),
            verify,
        )
        .await?;
        dispatch_live_command(ctx, durable, argv, Some(ImpactKind::WorkspaceMutation)).await
    }

    async fn run_artifact_expect_error(
        &self,
        argv: Vec<String>,
        write_pattern: &str,
        verify: Option<&str>,
    ) -> Result<LiveCommandError, CanaryResult> {
        let (ctx, durable) = live_command_ctx(
            &self.root,
            argv_allowlist(&argv),
            Some(write_pattern),
            Vec::new(),
            verify,
        )
        .await?;
        dispatch_live_command_error(ctx, durable, argv, Some(ImpactKind::WorkspaceMutation)).await
    }

    async fn run_artifact_with_verify_sentinel(
        &self,
        argv: Vec<String>,
        write_pattern: &str,
        verify_write: &str,
    ) -> Result<LiveCommandOutput, CanaryResult> {
        let helper = install_phase30_verify_helper(&self.root)?;
        let (ctx, durable) = live_command_ctx(
            &self.root,
            argv_allowlist(&argv),
            Some(write_pattern),
            vec![vec!["phase30-verify-helper".into()]],
            Some("phase30-verify-helper"),
        )
        .await?;
        let output = dispatch_live_command_with_env(
            ctx,
            durable,
            argv,
            Some(ImpactKind::WorkspaceMutation),
            [(
                PHASE30_VERIFY_SENTINEL.to_string(),
                verify_write.to_string(),
            )]
            .into_iter()
            .collect(),
        )
        .await;
        let _ = fs::remove_file(helper);
        output
    }

    async fn run_artifact_with_verify_drift(
        &self,
        argv: Vec<String>,
        write_pattern: &str,
        drift_path: &str,
    ) -> Result<LiveCommandError, CanaryResult> {
        let helper = install_phase30_verify_helper(&self.root)?;
        let (ctx, durable) = live_command_ctx(
            &self.root,
            argv_allowlist(&argv),
            Some(write_pattern),
            vec![vec!["phase30-verify-helper".into()]],
            Some("phase30-verify-helper"),
        )
        .await?;
        let output = dispatch_live_command_error_with_env(
            ctx,
            durable,
            argv,
            Some(ImpactKind::WorkspaceMutation),
            [(PHASE30_VERIFY_SENTINEL.to_string(), drift_path.to_string())]
                .into_iter()
                .collect(),
        )
        .await;
        let _ = fs::remove_file(helper);
        output
    }
}

async fn live_command_ctx(
    root: &Path,
    argv_allowlist: Vec<String>,
    write_path_pattern: Option<&str>,
    extra_diagnostic_commands: Vec<Vec<String>>,
    verify: Option<&str>,
) -> Result<
    (
        HandlerContext,
        crate::server::call_durable_metadata::CallDurableMetadata,
    ),
    CanaryResult,
> {
    let root = root.to_string_lossy().to_string();
    let (event_tx, _event_rx) = mpsc::channel(64);
    let (_cancel_tx, cancel_rx) = watch::channel(false);

    let mut policy_config =
        lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(PathBuf::from(
            &root,
        ));
    for command in [
        vec!["touch".into()],
        vec!["false".into()],
        vec!["true".into()],
    ] {
        if !policy_config
            .command
            .allowed_diagnostic_commands
            .contains(&command)
        {
            policy_config
                .command
                .allowed_diagnostic_commands
                .push(command);
        }
    }
    for command in extra_diagnostic_commands {
        if !policy_config
            .command
            .allowed_diagnostic_commands
            .contains(&command)
        {
            policy_config
                .command
                .allowed_diagnostic_commands
                .push(command);
        }
    }
    if !policy_config
        .command
        .allowed_env
        .contains(&PHASE30_VERIFY_SENTINEL.to_string())
    {
        policy_config
            .command
            .allowed_env
            .push(PHASE30_VERIFY_SENTINEL.to_string());
    }
    let cep_policy = policy_config.to_cep_policy_config();
    let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
        policy_config,
    ));

    let store = Arc::new(ActiveEnvelopeStore::new());
    let mut capabilities = std::collections::BTreeSet::new();
    capabilities.insert(Capability::RunCommand { argv_allowlist });
    if let Some(pattern) = write_path_pattern {
        capabilities.insert(Capability::WritePath {
            path_pattern: pattern.to_string(),
        });
    }
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "phase30 live overlay canary".into(),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(root.clone()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![Impact {
            kind: if write_path_pattern.is_some() {
                ImpactKind::WorkspaceMutation
            } else {
                ImpactKind::CommandExecution
            },
            target: root.clone(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: verify.map(|command| VerifyCommand {
            command: command.into(),
            expected_exit_code: Some(0),
        }),
        on_failure: Some(OnFailure::RetainDiff),
        extensions: serde_json::Map::new(),
    };
    store
        .declare(envelope, 0, "phase30-live")
        .await
        .map_err(|err| {
            CanaryResult::SkippedUnsupported(format!(
                "phase30 overlay canary unavailable: envelope declaration failed: {err:?}"
            ))
        })?;

    let durable_metadata = crate::server::call_durable_metadata::CallDurableMetadata::default();
    let ctx = HandlerContext {
        call_id: 30,
        events: EventSink::new(event_tx, 30),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: BaseExecutionEnvelope::for_workspace_root(&root, "phase30 live canary"),
        cep_context: Arc::new(CepTurnContext::from_policy(CepPolicyConfig {
            command: cep_policy.command.clone(),
            ..cep_policy
        })),
        active_envelope: store,
        turn_id: "phase30-live".into(),
        durable_metadata: durable_metadata.clone(),
        effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: Some(default_hook),
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "phase30-live".into(),
            session_id: "phase30-live-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: live_overlay_isolation_config(),
    };

    Ok((ctx, durable_metadata))
}

fn live_overlay_isolation_config() -> CommandIsolationRuntimeConfig {
    CommandIsolationRuntimeConfig {
        mode: CommandIsolationMode::Enforce,
        required_boundaries: vec![lancet_protocol::BoundaryDimension::Filesystem],
        optional_boundaries: Vec::new(),
        read_only_toolchain_paths: live_toolchain_paths(),
        scratch_paths: Vec::new(),
        landlock: CommandIsolationLandlockBehavior::Off,
        unsupported_host: CommandIsolationUnsupportedHostBehavior::FailClosed,
        host_probe: None,
    }
}

fn live_toolchain_paths() -> Vec<PathBuf> {
    ["/bin", "/usr"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect()
}

async fn dispatch_live_command(
    ctx: HandlerContext,
    durable: crate::server::call_durable_metadata::CallDurableMetadata,
    argv: Vec<String>,
    declared_impact: Option<ImpactKind>,
) -> Result<LiveCommandOutput, CanaryResult> {
    dispatch_live_command_with_env(ctx, durable, argv, declared_impact, Default::default()).await
}

async fn dispatch_live_command_with_env(
    ctx: HandlerContext,
    durable: crate::server::call_durable_metadata::CallDurableMetadata,
    argv: Vec<String>,
    declared_impact: Option<ImpactKind>,
    env: std::collections::BTreeMap<String, String>,
) -> Result<LiveCommandOutput, CanaryResult> {
    let registry = live_command_registry();
    let params = RunParams {
        argv,
        cwd: None,
        env,
        timeout_ms: Some(10_000),
        declared_impact,
    };
    match registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
    {
        Ok(value) => Ok(LiveCommandOutput {
            result: serde_json::from_value(value).map_err(|err| {
                CanaryResult::Failed(format!("phase30 command result decode failed: {err}"))
            })?,
            metadata: durable_metadata_json(&durable),
        }),
        Err(error) => Err(classify_live_command_error(
            error,
            durable_metadata_json(&durable),
        )),
    }
}

async fn dispatch_live_command_error(
    ctx: HandlerContext,
    durable: crate::server::call_durable_metadata::CallDurableMetadata,
    argv: Vec<String>,
    declared_impact: Option<ImpactKind>,
) -> Result<LiveCommandError, CanaryResult> {
    dispatch_live_command_error_with_env(ctx, durable, argv, declared_impact, Default::default())
        .await
}

async fn dispatch_live_command_error_with_env(
    ctx: HandlerContext,
    durable: crate::server::call_durable_metadata::CallDurableMetadata,
    argv: Vec<String>,
    declared_impact: Option<ImpactKind>,
    env: std::collections::BTreeMap<String, String>,
) -> Result<LiveCommandError, CanaryResult> {
    let registry = live_command_registry();
    let params = RunParams {
        argv,
        cwd: None,
        env,
        timeout_ms: Some(10_000),
        declared_impact,
    };
    match registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
    {
        Ok(value) => {
            let result: RunResult = serde_json::from_value(value).map_err(|err| {
                CanaryResult::Failed(format!("phase30 command result decode failed: {err}"))
            })?;
            Err(CanaryResult::Failed(format!(
                "phase30 command unexpectedly succeeded with exit_code={}",
                result.exit_code
            )))
        }
        Err(error) => {
            let metadata = durable_metadata_json(&durable);
            if let Some(skip) = classify_live_command_error_as_skip(&error, &metadata) {
                return Err(skip);
            }
            Ok(LiveCommandError { metadata })
        }
    }
}

fn live_command_registry() -> MethodRegistry {
    let mut registry = MethodRegistry::new();
    register_command_methods(&mut registry).expect("register command methods");
    registry
}

fn classify_live_command_error(
    error: lancet_protocol::CepError,
    metadata: serde_json::Value,
) -> CanaryResult {
    classify_live_command_error_as_skip(&error, &metadata).unwrap_or_else(|| {
        CanaryResult::Failed(format!(
            "phase30 command failed: code={:?} message={} metadata={metadata}",
            error.code, error.message
        ))
    })
}

fn classify_live_command_error_as_skip(
    error: &lancet_protocol::CepError,
    metadata: &serde_json::Value,
) -> Option<CanaryResult> {
    let text = format!("{} {metadata}", error.message).to_ascii_lowercase();
    let setup_failed = metadata
        .get("boundary_setup_failed_closed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || text.contains("setup_failure_reason")
        || text.contains("overlay_mount")
        || text.contains("operation not permitted")
        || text.contains("permission denied");
    if setup_failed {
        return Some(CanaryResult::SkippedUnsupported(format!(
            "phase30 overlay unavailable: {}",
            error.message
        )));
    }
    if text.contains("no such file or directory") && text.contains("failed to spawn command") {
        return Some(CanaryResult::SkippedUnsupported(format!(
            "phase30 command execution unavailable inside supervisor view: {}",
            error.message
        )));
    }
    None
}

fn durable_metadata_json(
    durable: &crate::server::call_durable_metadata::CallDurableMetadata,
) -> serde_json::Value {
    durable
        .take_json()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn argv_allowlist(argv: &[String]) -> Vec<String> {
    argv.first().cloned().into_iter().collect()
}

fn require_success(result: &RunResult, label: &str) -> Result<(), CanaryResult> {
    if result.exit_code == 0 {
        Ok(())
    } else {
        Err(CanaryResult::Failed(format!(
            "{label} exited {} stdout={} stderr={}",
            result.exit_code, result.stdout, result.stderr
        )))
    }
}

fn require_metadata(
    metadata: &serde_json::Value,
    key: &str,
    expected: &str,
) -> Result<(), CanaryResult> {
    if metadata.get(key).and_then(serde_json::Value::as_str) == Some(expected) {
        Ok(())
    } else {
        Err(CanaryResult::Failed(format!(
            "metadata {key} expected {expected}, got {}",
            metadata.get(key).unwrap_or(&serde_json::Value::Null)
        )))
    }
}

fn workspace_listing(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    collect_workspace_listing(root, root, &mut paths);
    paths.sort();
    paths
}

fn collect_workspace_listing(root: &Path, current: &Path, paths: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(relative) = path.strip_prefix(root) {
            paths.push(relative.to_string_lossy().to_string());
        }
        if path.is_dir() {
            collect_workspace_listing(root, &path, paths);
        }
    }
}

fn install_phase30_verify_helper(root: &Path) -> Result<PathBuf, CanaryResult> {
    let helper = root.join("phase30-verify-helper");
    let script = format!(
        "#!/bin/sh\nif [ -n \"${{{PHASE30_VERIFY_SENTINEL}:-}}\" ]; then touch \"${{{PHASE30_VERIFY_SENTINEL}}}\"; fi\n"
    );
    fs::write(&helper, script).map_err(|err| {
        CanaryResult::SkippedUnsupported(format!(
            "phase30 verify helper unavailable: write failed: {err}"
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&helper)
            .map_err(|err| {
                CanaryResult::SkippedUnsupported(format!(
                    "phase30 verify helper unavailable: metadata failed: {err}"
                ))
            })?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper, permissions).map_err(|err| {
            CanaryResult::SkippedUnsupported(format!(
                "phase30 verify helper unavailable: chmod failed: {err}"
            ))
        })?;
    }
    Ok(helper)
}

fn run_phase30_canary(
    name: &str,
    f: impl FnOnce(&tokio::runtime::Runtime) -> Result<String, CanaryResult>,
) -> CanaryResult {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            return CanaryResult::SkippedUnsupported(format!(
                "{name} unavailable: tokio runtime setup failed: {err}"
            ))
        }
    };
    match f(&runtime) {
        Ok(detail) => CanaryResult::Passed(detail),
        Err(result) => result,
    }
}

#[cfg(unix)]
#[test]
fn landlock_live_canary_requires_applied_status_to_pass() {
    use std::os::unix::process::ExitStatusExt;

    let applied = Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: b"landlock_status=applied\n".to_vec(),
        stderr: Vec::new(),
    };
    assert!(matches!(
        classify_landlock_helper_output(&applied),
        CanaryResult::Passed(_)
    ));

    let not_applied = Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: b"landlock_status=not_applied\n".to_vec(),
        stderr: Vec::new(),
    };
    assert!(matches!(
        classify_landlock_helper_output(&not_applied),
        CanaryResult::Failed(_)
    ));

    let unsupported = Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: b"landlock_status=unsupported\n".to_vec(),
        stderr: Vec::new(),
    };
    assert!(matches!(
        classify_landlock_helper_output(&unsupported),
        CanaryResult::SkippedUnsupported(_)
    ));
}

#[test]
#[ignore]
fn lattice_live_canary_netns_helper() {
    if env::var(HELPER_ENV).as_deref() != Ok(HELPER_NETNS) {
        return;
    }
    let port = env::var("LATTICE_LIVE_PARENT_PORT")
        .expect("LATTICE_LIVE_PARENT_PORT")
        .parse::<u16>()
        .expect("port is u16");
    if TcpStream::connect(("127.0.0.1", port)).is_ok() {
        panic!("connected to parent listener from child namespace");
    }

    let parent_netns = env::var("LATTICE_LIVE_PARENT_NETNS").unwrap_or_default();
    let child_netns = fs::read_link("/proc/self/ns/net")
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    assert_ne!(
        child_netns, parent_netns,
        "child joined the host network namespace"
    );

    let devices = fs::read_to_string("/proc/net/dev").expect("/proc/net/dev readable");
    assert_proc_net_dev_has_only_loopback(&devices)
        .expect("fresh child netns has only loopback interface");
    let routes = fs::read_to_string("/proc/net/route").expect("/proc/net/route readable");
    assert_proc_net_route_has_no_default_route(&routes)
        .expect("fresh child netns has no default route");
}

#[test]
#[ignore]
fn lattice_live_canary_mount_view_helper() {
    if env::var(HELPER_ENV).as_deref() != Ok(HELPER_MOUNT_VIEW) {
        return;
    }
    let root = PathBuf::from(env::var("LATTICE_LIVE_ROOT").expect("LATTICE_LIVE_ROOT"));
    let project = PathBuf::from(env::var("LATTICE_LIVE_PROJECT").expect("LATTICE_LIVE_PROJECT"));
    let toolchain =
        PathBuf::from(env::var("LATTICE_LIVE_TOOLCHAIN").expect("LATTICE_LIVE_TOOLCHAIN"));
    let secret = PathBuf::from(env::var("LATTICE_LIVE_HOST_SECRET").expect("secret"));

    mount_bind(&project, &root.join("project")).expect("project bind mount");
    mount_bind(&toolchain, &root.join("toolchain/ro0")).expect("toolchain bind mount");
    remount_read_only(&root.join("toolchain/ro0")).expect("toolchain read-only remount");
    mount_tmpfs(&root.join("tmp")).expect("scratch tmpfs");
    for dev in ["null", "zero", "urandom"] {
        mount_bind(
            Path::new("/dev").join(dev).as_path(),
            &root.join("dev").join(dev),
        )
        .unwrap_or_else(|err| panic!("minimal device {dev} bind mount: {err}"));
    }

    chroot_to(&root).expect("chroot into minimal mount view");
    assert!(
        Path::new("/project/visible.txt").is_file(),
        "/project missing"
    );
    assert!(Path::new("/tmp").is_dir(), "scratch /tmp missing");
    fs::write("/tmp/scratch-ok", b"scratch").expect("scratch tmpfs is writable");
    assert!(Path::new("/dev/null").exists(), "minimal /dev/null missing");

    assert!(!Path::new("/home").exists(), "broad HOME tree is visible");
    assert!(!Path::new("/run").exists(), "/run is visible");
    assert!(
        !Path::new("/var/run/docker.sock").exists(),
        "Docker socket is visible"
    );
    for key in [
        "SSH_AUTH_SOCK",
        "LATTICE_AGENT_SOCKET",
        "CODEX_AGENT_SOCKET",
    ] {
        if let Ok(value) = env::var(key) {
            assert!(
                !Path::new(&value).exists(),
                "{key} target is visible inside mount view"
            );
        }
    }
    assert!(
        !secret.exists(),
        "host temp secret remained visible inside mount view"
    );
    assert!(!Path::new("/old_root").exists(), "old root is reachable");
    assert!(
        fs::write("/toolchain/ro0/should-not-write", b"blocked").is_err(),
        "read-only toolchain path accepted a write"
    );
    println!("read_only_bind_scope=top_level");
}

#[test]
#[ignore]
fn lattice_live_canary_fd_cleanup_helper() {
    if env::var(HELPER_ENV).as_deref() != Ok(HELPER_FD_CLEANUP) {
        return;
    }
    let leak_fd = env::var("LATTICE_LIVE_LEAK_FD")
        .expect("leak fd")
        .parse::<RawFd>()
        .expect("leak fd raw");
    let report_fd = env::var("LATTICE_LIVE_REPORT_FD")
        .expect("report fd")
        .parse::<RawFd>()
        .expect("report fd raw");

    let mut backend = LiveFdCleanupBackend;
    let report = cleanup_inherited_fds(&mut backend, &[report_fd]);
    assert_eq!(report.status.label(), "applied", "{report:?}");
    set_cloexec(report_fd).expect("setup-report pipe CLOEXEC");
    raw_write(report_fd, b"ready\n").expect("setup-report write");

    let output = helper_command(HELPER_FD_FINAL)
        .env("LATTICE_LIVE_LEAK_FD", leak_fd.to_string())
        .env("LATTICE_LIVE_REPORT_FD", report_fd.to_string())
        .output()
        .expect("spawn final fd helper");
    assert!(output.status.success(), "{}", output_text(&output));
}

#[test]
#[ignore]
fn lattice_live_canary_fd_final_helper() {
    if env::var(HELPER_ENV).as_deref() != Ok(HELPER_FD_FINAL) {
        return;
    }
    let leak_fd = env::var("LATTICE_LIVE_LEAK_FD")
        .expect("leak fd")
        .parse::<RawFd>()
        .expect("leak fd raw");
    let report_fd = env::var("LATTICE_LIVE_REPORT_FD")
        .expect("report fd")
        .parse::<RawFd>()
        .expect("report fd raw");
    assert!(!fd_is_open(leak_fd), "non-CLOEXEC fd leaked after exec");
    assert!(
        !fd_is_open(report_fd),
        "setup-report pipe fd leaked after exec"
    );
}

#[test]
#[ignore]
fn lattice_live_canary_landlock_helper() {
    if env::var(HELPER_ENV).as_deref() != Ok(HELPER_LANDLOCK) {
        return;
    }
    let probe = super::landlock::probe_landlock_abi();
    if !probe.supported {
        println!("landlock_status=unsupported reason={}", probe.reason);
        return;
    }
    let abi = probe.abi_version.expect("supported ABI version");
    let rights = LandlockRightsMask::for_abi(abi);
    let plan = LandlockRefinementPlan {
        abi_version: abi,
        required: false,
        fail_closed: false,
        handled_access_fs: rights.handled_access_fs,
        rules: Vec::new(),
    };
    let mut syscalls = LiveSupervisorSyscalls::default();
    match syscalls.apply_landlock(&plan) {
        Ok(report) => println!(
            "landlock_status={} landlock_abi_version={} landlock_rights_mask=0x{:x}",
            report.status.label(),
            report.abi_version.unwrap_or(abi),
            report.rights_mask.unwrap_or(rights.handled_access_fs)
        ),
        Err(err) => {
            println!(
                "landlock_status=not_applied landlock_abi_version={abi} landlock_rights_mask=0x{:x} reason={err}",
                rights.handled_access_fs
            );
            panic!("Landlock ABI is supported but apply_landlock did not apply");
        }
    }
}

struct HelperCommand {
    inner: Command,
}

impl HelperCommand {
    fn env<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: AsRef<std::ffi::OsStr>,
        V: AsRef<std::ffi::OsStr>,
    {
        self.inner.env(key, value);
        self
    }

    fn output(&mut self) -> io::Result<Output> {
        self.inner.output()
    }

    fn pipe_through_unshare(&mut self, unshare: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(unshare);
        command.args(args);
        command.arg(current_test_exe());
        for arg in self.inner.get_args() {
            command.arg(arg);
        }
        for (key, value) in self.inner.get_envs() {
            if let Some(value) = value {
                command.env(key, value);
            }
        }
        command
    }
}

fn helper_command(helper: &str) -> HelperCommand {
    let mut command = Command::new(current_test_exe());
    command
        .arg(helper_test_filter(helper))
        .arg("--ignored")
        .arg("--nocapture")
        .env(HELPER_ENV, helper);
    HelperCommand { inner: command }
}

fn helper_test_filter(helper: &str) -> &'static str {
    match helper {
        HELPER_NETNS => "lattice_live_canary_netns_helper",
        HELPER_MOUNT_VIEW => "lattice_live_canary_mount_view_helper",
        HELPER_FD_CLEANUP => "lattice_live_canary_fd_cleanup_helper",
        HELPER_FD_FINAL => "lattice_live_canary_fd_final_helper",
        HELPER_LANDLOCK => "lattice_live_canary_landlock_helper",
        _ => "linux_isolation_live_canary_suite",
    }
}

fn current_test_exe() -> PathBuf {
    env::current_exe().expect("current test binary")
}

fn classify_helper_output(
    unsupported_prefix: &str,
    output: io::Result<Output>,
) -> Result<(), CanaryResult> {
    match output {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let text = output_text(&output);
            if let Some(reason) = classify_unsupported_reason(unsupported_prefix, &text) {
                Err(CanaryResult::SkippedUnsupported(reason))
            } else {
                Err(CanaryResult::Failed(text))
            }
        }
        Err(err) => Err(CanaryResult::SkippedUnsupported(format!(
            "{unsupported_prefix}: helper spawn failed: {err}"
        ))),
    }
}

fn classify_unsupported_reason(prefix: &str, text: &str) -> Option<String> {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("operation not permitted")
        || lowered.contains("permission denied")
        || lowered.contains("not permitted")
    {
        Some(format!("{prefix}: operation not permitted"))
    } else if lowered.contains("user namespace") && lowered.contains("disabled") {
        Some(format!("{prefix}: user namespace disabled"))
    } else if lowered.contains("uid_map")
        || lowered.contains("gid_map")
        || (lowered.contains("invalid option") && lowered.contains("map-root-user"))
    {
        Some(format!("{prefix}: uid/gid map unavailable"))
    } else {
        None
    }
}

fn output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!(
        "status={} stdout={} stderr={}",
        output.status,
        stdout.trim(),
        stderr.trim()
    )
}

fn find_executable(paths: &[&str]) -> Option<PathBuf> {
    paths.iter().map(PathBuf::from).find(|path| path.is_file())
}

fn clear_cloexec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn fd_is_open(fd: RawFd) -> bool {
    let rc = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    rc >= 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EBADF)
}

fn raw_write(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    let rc = unsafe { libc::write(fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len()) };
    if rc == bytes.len() as isize {
        Ok(())
    } else if rc >= 0 {
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short setup-report write",
        ))
    } else {
        Err(io::Error::last_os_error())
    }
}

fn mount_bind(source: &Path, target: &Path) -> io::Result<()> {
    live_mount(
        Some(source),
        target,
        None,
        libc::MS_BIND | libc::MS_REC,
        None,
    )
}

fn remount_read_only(target: &Path) -> io::Result<()> {
    live_mount(
        None,
        target,
        None,
        libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
        None,
    )
}

fn mount_tmpfs(target: &Path) -> io::Result<()> {
    live_mount(
        Some(Path::new("tmpfs")),
        target,
        Some("tmpfs"),
        0,
        Some("mode=700"),
    )
}

fn live_mount(
    source: Option<&Path>,
    target: &Path,
    fstype: Option<&str>,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = source
        .map(|path| CString::new(path.as_os_str().as_bytes()))
        .transpose()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let fstype = fstype
        .map(CString::new)
        .transpose()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let data = data
        .map(CString::new)
        .transpose()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

    let rc = unsafe {
        libc::mount(
            source
                .as_ref()
                .map(|value| value.as_ptr())
                .unwrap_or(std::ptr::null()),
            target.as_ptr(),
            fstype
                .as_ref()
                .map(|value| value.as_ptr())
                .unwrap_or(std::ptr::null()),
            flags,
            data.as_ref()
                .map(|value| value.as_ptr().cast())
                .unwrap_or(std::ptr::null()),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn chroot_to(root: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let root = CString::new(root.as_os_str().as_bytes())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let rc = unsafe { libc::chroot(root.as_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    env::set_current_dir("/")
}
