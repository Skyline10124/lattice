// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::driver::{RunningCommand, processes_in_pgid, signal_process_group};

/// 前台进程组不包含 setsid 后的后台；场景必须另持清理归属。
pub(crate) struct ScenarioDaemon {
    state_root: PathBuf,
    binary: Mutex<Option<PathBuf>>,
}

impl ScenarioDaemon {
    pub(crate) fn new(state_root: PathBuf) -> Self {
        Self {
            state_root,
            binary: Mutex::new(None),
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
            let args = match std::fs::read(entry.path().join("cmdline")) {
                Ok(args) => args,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
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
                // SAFETY: 只查询已经核对归属的进程组，不修改进程状态。
                if unsafe { libc::getpgid(pid as libc::pid_t) } == pid as libc::pid_t {
                    owners.push(pid);
                }
            }
        }
        owners.sort_unstable();
        Ok(owners)
    }

    pub(crate) fn cleanup(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let owners = self
            .owners()
            .map_err(|_| vec!["daemon ownership inspection failed".into()])?;
        if owners.is_empty() {
            return Ok(());
        }
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
        match RunningCommand::spawn(command).and_then(|running| {
            running
                .wait(Some(Duration::from_secs(30)))
                .map_err(std::io::Error::other)
        }) {
            Ok(outcome) if outcome.is_clean_exit(0) => {}
            Ok(outcome) => errors.push(format!("daemon stop: {}", outcome.summary())),
            Err(_) => errors.push("daemon stop command failed".into()),
        }
        // stop 回执早于 serve 退出；只等已识别的后台组，不把前台 EOF 当作排空。
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if owners
                .iter()
                .all(|pid| processes_in_pgid(*pid).is_ok_and(|members| members.is_empty()))
            {
                return if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors)
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for &pid in &owners {
            match self.owners() {
                Ok(current) if current.contains(&pid) => {
                    errors.push(format!("daemon required forced cleanup: pgid={pid}"));
                    if signal_process_group(pid, libc::SIGKILL).is_err() {
                        errors.push(format!("daemon SIGKILL failed: pgid={pid}"));
                    }
                }
                Ok(_) => {}
                Err(_) => errors.push("daemon ownership recheck failed".into()),
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // 组首领退出不证明其子进程已退出；复核最初确认过的每个进程组。
            let remaining: Result<Vec<_>, _> = owners
                .iter()
                .flat_map(|pid| match processes_in_pgid(*pid) {
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
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}
