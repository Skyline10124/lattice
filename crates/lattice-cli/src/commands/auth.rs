// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{
    io::{self, IsTerminal, Read, Write},
    os::fd::AsRawFd,
};

use clap::Subcommand;
use lattice_api::{StateLayout, local::LocalClient};
use zeroize::Zeroizing;

mod terminal;
use terminal::TerminalInput;

// 读取预算保留 CRLF 和一个超限字节，key 的规则由后台统一校验。
const MAX_INPUT_BYTES: usize = 16 * 1024 + 3;

#[derive(Subcommand)]
pub enum AuthAction {
    /// Read an API key from the terminal without echo, or from standard input.
    Set { provider: String },
    /// Remove a provider's API key.
    Remove { provider: String },
    /// Show credential status without exposing credentials.
    Status,
}

pub async fn run(action: AuthAction, json: bool) -> anyhow::Result<()> {
    let client = LocalClient::new(StateLayout::resolve()?);
    match action {
        AuthAction::Set { provider } => {
            let mut key = Zeroizing::new(read_key()?);
            client.ensure_running().await?;
            client
                .auth_set(&provider, std::mem::take(&mut *key))
                .await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"type":"auth","action":"set","provider":provider})
                );
            } else {
                println!("{provider} set");
            }
        }
        AuthAction::Remove { provider } => {
            client.ensure_running().await?;
            let removed = client.auth_remove(&provider).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"type":"auth","action":"remove","provider":provider,"removed":removed})
                );
            } else {
                println!("{provider} {}", if removed { "removed" } else { "missing" });
            }
        }
        AuthAction::Status => {
            client.ensure_running().await?;
            let status = client.auth_status().await?;
            if json {
                println!("{}", serde_json::to_string(&status)?);
            } else {
                for row in status.providers {
                    print!("{} {}", row.provider, row.status);
                    if let Some(email) = row.email {
                        print!(" email={email}");
                    }
                    if let Some(expiry) = row.expires_at {
                        print!(" expires_at={expiry}");
                    }
                    println!();
                }
                if let Some(error) = status.error {
                    eprintln!("{error}");
                }
            }
        }
    }
    Ok(())
}

fn finish_key(mut bytes: Zeroizing<Vec<u8>>) -> anyhow::Result<String> {
    // 管道常用一行传入 key；只移除一个行结束符，不裁剪 key 两侧的空格。
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    let key = std::str::from_utf8(&bytes).map_err(|_| {
        lattice_api::ApiError::Configuration(lattice_api::ConfigRejection::new(
            "API key input must be valid UTF-8",
            &[],
        ))
    })?;
    Ok(key.to_owned())
}

fn read_key() -> anyhow::Result<String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    if !stdin.is_terminal() {
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_INPUT_BYTES + 1));
        input.take(MAX_INPUT_BYTES as u64).read_to_end(&mut bytes)?;
        return finish_key(bytes);
    }
    let mut terminal = TerminalInput::new(input.as_raw_fd())?;
    eprint!("API key: ");
    io::stderr().flush()?;
    let result = read_terminal(&mut input);
    terminal.restore()?;
    eprintln!();
    result
}

fn read_terminal(input: &mut impl Read) -> anyhow::Result<String> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_INPUT_BYTES + 1));
    let mut input = input.take(MAX_INPUT_BYTES as u64);
    loop {
        let mut byte = [0];
        if input.read(&mut byte)? == 0 {
            return finish_key(bytes);
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            return finish_key(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_normalization_only_strips_one_line_ending_and_checks_utf8() {
        for input in [" key ", " key \n", " key \r\n"] {
            assert_eq!(
                finish_key(Zeroizing::new(input.as_bytes().to_vec())).unwrap(),
                " key "
            );
        }
        for (input, expected) in [
            ("", ""),
            ("\n", ""),
            ("one\ntwo\n", "one\ntwo"),
            ("bad\u{85}", "bad\u{85}"),
            ("bad\0", "bad\0"),
        ] {
            assert!(finish_key(Zeroizing::new(input.as_bytes().to_vec())).unwrap() == expected);
        }
        assert!(finish_key(Zeroizing::new(vec![b'k'; 16_385])).is_ok());
        assert!(finish_key(Zeroizing::new(vec![0xff])).is_err());
    }
    #[test]
    fn terminal_line_preserves_input_for_backend_validation_and_bounds_reading() {
        assert_eq!(read_terminal(&mut "a中b\n".as_bytes()).unwrap(), "a中b");
        assert_eq!(read_terminal(&mut "a\0b\n".as_bytes()).unwrap(), "a\0b");
        let source = vec![b'k'; 32_768];
        let mut input = std::io::Cursor::new(source);
        assert_eq!(read_terminal(&mut input).unwrap().len(), 16_387);
        assert_eq!(input.position(), 16_387);
    }

    #[tokio::test]
    async fn terminal_preserves_canonical_editing_and_restores_settings_after_read() {
        let _guard = crate::test_support::signal_guard().await;
        use std::{fs::File, os::fd::FromRawFd};
        let (mut master, mut slave) = (-1, -1);
        // openpty 成功后两个描述符均由 File 独占并在测试结束时关闭。
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        let mut master = unsafe { File::from_raw_fd(master) };
        let mut slave = unsafe { File::from_raw_fd(slave) };
        let mut original = std::mem::MaybeUninit::uninit();
        // slave 是仍存活的终端；只有系统调用成功才读取初始化结果。
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), original.as_mut_ptr()) },
            0
        );
        let original = unsafe { original.assume_init() };
        {
            let _terminal = TerminalInput::new(slave.as_raw_fd()).unwrap();
            let mut hidden = std::mem::MaybeUninit::uninit();
            // 检查实际终端，确认只有回显位发生变化。
            assert_eq!(
                unsafe { libc::tcgetattr(slave.as_raw_fd(), hidden.as_mut_ptr()) },
                0
            );
            let hidden = unsafe { hidden.assume_init() };
            assert_eq!(
                hidden.c_lflag,
                original.c_lflag & !(libc::ECHO | libc::ECHONL)
            );
            assert_eq!(hidden.c_iflag, original.c_iflag);
            assert_eq!(hidden.c_cc, original.c_cc);
            master.write_all(b"terminal-test-secrex\x7ft\n").unwrap();
            assert!(read_terminal(&mut slave).unwrap() == "terminal-test-secret");
            let mut available = 0_i32;
            // FIONREAD 只查询 master 中可读的字节量，不输出终端内容。
            assert_eq!(
                unsafe { libc::ioctl(master.as_raw_fd(), libc::FIONREAD, &mut available) },
                0
            );
            assert_eq!(available, 0);
        }
        let mut restored = std::mem::MaybeUninit::uninit();
        // 与上面的查询相同，成功后才读取 termios。
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), restored.as_mut_ptr()) },
            0
        );
        let restored = unsafe { restored.assume_init() };
        assert_eq!(restored.c_lflag, original.c_lflag);
        assert_eq!(restored.c_iflag, original.c_iflag);
        assert_eq!(restored.c_cc, original.c_cc);
    }

    #[test]
    fn terminal_signals_restore_echo_before_default_termination() {
        use std::{
            fs::File,
            os::{fd::FromRawFd, unix::process::ExitStatusExt},
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        const CHILD: &str = "LATTICE_AUTH_TERMINAL_SIGNAL_TEST";
        if let Ok(role) = std::env::var(CHILD) {
            let (signal, ignored) = role.split_once(':').unwrap();
            let signal = signal.parse::<i32>().unwrap();
            let ignored = ignored == "ignored";
            // 子进程只调整本轮要验证的信号，避免继承测试宿主的处置影响断言。
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = if ignored {
                libc::SIG_IGN
            } else {
                libc::SIG_DFL
            };
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            assert_eq!(
                unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) },
                0
            );
            let stdin = io::stdin();
            let mut input = stdin.lock();
            let _terminal = TerminalInput::new(input.as_raw_fd()).unwrap();
            if ignored {
                let mut current = std::mem::MaybeUninit::uninit();
                assert_eq!(
                    unsafe { libc::sigaction(signal, std::ptr::null(), current.as_mut_ptr()) },
                    0
                );
                assert_eq!(unsafe { current.assume_init() }.sa_sigaction, libc::SIG_IGN);
            }
            let _ = read_terminal(&mut input);
            return;
        }
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        fn settings(fd: i32) -> libc::termios {
            let mut value = std::mem::MaybeUninit::uninit();
            // fd 是本测试持有的 PTY，成功后才读取 termios。
            assert_eq!(unsafe { libc::tcgetattr(fd, value.as_mut_ptr()) }, 0);
            unsafe { value.assume_init() }
        }
        for (signal, ignored) in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP]
            .into_iter()
            .flat_map(|signal| [(signal, false), (signal, true)])
        {
            let (mut master, mut slave) = (-1, -1);
            // 成功创建的两个 PTY 描述符立即交给 File 管理。
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        std::ptr::null(),
                    )
                },
                0
            );
            let mut master = unsafe { File::from_raw_fd(master) };
            let slave = unsafe { File::from_raw_fd(slave) };
            let original = settings(slave.as_raw_fd());
            let mut child = Child(Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "commands::auth::tests::terminal_signals_restore_echo_before_default_termination", "--nocapture"])
                .env(CHILD, format!("{signal}:{}", if ignored { "ignored" } else { "default" }))
                .stdin(Stdio::from(slave.try_clone().unwrap()))
                .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
            let deadline = Instant::now() + Duration::from_secs(5);
            while settings(slave.as_raw_fd()).c_lflag & libc::ECHO != 0 {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "terminal reader exited early"
                );
                assert!(
                    Instant::now() < deadline,
                    "terminal reader did not disable echo"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            // 只向本测试创建且尚未退出的子进程发送信号。
            assert_eq!(unsafe { libc::kill(child.0.id() as i32, signal) }, 0);
            if ignored {
                master.write_all(b"ignored-input\n").unwrap();
            }
            let status = loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                assert!(
                    Instant::now() < deadline,
                    "terminal reader did not exit after signal"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            if ignored {
                assert_eq!(status.code(), Some(0));
            } else {
                assert_eq!(status.signal(), Some(signal));
            }
            let restored = settings(slave.as_raw_fd());
            assert_eq!(restored.c_lflag, original.c_lflag);
            assert_eq!(restored.c_iflag, original.c_iflag);
            assert_eq!(restored.c_cc, original.c_cc);
        }
    }
}
