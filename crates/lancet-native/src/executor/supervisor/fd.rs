// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Inherited file-descriptor cleanup for supervised command exec.

use std::collections::BTreeSet;
use std::io;
use std::os::fd::RawFd;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FdCleanupMethod {
    CloseRange,
    RlimitLoop,
    ProcFdLoop,
    None,
}

impl FdCleanupMethod {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            FdCleanupMethod::CloseRange => "close_range",
            FdCleanupMethod::RlimitLoop => "rlimit_loop",
            FdCleanupMethod::ProcFdLoop => "proc_fd_loop",
            FdCleanupMethod::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FdCleanupStatus {
    Applied,
    Failed,
    NotRequested,
}

impl FdCleanupStatus {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            FdCleanupStatus::Applied => "applied",
            FdCleanupStatus::Failed => "failed",
            FdCleanupStatus::NotRequested => "not_requested",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FdCleanupReport {
    pub(crate) status: FdCleanupStatus,
    pub(crate) method: FdCleanupMethod,
    pub(crate) closed_fds: Vec<RawFd>,
    pub(crate) preserved_fds: Vec<RawFd>,
    pub(crate) error: Option<String>,
}

impl FdCleanupReport {
    fn applied(method: FdCleanupMethod, closed_fds: Vec<RawFd>, preserved_fds: Vec<RawFd>) -> Self {
        Self {
            status: FdCleanupStatus::Applied,
            method,
            closed_fds,
            preserved_fds,
            error: None,
        }
    }

    fn failed(error: impl Into<String>, preserved_fds: Vec<RawFd>) -> Self {
        Self {
            status: FdCleanupStatus::Failed,
            method: FdCleanupMethod::None,
            closed_fds: Vec::new(),
            preserved_fds,
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FdCleanupError {
    Unavailable(String),
    Failed(String),
}

impl std::fmt::Display for FdCleanupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FdCleanupError::Unavailable(reason) | FdCleanupError::Failed(reason) => {
                f.write_str(reason)
            }
        }
    }
}

pub(crate) trait FdCleanupBackend {
    fn close_range_from(&mut self, first_fd: RawFd) -> Result<(), FdCleanupError>;
    fn rlimit_nofile(&mut self) -> Result<RawFd, FdCleanupError>;
    fn close_fd(&mut self, fd: RawFd) -> Result<(), FdCleanupError>;
    fn proc_self_fds(&mut self) -> Result<Vec<RawFd>, FdCleanupError>;
}

pub(crate) fn cleanup_inherited_fds<B: FdCleanupBackend>(
    backend: &mut B,
    preserve_fds: &[RawFd],
) -> FdCleanupReport {
    let preserve = preserved_set(preserve_fds);
    let mut preserved: Vec<_> = preserve.iter().copied().collect();
    preserved.sort_unstable();

    if preserve.iter().all(|fd| *fd <= 2) && backend.close_range_from(3).is_ok() {
        return FdCleanupReport::applied(FdCleanupMethod::CloseRange, Vec::new(), preserved);
    }

    match cleanup_with_rlimit_loop(backend, &preserve) {
        Ok(closed) => {
            return FdCleanupReport::applied(FdCleanupMethod::RlimitLoop, closed, preserved)
        }
        Err(_err) => {}
    }

    match cleanup_with_proc_fd_loop(backend, &preserve) {
        Ok(closed) => FdCleanupReport::applied(FdCleanupMethod::ProcFdLoop, closed, preserved),
        Err(err) => FdCleanupReport::failed(err.to_string(), preserved),
    }
}

fn cleanup_with_rlimit_loop<B: FdCleanupBackend>(
    backend: &mut B,
    preserve: &BTreeSet<RawFd>,
) -> Result<Vec<RawFd>, FdCleanupError> {
    let limit = backend.rlimit_nofile()?;
    let mut closed = Vec::new();
    for fd in 3..limit {
        if preserve.contains(&fd) {
            continue;
        }
        backend.close_fd(fd)?;
        closed.push(fd);
    }
    Ok(closed)
}

fn cleanup_with_proc_fd_loop<B: FdCleanupBackend>(
    backend: &mut B,
    preserve: &BTreeSet<RawFd>,
) -> Result<Vec<RawFd>, FdCleanupError> {
    let mut closed = Vec::new();
    for fd in backend.proc_self_fds()? {
        if fd <= 2 || preserve.contains(&fd) {
            continue;
        }
        backend.close_fd(fd)?;
        closed.push(fd);
    }
    Ok(closed)
}

fn preserved_set(preserve_fds: &[RawFd]) -> BTreeSet<RawFd> {
    let mut preserve = BTreeSet::from([0, 1, 2]);
    preserve.extend(preserve_fds.iter().copied().filter(|fd| *fd >= 0));
    preserve
}

pub(crate) struct LiveFdCleanupBackend;

impl FdCleanupBackend for LiveFdCleanupBackend {
    fn close_range_from(&mut self, first_fd: RawFd) -> Result<(), FdCleanupError> {
        live_close_range(first_fd)
    }

    fn rlimit_nofile(&mut self) -> Result<RawFd, FdCleanupError> {
        live_rlimit_nofile()
    }

    fn close_fd(&mut self, fd: RawFd) -> Result<(), FdCleanupError> {
        live_close_fd(fd)
    }

    fn proc_self_fds(&mut self) -> Result<Vec<RawFd>, FdCleanupError> {
        live_proc_self_fds()
    }
}

#[cfg(target_os = "linux")]
fn live_close_range(first_fd: RawFd) -> Result<(), FdCleanupError> {
    let rc = unsafe { libc::syscall(libc::SYS_close_range, first_fd as u32, u32::MAX, 0u32) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ENOSYS) | Some(libc::EINVAL) => Err(FdCleanupError::Unavailable(format!(
            "close_range unavailable: {err}"
        ))),
        _ => Err(FdCleanupError::Failed(format!("close_range failed: {err}"))),
    }
}

#[cfg(not(target_os = "linux"))]
fn live_close_range(_first_fd: RawFd) -> Result<(), FdCleanupError> {
    Err(FdCleanupError::Unavailable(
        "close_range unsupported on this platform".to_string(),
    ))
}

fn live_rlimit_nofile() -> Result<RawFd, FdCleanupError> {
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) };
    if rc != 0 {
        return Err(FdCleanupError::Unavailable(format!(
            "getrlimit(RLIMIT_NOFILE) failed: {}",
            io::Error::last_os_error()
        )));
    }
    let limit = unsafe { limit.assume_init() };
    let soft = if limit.rlim_cur == libc::RLIM_INFINITY {
        1024 * 1024
    } else {
        limit.rlim_cur
    };
    i64::try_from(soft)
        .ok()
        .and_then(|value| RawFd::try_from(value).ok())
        .ok_or_else(|| FdCleanupError::Unavailable("RLIMIT_NOFILE exceeds fd range".to_string()))
}

fn live_close_fd(fd: RawFd) -> Result<(), FdCleanupError> {
    let rc = unsafe { libc::close(fd) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EBADF) {
        Ok(())
    } else {
        Err(FdCleanupError::Failed(format!("close({fd}) failed: {err}")))
    }
}

fn live_proc_self_fds() -> Result<Vec<RawFd>, FdCleanupError> {
    let entries = std::fs::read_dir("/proc/self/fd")
        .map_err(|err| FdCleanupError::Unavailable(format!("/proc/self/fd unavailable: {err}")))?;
    let mut fds = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            FdCleanupError::Unavailable(format!("/proc/self/fd entry failed: {err}"))
        })?;
        if let Some(name) = entry.file_name().to_str() {
            if let Ok(fd) = name.parse::<RawFd>() {
                fds.push(fd);
            }
        }
    }
    Ok(fds)
}

pub(crate) fn set_cloexec(fd: RawFd) -> Result<(), FdCleanupError> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(FdCleanupError::Failed(format!(
            "fcntl(F_GETFD) failed for fd {fd}: {}",
            io::Error::last_os_error()
        )));
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if rc == 0 {
        Ok(())
    } else {
        Err(FdCleanupError::Failed(format!(
            "fcntl(F_SETFD) failed for fd {fd}: {}",
            io::Error::last_os_error()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::os::fd::RawFd;

    struct FakeFdBackend {
        close_range: Result<(), FdCleanupError>,
        rlimit: Result<RawFd, FdCleanupError>,
        proc_fds: Result<Vec<RawFd>, FdCleanupError>,
        close_fails: bool,
        closed: Vec<RawFd>,
    }

    impl Default for FakeFdBackend {
        fn default() -> Self {
            Self {
                close_range: Ok(()),
                rlimit: Ok(8),
                proc_fds: Ok(vec![0, 1, 2, 3, 4, 5]),
                close_fails: false,
                closed: Vec::new(),
            }
        }
    }

    impl FakeFdBackend {
        fn close_range_unavailable(mut self) -> Self {
            self.close_range = Err(FdCleanupError::Unavailable("no close_range".into()));
            self
        }

        fn rlimit_unavailable(mut self) -> Self {
            self.rlimit = Err(FdCleanupError::Unavailable("no rlimit".into()));
            self
        }

        fn proc_unavailable(mut self) -> Self {
            self.proc_fds = Err(FdCleanupError::Unavailable("no proc fd".into()));
            self
        }

        fn close_fails(mut self) -> Self {
            self.close_fails = true;
            self
        }
    }

    impl FdCleanupBackend for FakeFdBackend {
        fn close_range_from(&mut self, _first_fd: RawFd) -> Result<(), FdCleanupError> {
            self.close_range.clone()
        }

        fn rlimit_nofile(&mut self) -> Result<RawFd, FdCleanupError> {
            self.rlimit.clone()
        }

        fn close_fd(&mut self, fd: RawFd) -> Result<(), FdCleanupError> {
            if self.close_fails {
                Err(FdCleanupError::Failed(format!("close {fd} failed")))
            } else {
                self.closed.push(fd);
                Ok(())
            }
        }

        fn proc_self_fds(&mut self) -> Result<Vec<RawFd>, FdCleanupError> {
            self.proc_fds.clone()
        }
    }

    #[test]
    fn fd_cleanup_fake_prefers_close_range_success() {
        let mut backend = FakeFdBackend::default();

        let report = cleanup_inherited_fds(&mut backend, &[]);

        assert_eq!(report.status, FdCleanupStatus::Applied);
        assert_eq!(report.method, FdCleanupMethod::CloseRange);
        assert!(backend.closed.is_empty());
    }

    #[test]
    fn fd_cleanup_fake_falls_back_to_rlimit_loop_when_close_range_unavailable() {
        let mut backend = FakeFdBackend::default().close_range_unavailable();

        let report = cleanup_inherited_fds(&mut backend, &[]);

        assert_eq!(report.status, FdCleanupStatus::Applied);
        assert_eq!(report.method, FdCleanupMethod::RlimitLoop);
        assert_eq!(backend.closed, vec![3, 4, 5, 6, 7]);
    }

    #[test]
    fn fd_cleanup_fake_uses_proc_fd_loop_after_rlimit_failure() {
        let mut backend = FakeFdBackend::default()
            .close_range_unavailable()
            .rlimit_unavailable();

        let report = cleanup_inherited_fds(&mut backend, &[4]);

        assert_eq!(report.status, FdCleanupStatus::Applied);
        assert_eq!(report.method, FdCleanupMethod::ProcFdLoop);
        assert_eq!(backend.closed, vec![3, 5]);
        assert!(report.preserved_fds.contains(&4));
    }

    #[test]
    fn fd_cleanup_fake_reports_failed_when_all_methods_fail() {
        let mut backend = FakeFdBackend::default()
            .close_range_unavailable()
            .rlimit_unavailable()
            .proc_unavailable()
            .close_fails();

        let report = cleanup_inherited_fds(&mut backend, &[]);

        assert_eq!(report.status, FdCleanupStatus::Failed);
        assert_eq!(report.method, FdCleanupMethod::None);
        assert!(report
            .error
            .as_deref()
            .is_some_and(|err| err.contains("no proc fd")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fd_cleanup_child_closes_non_cloexec_fd_and_report_pipe_before_final_exec() {
        let mut leak_pipe = [0; 2];
        let mut report_pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(leak_pipe.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::pipe(report_pipe.as_mut_ptr()) }, 0);

        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());
        if pid == 0 {
            unsafe {
                libc::close(report_pipe[0]);
                libc::close(leak_pipe[1]);
            }
            let mut backend = LiveFdCleanupBackend;
            let report = cleanup_inherited_fds(&mut backend, &[report_pipe[1]]);
            if report.status != FdCleanupStatus::Applied {
                unsafe { libc::_exit(80) };
            }
            let byte = [b'R'];
            unsafe {
                libc::write(report_pipe[1], byte.as_ptr().cast(), 1);
            }
            set_cloexec(report_pipe[1]).unwrap();
            let script = CString::new(format!(
                "test ! -e /proc/self/fd/{} && test ! -e /proc/self/fd/{}",
                leak_pipe[0], report_pipe[1]
            ))
            .unwrap();
            exec_sh(&script);
        }

        unsafe {
            libc::close(report_pipe[1]);
            libc::close(leak_pipe[0]);
            libc::close(leak_pipe[1]);
        }
        let mut byte = [0u8; 1];
        let read = unsafe { libc::read(report_pipe[0], byte.as_mut_ptr().cast(), 1) };
        unsafe {
            libc::close(report_pipe[0]);
        }
        assert_eq!(read, 1);
        assert_eq!(byte[0], b'R');
        assert_wait_success(pid);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fd_cleanup_setup_report_pipe_survives_partial_setup_failure() {
        let mut report_pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(report_pipe.as_mut_ptr()) }, 0);

        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());
        if pid == 0 {
            unsafe {
                libc::close(report_pipe[0]);
            }
            let mut backend = LiveFdCleanupBackend;
            let report = cleanup_inherited_fds(&mut backend, &[report_pipe[1]]);
            if report.status != FdCleanupStatus::Applied {
                unsafe { libc::_exit(80) };
            }
            let byte = [b'E'];
            unsafe {
                libc::write(report_pipe[1], byte.as_ptr().cast(), 1);
                libc::_exit(42);
            }
        }

        unsafe {
            libc::close(report_pipe[1]);
        }
        let mut byte = [0u8; 1];
        let read = unsafe { libc::read(report_pipe[0], byte.as_mut_ptr().cast(), 1) };
        unsafe {
            libc::close(report_pipe[0]);
        }
        assert_eq!(read, 1);
        assert_eq!(byte[0], b'E');
        assert_wait_code(pid, 42);
    }

    #[cfg(target_os = "linux")]
    fn exec_sh(script: &CString) -> ! {
        let sh = CString::new("/bin/sh").unwrap();
        let arg0 = CString::new("sh").unwrap();
        let arg1 = CString::new("-c").unwrap();
        unsafe {
            libc::execl(
                sh.as_ptr(),
                arg0.as_ptr(),
                arg1.as_ptr(),
                script.as_ptr(),
                std::ptr::null::<libc::c_char>(),
            );
            libc::_exit(127);
        }
    }

    #[cfg(target_os = "linux")]
    fn assert_wait_success(pid: libc::pid_t) {
        assert_wait_code(pid, 0);
    }

    #[cfg(target_os = "linux")]
    fn assert_wait_code(pid: libc::pid_t, expected: i32) {
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), expected);
    }
}
