// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{io, ptr, sync::OnceLock};

const SIGNALS: [i32; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];
static SAVED: OnceLock<SavedTerminal> = OnceLock::new();

struct SavedTerminal {
    fd: i32,
    settings: libc::termios,
}

extern "C" fn restore_on_signal(signal: i32) {
    // 快照保留到进程结束，信号路径用异步信号安全的接口恢复终端。
    if let Some(saved) = SAVED.get() {
        unsafe {
            libc::tcsetattr(saved.fd, libc::TCSANOW, &saved.settings);
        }
    }
    // 恢复默认动作并解除本信号的屏蔽，重新发送后按原信号结束进程。
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

/// 处理函数仅在录入期间安装；终端快照保留到进程结束。
pub(super) struct TerminalInput {
    saved: &'static SavedTerminal,
    previous: Vec<(i32, libc::sigaction)>,
    active: bool,
}

impl TerminalInput {
    pub(super) fn new(fd: i32) -> io::Result<Self> {
        let mut settings = std::mem::MaybeUninit::uninit();
        // fd 在录入期间存活，系统调用成功后才读取初始化结果。
        if unsafe { libc::tcgetattr(fd, settings.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let settings = unsafe { settings.assume_init() };
        // CLI 每个进程只录入一次，静态快照避免信号处理与释放内存竞争。
        SAVED
            .set(SavedTerminal { fd, settings })
            .map_err(|_| io::Error::other("terminal input has already been initialized"))?;
        let mut terminal = Self {
            saved: SAVED.get().expect("terminal snapshot initialized"),
            previous: Vec::with_capacity(SIGNALS.len()),
            active: true,
        };
        for signal in SIGNALS {
            let mut previous = std::mem::MaybeUninit::uninit();
            if unsafe { libc::sigaction(signal, ptr::null(), previous.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let previous = unsafe { previous.assume_init() };
            if previous.sa_sigaction == libc::SIG_IGN {
                continue;
            }
            // 先装好恢复处理，再关闭回显；处理期间屏蔽这三个信号以避免同线程重入。
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

    pub(super) fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        // 即使终端恢复失败，也逐个恢复本次安装前的信号处置。
        let mut failure = if unsafe {
            libc::tcsetattr(self.saved.fd, libc::TCSAFLUSH, &self.saved.settings)
        } != 0
        {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        while let Some((signal, previous)) = self.previous.pop() {
            if unsafe { libc::sigaction(signal, &previous, ptr::null_mut()) } != 0
                && failure.is_none()
            {
                failure = Some(io::Error::last_os_error());
            }
        }
        self.active = false;
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
