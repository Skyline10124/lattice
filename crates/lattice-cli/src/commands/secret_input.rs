// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 终端无回显行读取与限量管道读取（供 `lattice auth` 与 `lattice im auth` 共用）。

use std::{
    cell::UnsafeCell,
    io::{self, IsTerminal, Read, Write},
    mem::MaybeUninit,
    os::fd::AsRawFd,
    ptr,
    sync::atomic::{AtomicU8, Ordering},
};

use lattice_local::{LocalError, LocalErrorKind};
use zeroize::Zeroizing;

/// 读取预算保留 CRLF 和一个超限判定字节，具体长度与格式规则由后台统一校验。
pub(crate) const MAX_SECRET_INPUT_BYTES: usize = 16 * 1024 + 3;

const SIGNALS: [i32; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

const STATE_IDLE: u8 = 0;
const STATE_ACTIVE: u8 = 1;
const STATE_TRANSITION: u8 = 2;

#[derive(Clone, Copy)]
struct SavedTerminal {
    fd: i32,
    settings: libc::termios,
}

struct SavedTerminalSlot(UnsafeCell<MaybeUninit<SavedTerminal>>);

// SAFETY: 写入仅在 `TERMINAL_STATE` 从 `IDLE` 原子切换至 `TRANSITION` 后、安装信号处理函数前发生；
// 信号处理函数仅在 `TERMINAL_STATE == STATE_ACTIVE` 期间只读访问 `SAVED`；
// 恢复阶段先在 `STATE_ACTIVE` 下恢复终端属性（`tcsetattr`），再置 `TRANSITION`、卸载信号处理函数，最后置回 `IDLE`。
unsafe impl Sync for SavedTerminalSlot {}

static TERMINAL_STATE: AtomicU8 = AtomicU8::new(STATE_IDLE);
static SAVED: SavedTerminalSlot = SavedTerminalSlot(UnsafeCell::new(MaybeUninit::uninit()));

extern "C" fn restore_on_signal(signal: i32) {
    // 信号路径仅使用异步信号安全的原子读取与系统调用恢复终端回显。
    if TERMINAL_STATE.load(Ordering::Acquire) == STATE_ACTIVE {
        unsafe {
            let saved = (*SAVED.0.get()).assume_init_ref();
            libc::tcsetattr(saved.fd, libc::TCSANOW, &saved.settings);
        }
    }
    // 恢复默认信号动作并解除当前信号屏蔽，重新触发使进程按原始信号终止。
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, ptr::null_mut());
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, signal);
        libc::sigprocmask(libc::SIG_UNBLOCK, &mask, ptr::null_mut());
        libc::raise(signal);
        libc::_exit(128 + signal);
    }
}

/// 终端无回显守卫：保留规范行编辑（`ICANON`），仅关闭 `ECHO | ECHONL`，并在返回、`Drop` 或终止信号时恢复。
pub(crate) struct TerminalInput {
    fd: i32,
    settings: libc::termios,
    previous: Vec<(i32, libc::sigaction)>,
    active: bool,
}

impl TerminalInput {
    pub(crate) fn new(fd: i32) -> io::Result<Self> {
        let mut settings = MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(fd, settings.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let settings = unsafe { settings.assume_init() };

        TERMINAL_STATE
            .compare_exchange(
                STATE_IDLE,
                STATE_TRANSITION,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| io::Error::other("terminal input is already active"))?;

        unsafe {
            (*SAVED.0.get()).write(SavedTerminal { fd, settings });
        }
        TERMINAL_STATE.store(STATE_ACTIVE, Ordering::Release);

        let mut terminal = Self {
            fd,
            settings,
            previous: Vec::with_capacity(SIGNALS.len()),
            active: true,
        };

        for signal in SIGNALS {
            let mut previous = MaybeUninit::uninit();
            if unsafe { libc::sigaction(signal, ptr::null(), previous.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let previous = unsafe { previous.assume_init() };
            if previous.sa_sigaction == libc::SIG_IGN {
                continue;
            }
            // 先装好恢复处理函数再关闭回显；处理期间屏蔽三类终止信号避免同线程重入。
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = restore_on_signal as *const () as usize;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            for blocked in SIGNALS {
                unsafe {
                    libc::sigaddset(&mut action.sa_mask, blocked);
                }
            }
            if unsafe { libc::sigaction(signal, &action, ptr::null_mut()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            terminal.previous.push((signal, previous));
        }

        let mut hidden = settings;
        hidden.c_lflag &= !(libc::ECHO | libc::ECHONL);
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(terminal)
    }

    pub(crate) fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }

        let mut failure = None;
        // 保持信号恢复处理与 `STATE_ACTIVE` 有效，先恢复终端回显，消除卸载期间收到信号留下无回显终端的窗口。
        if unsafe { libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.settings) } != 0 {
            failure = Some(io::Error::last_os_error());
        }

        TERMINAL_STATE.store(STATE_TRANSITION, Ordering::Release);
        while let Some((signal, previous)) = self.previous.pop() {
            if unsafe { libc::sigaction(signal, &previous, ptr::null_mut()) } != 0
                && failure.is_none()
            {
                failure = Some(io::Error::last_os_error());
            }
        }

        self.active = false;
        TERMINAL_STATE.store(STATE_IDLE, Ordering::Release);
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// 秘密读取错误（区分底层 I/O 失败与 UTF-8 校验拒绝）。
#[derive(Debug)]
pub(crate) enum SecretReadError {
    Io(io::Error),
    InvalidUtf8(LocalError),
}

impl From<io::Error> for SecretReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl SecretReadError {
    pub(crate) fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Io(error) => error.into(),
            Self::InvalidUtf8(error) => error.into(),
        }
    }
}

/// 仅剥离末尾至多一个 `\n` 或 `\r\n`，不自动裁剪空白字符，并校验 UTF-8。
pub(crate) fn finish_secret_bytes(
    mut bytes: Zeroizing<Vec<u8>>,
    invalid_utf8_message: &str,
) -> Result<String, LocalError> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    let secret = std::str::from_utf8(&bytes)
        .map_err(|_| LocalError::new(LocalErrorKind::BadRequest, invalid_utf8_message))?;
    Ok(secret.to_owned())
}

/// 从终端流按单字节读取至多一行（受 `MAX_SECRET_INPUT_BYTES` 限制）。
pub(crate) fn read_terminal_line(
    input: &mut impl Read,
    invalid_utf8_message: &str,
) -> Result<String, SecretReadError> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_SECRET_INPUT_BYTES + 1));
    let mut limited = input.take(MAX_SECRET_INPUT_BYTES as u64);
    loop {
        let mut byte = [0u8; 1];
        if limited.read(&mut byte)? == 0 {
            return finish_secret_bytes(bytes, invalid_utf8_message)
                .map_err(SecretReadError::InvalidUtf8);
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            return finish_secret_bytes(bytes, invalid_utf8_message)
                .map_err(SecretReadError::InvalidUtf8);
        }
    }
}

/// 从非终端管道限量读取凭据文本。
pub(crate) fn read_pipe_secret(
    input: &mut impl Read,
    invalid_utf8_message: &str,
) -> Result<String, SecretReadError> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_SECRET_INPUT_BYTES + 1));
    input
        .take(MAX_SECRET_INPUT_BYTES as u64)
        .read_to_end(&mut bytes)?;
    finish_secret_bytes(bytes, invalid_utf8_message).map_err(SecretReadError::InvalidUtf8)
}

/// 根据标准输入是否为终端自动选择无回显行录入或限量管道读取。
pub(crate) fn read_secret_from_stdin(
    terminal_prompt: Option<&str>,
    invalid_utf8_message: &str,
) -> Result<String, SecretReadError> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    if !stdin.is_terminal() {
        return read_pipe_secret(&mut input, invalid_utf8_message);
    }
    let mut terminal = TerminalInput::new(input.as_raw_fd())?;
    if let Some(prompt) = terminal_prompt {
        eprint!("{prompt}");
        io::stderr().flush()?;
    }
    let result = read_terminal_line(&mut input, invalid_utf8_message);
    terminal.restore()?;
    if terminal_prompt.is_some() {
        eprintln!();
    }
    result
}
