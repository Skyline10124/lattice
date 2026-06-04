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

use super::fd::{cleanup_inherited_fds, set_cloexec, LiveFdCleanupBackend};
use super::landlock::{LandlockRefinementPlan, LandlockRightsMask};
use super::syscall::{
    assert_proc_net_dev_has_only_loopback, assert_proc_net_route_has_no_default_route,
    LiveSupervisorSyscalls, SupervisorSyscalls,
};

const LIVE_ENV: &str = "LATTICE_LINUX_ISOLATION_LIVE";
const HELPER_ENV: &str = "LATTICE_LIVE_CANARY_HELPER";
const HELPER_NETNS: &str = "netns";
const HELPER_MOUNT_VIEW: &str = "mount_view";
const HELPER_FD_CLEANUP: &str = "fd_cleanup";
const HELPER_FD_FINAL: &str = "fd_final";
const HELPER_LANDLOCK: &str = "landlock";

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
