// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Live supervised command process handoff.

#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use lancet_protocol::error::{CepErrorData, ExecutionErrorData, ValidationErrorData};
#[cfg(unix)]
use lancet_protocol::{CepError, CepErrorCode, Retriability};

#[cfg(unix)]
use super::linux::{boundary_attempt_from_supervisor, run_linux_supervisor_attempt};
#[cfg(unix)]
use super::setup_report::encode_setup_attempt_report;
#[cfg(unix)]
use super::syscall::{LiveSupervisorSyscalls, UsernsChildHandshake, UsernsMapStage};
#[cfg(unix)]
use super::SupervisorPlan;

#[cfg(unix)]
pub(crate) struct LiveSupervisorProcess {
    child: ManualChild,
    stdout: Option<tokio::fs::File>,
    stderr: Option<tokio::fs::File>,
    pub(crate) handshake_error: Option<io::Error>,
}

#[cfg(unix)]
impl LiveSupervisorProcess {
    pub(crate) fn take_stdout(&mut self) -> tokio::fs::File {
        self.stdout.take().expect("supervised stdout was taken")
    }

    pub(crate) fn take_stderr(&mut self) -> tokio::fs::File {
        self.stderr.take().expect("supervised stderr was taken")
    }

    pub(crate) async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    pub(crate) async fn kill_and_reap(&mut self) -> io::Result<()> {
        kill_and_reap_manual_child(&mut self.child).await
    }
}

#[cfg(unix)]
pub(crate) fn spawn_live_supervisor_process(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    plan: SupervisorPlan,
    setup_child_write: RawFd,
) -> Result<LiveSupervisorProcess, CepError> {
    let mut stdout_pipe = FdPipe::new("stdout")?;
    let mut stderr_pipe = FdPipe::new("stderr")?;
    let mut userns_request = FdPipe::new("userns_request")?;
    let mut userns_response = FdPipe::new("userns_response")?;

    let argv_cstrings = argv_cstrings(argv)?;
    let env_cstrings = env_cstrings(env)?;
    let argv_ptrs = cstring_ptrs(&argv_cstrings);
    let env_ptrs = cstring_ptrs(&env_cstrings);
    let cwd_cstring = CString::new(cwd.as_os_str().as_bytes()).map_err(|err| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("failed to prepare supervised command cwd: {err}"),
            CepErrorData::Execution(ExecutionErrorData {
                command: argv[0].clone(),
                exit_code: None,
                stderr: Some(err.to_string()),
            }),
            Retriability::Transient,
        )
    })?;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            format!(
                "failed to spawn supervised command process: {}",
                io::Error::last_os_error()
            ),
            CepErrorData::Execution(ExecutionErrorData {
                command: argv[0].clone(),
                exit_code: None,
                stderr: Some(io::Error::last_os_error().to_string()),
            }),
            Retriability::Transient,
        ));
    }

    if pid == 0 {
        child_supervisor_main(
            &argv_ptrs,
            &env_ptrs,
            &cwd_cstring,
            plan,
            stdout_pipe.read,
            stdout_pipe.write,
            stderr_pipe.read,
            stderr_pipe.write,
            setup_child_write,
            userns_request.read,
            userns_request.write,
            userns_response.read,
            userns_response.write,
        );
    }

    let child = ManualChild::new(pid);
    stdout_pipe.close_write();
    stderr_pipe.close_write();
    userns_request.close_write();
    userns_response.close_read();

    let stdout = stdout_pipe.take_read_file()?;
    let stderr = stderr_pipe.take_read_file()?;
    drop(argv_ptrs);
    drop(env_ptrs);
    drop(argv_cstrings);
    drop(env_cstrings);
    drop(cwd_cstring);

    let handshake_error = run_userns_parent_handshake(
        pid,
        userns_request.read,
        userns_response.write,
        unsafe { libc::getuid() },
        unsafe { libc::getgid() },
    )
    .err();
    userns_request.close_read();
    userns_response.close_write();

    Ok(LiveSupervisorProcess {
        child,
        stdout: Some(stdout),
        stderr: Some(stderr),
        handshake_error,
    })
}

#[cfg(unix)]
struct FdPipe {
    read: RawFd,
    write: RawFd,
}

#[cfg(unix)]
impl FdPipe {
    fn new(label: &str) -> Result<Self, CepError> {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        if rc == 0 {
            Ok(Self {
                read: fds[0],
                write: fds[1],
            })
        } else {
            Err(CepError::new(
                CepErrorCode::ExecutionFailed,
                format!(
                    "failed to create supervised command {label} pipe: {}",
                    io::Error::last_os_error()
                ),
                CepErrorData::Execution(ExecutionErrorData {
                    command: "supervisor_setup".to_string(),
                    exit_code: None,
                    stderr: Some(io::Error::last_os_error().to_string()),
                }),
                Retriability::Transient,
            ))
        }
    }

    fn close_read(&mut self) {
        close_fd_slot(&mut self.read);
    }

    fn close_write(&mut self) {
        close_fd_slot(&mut self.write);
    }

    fn take_read_file(&mut self) -> Result<tokio::fs::File, CepError> {
        if self.read < 0 {
            return Err(CepError::new(
                CepErrorCode::ExecutionFailed,
                "supervised command pipe read fd already closed",
                CepErrorData::Execution(ExecutionErrorData {
                    command: "supervisor_setup".to_string(),
                    exit_code: None,
                    stderr: Some("pipe read fd already closed".to_string()),
                }),
                Retriability::Transient,
            ));
        }
        let fd = self.read;
        self.read = -1;
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        Ok(tokio::fs::File::from_std(file))
    }
}

#[cfg(unix)]
impl Drop for FdPipe {
    fn drop(&mut self) {
        self.close_read();
        self.close_write();
    }
}

#[cfg(unix)]
fn close_fd_slot(fd: &mut RawFd) {
    if *fd >= 0 {
        close_raw_fd(*fd);
        *fd = -1;
    }
}

#[cfg(unix)]
fn close_raw_fd(fd: RawFd) {
    if fd >= 0 {
        unsafe {
            libc::close(fd);
        }
    }
}

#[cfg(unix)]
fn argv_cstrings(argv: &[String]) -> Result<Vec<CString>, CepError> {
    argv.iter()
        .map(|arg| {
            CString::new(arg.as_str()).map_err(|err| {
                CepError::new(
                    CepErrorCode::SchemaMismatch,
                    format!("exec.command_run argv contains an invalid NUL byte: {err}"),
                    CepErrorData::Validation(ValidationErrorData {
                        field: "argv".into(),
                        reason: "contains NUL byte".into(),
                    }),
                    Retriability::Permanent,
                )
            })
        })
        .collect()
}

#[cfg(unix)]
fn env_cstrings(env: &BTreeMap<String, String>) -> Result<Vec<CString>, CepError> {
    env.iter()
        .map(|(key, value)| {
            CString::new(format!("{key}={value}")).map_err(|err| {
                CepError::new(
                    CepErrorCode::SchemaMismatch,
                    format!("exec.command_run env contains an invalid NUL byte: {err}"),
                    CepErrorData::Validation(ValidationErrorData {
                        field: format!("env.{key}"),
                        reason: "contains NUL byte".into(),
                    }),
                    Retriability::Permanent,
                )
            })
        })
        .collect()
}

#[cfg(unix)]
fn cstring_ptrs(values: &[CString]) -> Vec<*const libc::c_char> {
    let mut out: Vec<_> = values.iter().map(|value| value.as_ptr()).collect();
    out.push(std::ptr::null());
    out
}

#[cfg(unix)]
struct ManualChild {
    pid: libc::pid_t,
    waited: bool,
}

#[cfg(unix)]
impl ManualChild {
    fn new(pid: libc::pid_t) -> Self {
        Self { pid, waited: false }
    }

    async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        if self.waited {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "manual child was already waited",
            ));
        }
        let pid = self.pid;
        let status = tokio::task::spawn_blocking(move || waitpid_blocking(pid))
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))??;
        self.waited = true;
        Ok(status)
    }
}

#[cfg(unix)]
fn waitpid_blocking(pid: libc::pid_t) -> io::Result<std::process::ExitStatus> {
    let mut status = 0;
    loop {
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc == pid {
            return Ok(std::process::ExitStatus::from_raw(status));
        }
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        }
    }
}

#[cfg(unix)]
async fn kill_and_reap_manual_child(child: &mut ManualChild) -> io::Result<()> {
    if !child.waited {
        let rc = unsafe { libc::kill(-child.pid, libc::SIGKILL) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err);
            }
        }
    }
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) if err.raw_os_error() == Some(libc::ECHILD) => Ok(()),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out waiting for killed child to exit",
        )),
    }
}

#[cfg(unix)]
fn child_supervisor_main(
    argv_ptrs: &[*const libc::c_char],
    env_ptrs: &[*const libc::c_char],
    cwd: &CString,
    plan: SupervisorPlan,
    stdout_read: RawFd,
    stdout_write: RawFd,
    stderr_read: RawFd,
    stderr_write: RawFd,
    setup_child_write: RawFd,
    userns_request_read: RawFd,
    userns_request_write: RawFd,
    userns_response_read: RawFd,
    userns_response_write: RawFd,
) -> ! {
    unsafe {
        libc::close(stdout_read);
        libc::close(stderr_read);
        libc::close(userns_request_read);
        libc::close(userns_response_write);
        libc::setpgid(0, 0);
        if libc::dup2(stdout_write, libc::STDOUT_FILENO) < 0 {
            libc::_exit(127);
        }
        if libc::dup2(stderr_write, libc::STDERR_FILENO) < 0 {
            libc::_exit(127);
        }
        if stdout_write > libc::STDERR_FILENO {
            libc::close(stdout_write);
        }
        if stderr_write > libc::STDERR_FILENO {
            libc::close(stderr_write);
        }
        if libc::chdir(cwd.as_ptr()) != 0 {
            libc::_exit(127);
        }
    }

    let userns_handshake = UsernsChildHandshake::new(userns_request_write, userns_response_read);
    let attempt = run_linux_supervisor_attempt(
        plan,
        LiveSupervisorSyscalls::with_preserve_fds_and_userns_handshake(
            vec![setup_child_write],
            userns_handshake,
        ),
    );
    let boundary = boundary_attempt_from_supervisor(&attempt);
    let report = encode_setup_attempt_report(&boundary);
    let _ = raw_write_all(setup_child_write, report.as_bytes());
    close_raw_fd(setup_child_write);
    if boundary.outcome.failed_closed {
        unsafe {
            libc::_exit(126);
        }
    }

    unsafe {
        libc::execve(argv_ptrs[0], argv_ptrs.as_ptr(), env_ptrs.as_ptr());
        libc::_exit(127);
    }
}

#[cfg(unix)]
fn run_userns_parent_handshake(
    pid: libc::pid_t,
    request_read: RawFd,
    response_write: RawFd,
    uid: libc::uid_t,
    gid: libc::gid_t,
) -> io::Result<()> {
    let mut saw_gid_map = false;
    while !saw_gid_map {
        let mut byte = [0u8; 1];
        match read_exact_raw(request_read, &mut byte) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(err) => return Err(err),
        }
        let Some(stage) = UsernsMapStage::from_request_byte(byte[0]) else {
            raw_write_all(response_write, b"0")?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown userns map request",
            ));
        };
        let result = write_child_userns_map(pid, stage, uid, gid);
        if result.is_ok() {
            raw_write_all(response_write, b"1")?;
            saw_gid_map = stage == UsernsMapStage::GidMap;
        } else {
            let _ = raw_write_all(response_write, b"0");
            return result;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_child_userns_map(
    pid: libc::pid_t,
    stage: UsernsMapStage,
    uid: libc::uid_t,
    gid: libc::gid_t,
) -> io::Result<()> {
    match stage {
        UsernsMapStage::UserNamespaceReady => Ok(()),
        UsernsMapStage::SetgroupsDeny => {
            std::fs::write(format!("/proc/{pid}/setgroups"), b"deny\n")
        }
        UsernsMapStage::UidMap => {
            std::fs::write(format!("/proc/{pid}/uid_map"), format!("0 {uid} 1\n"))
        }
        UsernsMapStage::GidMap => {
            std::fs::write(format!("/proc/{pid}/gid_map"), format!("0 {gid} 1\n"))
        }
    }
}

#[cfg(unix)]
fn read_exact_raw(fd: RawFd, mut bytes: &mut [u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let read =
            unsafe { libc::read(fd, bytes.as_mut_ptr().cast::<libc::c_void>(), bytes.len()) };
        if read < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        }
        if read == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "pipe closed"));
        }
        let (_, rest) = bytes.split_at_mut(read as usize);
        bytes = rest;
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn raw_write_all(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let written =
            unsafe { libc::write(fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len()) };
        if written < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        }
        if written == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short pipe write"));
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}
