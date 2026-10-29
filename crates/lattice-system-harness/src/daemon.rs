// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::driver::{CleanupError, RunningCommand, process_stat, processes_in_pgid};

/// 强制 SIGKILL 之后核验组已空的独立有界等待；只用于安全清理，不替代或放宽正常 stop 的预算。
const FORCED_CLEANUP_GRACE: Duration = Duration::from_secs(2);
/// 收尾 stop 未干净返回（not_running 或失败）时，等待已识别后台组自行退出的上限，不超过剩余正常预算。
/// 干净返回的 stop 走 `departed`：实例锁与 socket 立即核验，进程组只在 `DEPARTURE_EXIT_WINDOW` 内有界等待。
const STOP_DRAIN_WINDOW: Duration = Duration::from_secs(2);
/// stop 干净返回、实例锁已放开后，等待自有后台组退空的上限；超时即判失败，不再追加排空窗口。
const DEPARTURE_EXIT_WINDOW: Duration = Duration::from_secs(5);

/// 前台进程组不包含 setsid 后的后台；场景必须另持清理归属。
pub(crate) struct ScenarioDaemon {
    state_root: PathBuf,
    binary: Mutex<Option<PathBuf>>,
    /// 保存已确认组中的进程实例；连续存活的实例才可把归属传给下一次观察，数值 PGID 不够。
    confirmed: Mutex<BTreeMap<u32, Vec<OwnedProcess>>>,
}

/// pidfd 绑定一个内核进程实例；退出后即失效，不会因数值 PID 被复用而重新变活。
struct OwnedProcess {
    pid: u32,
    fd: OwnedFd,
}

impl OwnedProcess {
    fn open(pid: u32) -> std::io::Result<Self> {
        // SAFETY: pidfd_open 不接收指针；成功返回本函数独占的新 fd。
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            pid,
            // SAFETY: syscall 成功，fd 的唯一所有权移交给 OwnedFd。
            fd: unsafe { OwnedFd::from_raw_fd(fd as i32) },
        })
    }

    fn in_group(&self, pgid: u32, isolated: bool) -> std::io::Result<bool> {
        let stat = process_stat(self.pid)
            .map_err(|error| std::io::Error::new(error.kind, error.message))?;
        let mut poll = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll 指向一个有效 pollfd，零超时只查询该实例是否已退出。
        let result = unsafe { libc::poll(&mut poll, 1, 0) };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(std::io::Error::other("daemon pidfd inspection failed"));
        }
        // 必须在读取 /proc 后检查旧实例仍活着，否则元数据可能已来自复用后的新进程。
        Ok(result == 0
            && stat.is_some_and(|stat| stat.pgid == pgid && (!isolated || stat.session == pgid)))
    }

    fn kill(&self) -> std::io::Result<()> {
        // SAFETY: 空 siginfo 与 flags=0 使用 SIGKILL 的默认语义；目标是已核验实例而非数值组。
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            // pidfd 的 ESRCH 只表示此实例已退出，不会静默改指向新进程。
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
}

fn capture_members(pgid: u32, members: &[u32]) -> std::io::Result<Vec<OwnedProcess>> {
    let mut captured = Vec::new();
    for &pid in members {
        let member = match OwnedProcess::open(pid) {
            Ok(member) => member,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => continue,
            Err(error) => return Err(error),
        };
        if member.in_group(pgid, false)? {
            captured.push(member);
        }
    }
    Ok(captured)
}

impl ScenarioDaemon {
    pub(crate) fn new(state_root: PathBuf) -> Self {
        Self {
            state_root,
            binary: Mutex::new(None),
            confirmed: Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn register(&self, binary: &Path) -> std::io::Result<()> {
        let mut registered = self.binary.lock().expect("daemon binary lock");
        if registered
            .as_deref()
            .is_some_and(|current| current != binary)
        {
            return Err(std::io::Error::other("scenario changed its product binary"));
        }
        *registered = Some(binary.to_owned());
        Ok(())
    }

    /// 仅识别本场景状态根、当前产物和独立组首领；不凭 PID 文件猜测归属。
    pub(crate) fn owners(&self) -> std::io::Result<Vec<u32>> {
        self.owners_with_inspection(&mut processes_in_pgid)
    }

    fn owners_with_inspection(
        &self,
        inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
    ) -> std::io::Result<Vec<u32>> {
        let binary = self.binary.lock().expect("daemon binary lock");
        let Some(binary) = binary.as_ref() else {
            return Ok(Vec::new());
        };
        let mut owners = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            // 先核对可执行文件，不读取无关进程的参数或环境。
            if std::fs::read_link(entry.path().join("exe")).ok().as_ref() != Some(binary) {
                continue;
            }
            let leader = match OwnedProcess::open(pid) {
                Ok(leader) => leader,
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => continue,
                Err(error) => return Err(error),
            };
            // pidfd 打开之前的路径观察不作为实例证据，绑定后再次核对。
            let current_binary = std::fs::read_link(entry.path().join("exe"));
            if current_binary.ok().as_ref() != Some(binary) {
                continue;
            }
            let args = match std::fs::read(entry.path().join("cmdline")) {
                Ok(args) => args,
                Err(error) if crate::driver::process_exited(&error) => continue,
                Err(error) => return Err(error),
            };
            let fields: Vec<_> = args
                .split(|byte| *byte == 0)
                .filter(|part| !part.is_empty())
                .collect();
            if fields.len() == 5
                && fields[1..4] == [b"daemon".as_slice(), b"serve", b"--state-root"]
                && fields[4] == self.state_root.as_os_str().as_bytes()
            {
                let members =
                    inspect(pid).map_err(|error| std::io::Error::new(error.kind, error.message))?;
                let mut captured = capture_members(pid, &members)?;
                // 扫描后旧 leader 仍在原组，才能把这次组观察归入它确证过的实例。
                if leader.in_group(pid, false)? {
                    captured.push(leader);
                    self.confirmed
                        .lock()
                        .expect("daemon confirmed lock")
                        .insert(pid, captured);
                    owners.push(pid);
                }
            }
        }
        owners.sort_unstable();
        let mut confirmed = self.confirmed.lock().expect("daemon confirmed lock");
        // 首领已退出且组已空的后台不会回来；保留它只会指向将来重用同一 PGID 的陌生进程。
        let mut inspection_error = None;
        confirmed.retain(|pgid, _| {
            if owners.contains(pgid) {
                return true;
            }
            match inspect(*pgid) {
                Ok(members) => !members.is_empty(),
                Err(error) => {
                    inspection_error
                        .get_or_insert_with(|| std::io::Error::new(error.kind, error.message));
                    true
                }
            }
        });
        match inspection_error {
            Some(error) => Err(error),
            None => Ok(owners),
        }
    }

    /// `daemon stop` 干净返回后的退场核验。stop 只承诺放开实例：`owner.lock` 可取得、`api.sock` 已撤下，
    /// 这两项立即核验、不轮询。进程退出不在 stop 的承诺内，锁放开后进程仍可短暂存在，
    /// 因此按 harness 自己确认过的 PGID 在 `DEPARTURE_EXIT_WINDOW` 内有界等待组退空。
    pub(crate) fn departed(&self) -> Result<(), Vec<String>> {
        self.departed_with_inspection(&mut processes_in_pgid)
    }

    fn departed_with_inspection(
        &self,
        inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        match owner_lock_held(&self.state_root) {
            Ok(false) => {}
            Ok(true) => errors.push("daemon stop returned while owner.lock is still held".into()),
            Err(error) => errors.push(format!("owner.lock inspection failed: {error}")),
        }
        match std::fs::symlink_metadata(self.state_root.join("lattice/local/api.sock")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => errors.push("daemon stop returned while api.sock still exists".into()),
            Err(error) => errors.push(format!("api.sock inspection failed: {error}")),
        }
        // 扫描后 confirmed 只剩仍有成员的组，包括本次新识别的首领。
        if self.owners_with_inspection(inspect).is_err() {
            errors.push("daemon ownership inspection failed".into());
            return finish(errors);
        }
        let groups: Vec<u32> = self
            .confirmed
            .lock()
            .expect("daemon confirmed lock")
            .keys()
            .copied()
            .collect();
        let deadline = Instant::now() + DEPARTURE_EXIT_WINDOW;
        loop {
            let mut live = Vec::new();
            for &pgid in &groups {
                match inspect(pgid) {
                    Ok(members) if members.is_empty() => {}
                    Ok(_) => live.push(pgid),
                    Err(error) => {
                        errors.push(format!(
                            "daemon group inspection failed: pgid={pgid} {}",
                            error.message
                        ));
                        return finish(errors);
                    }
                }
            }
            if live.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                errors.push(format!(
                    "backend process group did not exit within {DEPARTURE_EXIT_WINDOW:?} after daemon stop returned: {live:?}"
                ));
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        finish(errors)
    }

    /// 正常收尾用场景剩余预算发出 `daemon stop` 并等待退出；预算耗尽时记录失败。
    /// 之后只对归属复核通过且仍有成员的自有组强制清理，并在独立有界宽限内核验。
    pub(crate) fn cleanup(&self, normal_deadline: Instant) -> Result<(), Vec<String>> {
        self.cleanup_with_inspection(normal_deadline, &mut processes_in_pgid)
    }

    fn cleanup_with_inspection(
        &self,
        normal_deadline: Instant,
        inspect: &mut impl FnMut(u32) -> Result<Vec<u32>, CleanupError>,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let owners = self
            .owners_with_inspection(inspect)
            .map_err(|_| vec!["daemon ownership inspection failed".into()])?;
        // 首领退出不等于组退出：收尾对象是所有曾确认且仍有成员的组。
        let confirmed: Vec<u32> = self
            .confirmed
            .lock()
            .expect("daemon confirmed lock")
            .keys()
            .copied()
            .collect();
        let mut groups = Vec::new();
        for pgid in confirmed {
            match inspect(pgid) {
                Ok(members) if members.is_empty() => {}
                Ok(_) => groups.push(pgid),
                Err(error) => errors.push(format!(
                    "daemon group inspection failed: pgid={pgid} {}",
                    error.message
                )),
            }
        }
        if groups.is_empty() {
            return finish(errors);
        }
        if owners.is_empty() {
            errors.push(format!(
                "daemon leader already exited with live group members: {groups:?}"
            ));
        } else if Instant::now() >= normal_deadline {
            errors.push("scenario budget exhausted before normal daemon stop".into());
        } else {
            let binary = self
                .binary
                .lock()
                .expect("daemon binary lock")
                .clone()
                .expect("registered binary");
            let mut command = Command::new(binary);
            command
                .args(["--json", "daemon", "stop"])
                .current_dir("/")
                .env_clear();
            command.env("XDG_STATE_HOME", &self.state_root);
            command.env("HOME", self.state_root.join("cleanup-home"));
            let stop = RunningCommand::spawn_until(command, normal_deadline)
                .and_then(|running| running.wait(None).map_err(std::io::Error::other));
            let returned = match stop {
                Ok(outcome) if outcome.is_clean_exit(0) => true,
                // 后台可能在归属扫描后、连接前已由别处停止；not_running 只说明连接已无人接收，
                // 组是否排空仍由下方排空检查裁决。
                Ok(outcome) if already_stopped(&outcome) => false,
                Ok(outcome) => {
                    errors.push(format!("daemon stop: {}", outcome.summary()));
                    false
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    errors.push("scenario budget exhausted before normal daemon stop".into());
                    false
                }
                Err(_) => {
                    errors.push("daemon stop command failed".into());
                    false
                }
            };
            if Instant::now() >= normal_deadline {
                errors.push("scenario budget exhausted during normal daemon stop".into());
            }
            if returned {
                // stop 干净返回即承诺实例已放开：锁与 socket 立即核验，组退出只做有界等待，早返回的回归不会被收尾掩盖。
                match self.departed_with_inspection(inspect) {
                    Ok(()) => return finish(errors),
                    Err(found) => errors.extend(found),
                }
            }
            // 只等已识别的后台组，不把前台 EOF 当作排空；等待不超过剩余正常预算。
            let deadline = if returned {
                Instant::now()
            } else {
                (Instant::now() + STOP_DRAIN_WINDOW).min(normal_deadline)
            };
            while Instant::now() < deadline {
                let scanned: Result<Vec<_>, _> = groups.iter().map(|pgid| inspect(*pgid)).collect();
                let remaining = match scanned {
                    Ok(remaining) => remaining,
                    Err(error) => {
                        errors.push(format!("daemon drain inspection failed: {}", error.message));
                        return Err(errors);
                    }
                };
                if remaining.iter().all(Vec::is_empty) {
                    if Instant::now() >= normal_deadline {
                        errors.push("scenario budget exhausted during daemon drain".into());
                    }
                    return finish(errors);
                }
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
        for &pgid in &groups {
            match inspect(pgid) {
                Ok(members) if members.is_empty() => {}
                Ok(members) => match self.group_confirmed(pgid, &members) {
                    Ok(Some(captured)) => {
                        errors.push(format!(
                            "daemon required forced cleanup: pgid={pgid} members={members:?}"
                        ));
                        for member in &captured {
                            if member.kill().is_err() {
                                errors.push(format!("daemon SIGKILL failed: pgid={pgid} pid={}", member.pid));
                            }
                        }
                        self.confirmed.lock().expect("daemon confirmed lock").insert(pgid, captured);
                    }
                    Ok(None) => errors.push(format!(
                        "daemon group ownership unconfirmed, left untouched: pgid={pgid} members={members:?}"
                    )),
                    Err(_) => errors.push(format!("daemon ownership recheck failed: pgid={pgid}")),
                },
                Err(_) => errors.push(format!("daemon ownership recheck failed: pgid={pgid}")),
            }
        }
        let deadline = Instant::now() + FORCED_CLEANUP_GRACE;
        loop {
            // 组首领退出不证明其子进程已退出；复核最初确认过的每个进程组。
            let remaining: Result<Vec<_>, _> = groups
                .iter()
                .flat_map(|pgid| match inspect(*pgid) {
                    Ok(members) => members.into_iter().map(Ok).collect::<Vec<_>>(),
                    Err(error) => vec![Err(error)],
                })
                .collect();
            match remaining {
                Ok(members) if members.is_empty() => break,
                Ok(members) if Instant::now() >= deadline => {
                    errors.push(format!("daemon group survivors: {members:?}"));
                    break;
                }
                Err(_) => {
                    errors.push("daemon final group inspection failed".into());
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        finish(errors)
    }

    /// 新观察必须与仍在原组的旧实例相接；孤儿还须保留首领建立的独立会话。
    /// 所有旧实例均已退出时，即使 SID/PGID 数值吻合，也不能恢复归属。
    fn group_confirmed(
        &self,
        pgid: u32,
        members: &[u32],
    ) -> std::io::Result<Option<Vec<OwnedProcess>>> {
        crate::driver::reject_own_process_group(pgid)?;
        let captured = capture_members(pgid, members)?;
        let confirmed = self.confirmed.lock().expect("daemon confirmed lock");
        if let Some(previous) = confirmed.get(&pgid) {
            for witness in previous {
                // 在新快照之后核对旧实例存活，证明两次观察之间不是空组再复用。
                if witness.in_group(pgid, witness.pid != pgid)? {
                    return Ok(Some(captured));
                }
            }
        }
        Ok(None)
    }
}

/// 以非阻塞 flock 试探存储布局公开的实例锁并立即放开；锁文件不存在视为已释放。
fn owner_lock_held(state_root: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(state_root.join("lattice/local/owner.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // SAFETY: flock 只作用于本函数打开的 fd；关闭 fd 即释放试探取得的锁。
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err(error)
    }
}

fn finish(errors: Vec<String>) -> Result<(), Vec<String>> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// `daemon stop` 干净退出 1 且报告 `not_running`：后台已不再接收连接。
fn already_stopped(outcome: &crate::driver::CommandOutcome) -> bool {
    outcome.is_clean_exit(1)
        && serde_json::from_slice::<serde_json::Value>(&outcome.stdout)
            .is_ok_and(|line| line["type"] == "error" && line["kind"] == "not_running")
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Stdio};

    use super::*;
    use crate::driver::processes_in_pgid;

    /// 自有假后台：把 shell 复制成独立产物路径，以 `daemon serve --state-root` 形态运行脚本，
    /// 派生一个子进程后按触发文件退出首领。只涉及本测试创建的进程，不触碰陌生进程。
    struct FakeDaemon {
        _dir: tempfile::TempDir,
        binary: PathBuf,
        state_root: PathBuf,
        trigger: PathBuf,
        child: Child,
        members: Vec<OwnedProcess>,
    }

    impl FakeDaemon {
        fn spawn(own_session: bool) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let shell = std::fs::canonicalize("/bin/sh").expect("resolve shell");
            let binary = dir.path().join("fake-lattice");
            std::fs::copy(&shell, &binary).expect("copy shell as private product path");
            let binary = std::fs::canonicalize(&binary).expect("canonical fake binary");
            let state_root = dir.path().join("state");
            std::fs::create_dir_all(&state_root).expect("state root");
            let ready = dir.path().join("ready");
            let trigger = dir.path().join("trigger");
            std::fs::write(
                dir.path().join("daemon"),
                format!(
                    "sleep 30 &\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nexit 0\n",
                    ready.display(),
                    trigger.display()
                ),
            )
            .expect("script");
            let mut command = Command::new(&binary);
            command
                .args(["daemon", "serve", "--state-root"])
                .arg(&state_root)
                .current_dir(dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if own_session {
                // SAFETY: fork 后只调用 async-signal-safe 的 setsid，模拟产品后台的独立会话。
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            } else {
                command.process_group(0);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            // linux `execve` reports ETXTBSY while any process still holds a write handle to the
            // inode; a sibling test thread's `fork` inherits the `fs::copy` write descriptor into a
            // child that has not exec'd yet. Retry only that transient error, inside the same
            // startup budget; every other error stays fatal on the spot.
            let mut spawned = command.spawn();
            while let Err(error) = &spawned {
                if error.kind() != std::io::ErrorKind::ExecutableFileBusy
                    || Instant::now() >= deadline
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
                spawned = command.spawn();
            }
            let child = spawned.expect("spawn fake daemon");
            while !ready.exists() {
                assert!(Instant::now() < deadline, "fake daemon never became ready");
                std::thread::sleep(Duration::from_millis(5));
            }
            let members = capture_members(
                child.id(),
                &processes_in_pgid(child.id()).expect("fixture group"),
            )
            .expect("fixture instances");
            Self {
                _dir: dir,
                binary,
                state_root,
                trigger,
                child,
                members,
            }
        }

        fn pgid(&self) -> u32 {
            self.child.id()
        }

        fn exit_leader(&mut self) {
            std::fs::write(&self.trigger, b"exit").expect("trigger");
            let status = self.child.wait().expect("reap leader");
            assert!(status.success(), "leader exit: {status:?}");
        }
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            // 兜底同样绑定真实实例，不在断言失败后的长窗口向数值 PGID 广播。
            for member in &self.members {
                let _ = member.kill();
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn wait_group_empty(pgid: u32) -> Vec<u32> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let members = processes_in_pgid(pgid).expect("inspect group");
            if members.is_empty() || Instant::now() >= deadline {
                return members;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 首领已按产物、参数、状态根和组归属确认后退出，其后代仍在同一独立会话组中：
    /// 清理必须复核并结束该组，而不是因为找不到首领就只报告存活者。
    #[test]
    fn confirmed_group_is_cleaned_after_its_leader_exits() {
        let mut fake = FakeDaemon::spawn(true);
        let daemon = ScenarioDaemon::new(fake.state_root.clone());
        daemon.register(&fake.binary).expect("register");
        let pgid = fake.pgid();
        assert_eq!(daemon.owners().expect("owners"), vec![pgid]);

        fake.exit_leader();
        assert!(daemon.owners().expect("owners").is_empty());
        let orphans = processes_in_pgid(pgid).expect("inspect group");
        assert!(
            !orphans.is_empty() && !orphans.contains(&pgid),
            "fixture must leave a live descendant without its leader: {orphans:?}"
        );

        let errors = daemon
            .cleanup(Instant::now() + Duration::from_secs(30))
            .expect_err("a failed stop and a forced cleanup are reported, not hidden");
        assert!(
            errors.iter().any(|error| error.contains("forced cleanup")),
            "{errors:?}"
        );
        assert!(
            !errors.iter().any(|error| error.contains("survivors")),
            "{errors:?}"
        );
        let survivors = wait_group_empty(pgid);
        assert!(
            survivors.is_empty(),
            "group survived cleanup: {survivors:?}"
        );
    }

    /// 首领退出后剩余成员既不是登记产物也不在首领的独立会话中：没有归属证据时只报告，不发信号。
    #[test]
    fn unconfirmed_group_members_are_reported_not_killed() {
        let mut fake = FakeDaemon::spawn(false);
        let daemon = ScenarioDaemon::new(fake.state_root.clone());
        daemon.register(&fake.binary).expect("register");
        let pgid = fake.pgid();
        assert_eq!(daemon.owners().expect("owners"), vec![pgid]);

        fake.exit_leader();
        let orphans = processes_in_pgid(pgid).expect("inspect group");
        assert!(!orphans.is_empty() && !orphans.contains(&pgid));

        let errors = daemon
            .cleanup(Instant::now() + Duration::from_secs(30))
            .expect_err("survivors must be reported");
        assert!(
            errors.iter().any(|error| error.contains("survivors")),
            "{errors:?}"
        );
        assert!(
            !errors.iter().any(|error| error.contains("SIGKILL")),
            "{errors:?}"
        );
        assert_eq!(
            processes_in_pgid(pgid).expect("inspect group"),
            orphans,
            "unconfirmed members must still be alive"
        );
    }

    #[test]
    fn group_scan_error_preserves_ownership_and_reports_failure() {
        let mut fake = FakeDaemon::spawn(true);
        let daemon = ScenarioDaemon::new(fake.state_root.clone());
        daemon.register(&fake.binary).expect("register");
        let pgid = fake.pgid();
        assert_eq!(daemon.owners().expect("owners"), vec![pgid]);
        fake.exit_leader();
        let orphans = processes_in_pgid(pgid).expect("orphans");
        assert!(!orphans.is_empty());
        let mut fail_scan = |_| {
            Err(CleanupError {
                operation: crate::driver::CleanupOperation::InspectProcessGroup(
                    crate::driver::ProcessInspectionOperation::ReadProc,
                ),
                kind: std::io::ErrorKind::PermissionDenied,
                raw_os_error: Some(libc::EACCES),
                message: "controlled proc inspection failure".into(),
            })
        };
        let result = daemon.cleanup_with_inspection(Instant::now(), &mut fail_scan);
        assert!(
            result.is_err(),
            "inspection failure must not report successful cleanup: {result:?}"
        );
        assert!(
            daemon
                .confirmed
                .lock()
                .expect("confirmed")
                .contains_key(&pgid),
            "failed inspection lost ownership"
        );
        assert_eq!(
            processes_in_pgid(pgid).expect("survivors"),
            orphans,
            "inspection error must not authorize signals"
        );
        // 检查恢复后仍能沿旧实例归属完成安全收尾，错误不造成永久弃管。
        let errors = daemon
            .cleanup(Instant::now())
            .expect_err("forced cleanup reported");
        assert!(
            errors.iter().any(|error| error.contains("forced cleanup")),
            "{errors:?}"
        );
        assert!(wait_group_empty(pgid).is_empty());
        daemon.owners().expect("empty group observed");
        assert!(
            !daemon
                .confirmed
                .lock()
                .expect("confirmed")
                .contains_key(&pgid)
        );
    }

    #[test]
    fn stale_group_identity_does_not_kill_replacement_orphans() {
        let mut replacement = FakeDaemon::spawn(true);
        let daemon = ScenarioDaemon::new(replacement.state_root.clone());
        daemon.register(&replacement.binary).expect("register");
        let pgid = replacement.pgid();
        // 受控模拟：缓存来自已消失的一代；当前同值组的首领也已退出。
        // 实际组完全由本测试创建，不强迫系统复用 PID，也不向陌生进程发信号。
        let mut old = FakeDaemon::spawn(true);
        let mut old_instance = OwnedProcess::open(old.pgid()).expect("old instance");
        old.exit_leader();
        drop(old);
        replacement.exit_leader();
        let orphans = processes_in_pgid(pgid).expect("replacement group");
        assert!(!orphans.is_empty() && !orphans.contains(&pgid));
        // 连成员 PID 都视作同值复用：/proc 读到当前成员，但 pidfd 仍绑定已退出的旧实例。
        old_instance.pid = orphans[0];
        daemon
            .confirmed
            .lock()
            .expect("confirmed")
            .insert(pgid, vec![old_instance]);
        let errors = daemon
            .cleanup(Instant::now())
            .expect_err("identity unknown");
        assert_eq!(
            processes_in_pgid(pgid).expect("replacement survivors"),
            orphans,
            "a numeric PGID and matching SID must not authorize killing another generation: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("ownership unconfirmed")),
            "{errors:?}"
        );
    }

    #[test]
    fn group_scan_cannot_renew_normal_stop_budget() {
        let fake = FakeDaemon::spawn(true);
        let daemon = ScenarioDaemon::new(fake.state_root.clone());
        daemon.register(&fake.binary).expect("register");
        let deadline = Instant::now() + Duration::from_millis(100);
        let mut scanned = false;
        let mut inspect = |pgid| {
            if !scanned {
                assert!(
                    Instant::now() < deadline,
                    "scan must start with remaining budget"
                );
                scanned = true;
                // 受控系统边界耗时：当前扫描在原期限之后才返回真实组成员。
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }
            processes_in_pgid(pgid)
        };
        let errors = daemon
            .cleanup_with_inspection(deadline, &mut inspect)
            .expect_err("expired stop");
        assert!(scanned);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("budget exhausted before normal daemon stop")),
            "{errors:?}"
        );
        assert!(
            !errors.iter().any(|error| error.starts_with("daemon stop")),
            "{errors:?}"
        );
        assert!(wait_group_empty(fake.pgid()).is_empty());
    }

    /// 预算耗尽时正常 stop 不再执行，失败被记录；归属确认的自有组仍在独立宽限内被强制清理。
    #[test]
    fn inspection_lock_cannot_renew_normal_stop_budget() {
        let fake = FakeDaemon::spawn(true);
        let daemon = ScenarioDaemon::new(fake.state_root.clone());
        daemon.register(&fake.binary).expect("register");
        assert_eq!(daemon.owners().expect("owners"), vec![fake.pgid()]);
        let normal_stop = Duration::from_millis(100);
        let deadline = Instant::now() + normal_stop;
        let guard = daemon.binary.lock().expect("hold inspection lock");
        let errors = std::thread::scope(|scope| {
            let (entered, waiting) = std::sync::mpsc::sync_channel(0);
            let daemon = &daemon;
            let worker = scope.spawn(move || {
                entered.send(()).expect("announce cleanup");
                daemon.cleanup(deadline)
            });
            waiting
                .recv_timeout(Duration::from_secs(2))
                .expect("cleanup entered");
            // 确定性耗尽预算：扫描所需锁在原期限后才释放，不靠调度时序碰撞。
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            drop(guard);
            worker
                .join()
                .expect("cleanup thread")
                .expect_err("expired stop")
        });
        assert!(
            errors
                .iter()
                .any(|error| error.contains("budget exhausted before normal daemon stop")),
            "inspection must consume the original budget: {errors:?}"
        );
        assert!(
            !errors.iter().any(|error| error.starts_with("daemon stop")),
            "no normal command may start after inspection exhausts the budget: {errors:?}"
        );
        assert!(wait_group_empty(fake.pgid()).is_empty());
    }

    #[test]
    fn exhausted_normal_budget_is_recorded_before_forced_cleanup() {
        let fake = FakeDaemon::spawn(true);
        let daemon = ScenarioDaemon::new(fake.state_root.clone());
        daemon.register(&fake.binary).expect("register");
        let pgid = fake.pgid();
        assert_eq!(daemon.owners().expect("owners"), vec![pgid]);

        let started = Instant::now();
        let errors = daemon
            .cleanup(Instant::now())
            .expect_err("exhausted budget must be reported");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("budget exhausted before normal daemon stop")),
            "{errors:?}"
        );
        assert!(
            !errors.iter().any(|error| error.starts_with("daemon stop")),
            "no stop command may run without budget: {errors:?}"
        );
        assert!(
            errors.iter().any(|error| error.contains("forced cleanup")),
            "{errors:?}"
        );
        assert!(
            started.elapsed() < FORCED_CLEANUP_GRACE + Duration::from_secs(3),
            "cleanup waited {:?}",
            started.elapsed()
        );
        assert!(wait_group_empty(pgid).is_empty());
    }
}
