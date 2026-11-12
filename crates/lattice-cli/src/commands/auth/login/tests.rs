// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use serde_json::{Value, json};
use std::{
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::fs::PermissionsExt,
    },
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::Notify,
};

const URL: &str =
    "https://auth.openai.com/authorize?state=private-state&id_token_hint=private-hint";

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("CLI 登录交互没有收尾")
}

async fn frame(stream: &mut UnixStream) -> Value {
    let mut magic = [0; 8];
    stream.read_exact(&mut magic).await.unwrap();
    assert_eq!(&magic, lattice_local::test_support::FRAME_MAGIC);
    let length = stream.read_u32().await.unwrap();
    let mut bytes = vec![0; length as usize];
    stream.read_exact(&mut bytes).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn reply(stream: &mut UnixStream, sequence: u64, response: &str, value: Value) {
    let bytes = serde_json::to_vec(&json!({"message_id":sequence,"offset":0,"complete":true,
        "json_fragment":json!({"response":response,"value":value}).to_string()}))
    .unwrap();
    stream
        .write_all(lattice_local::test_support::FRAME_MAGIC)
        .await
        .unwrap();
    stream.write_u32(bytes.len() as u32).await.unwrap();
    stream.write_all(&bytes).await.unwrap();
}

async fn peer() -> (tempfile::TempDir, LocalAuthLogin, UnixStream) {
    let directory = tempfile::tempdir().unwrap();
    let layout = lattice_local::StateLayout::in_state_root(directory.path());
    let local = layout.lattice_root().join("local");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = local.join("api.sock");
    let listener = UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let client = lattice_local::LocalClient::new(layout);
    let (login, wire) = tokio::join!(client.auth_login("chatgpt"), listener.accept());
    let mut wire = wire.unwrap().0;
    assert_eq!(
        frame(&mut wire).await,
        json!({"operation":"auth_login","provider":"chatgpt"})
    );
    (directory, login.unwrap(), wire)
}

fn input_pipe() -> (std::os::unix::net::UnixStream, File) {
    let (writer, reader) = std::os::unix::net::UnixStream::pair().unwrap();
    (writer, unsafe { File::from_raw_fd(reader.into_raw_fd()) })
}

#[derive(Default)]
struct Errors {
    bytes: Vec<u8>,
    shown: Arc<Notify>,
    echo: Option<File>,
    writes: Arc<AtomicUsize>,
}
impl Write for Errors {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(terminal) = &self.echo {
            assert_eq!(
                settings(terminal).c_lflag & libc::ECHO,
                0,
                "提示先于回显关闭"
            );
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.bytes.extend_from_slice(bytes);
        if String::from_utf8_lossy(&self.bytes).contains(URL) {
            self.shown.notify_one();
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn pasted_input_waits_for_authorization_and_never_appears_in_output() {
    let _signals = crate::test_support::signal_guard().await;
    let (_directory, login, mut wire) = peer().await;
    let (mut writer, reader) = input_pipe();
    let callback = "http://127.0.0.1:1455/auth/callback?code=private-code&state=private-state";
    writeln!(writer, "{callback}").unwrap();
    let mut out = Vec::new();
    let mut err = Errors::default();
    let buffered = Arc::new(Notify::new());
    let hooks = Hooks {
        buffered: Some(buffered.clone()),
        ..Hooks::default()
    };
    let ui = interact_with_hooks(
        login,
        true,
        || LoginInput::from_file(reader),
        None,
        &mut out,
        &mut err,
        &hooks,
    );
    let backend = async {
        buffered.notified().await;
        assert!(
            matches!(wire.try_read(&mut [0]), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
            "授权前已经发送回调"
        );
        reply(
            &mut wire,
            0,
            "auth_login_authorization",
            json!({"authorization_url":URL}),
        )
        .await;
        assert_eq!(
            frame(&mut wire).await,
            json!({"action":"callback","url":callback})
        );
        reply(
            &mut wire,
            1,
            "auth_login_result",
            json!({"outcome":"signed_in","status":{"provider":"chatgpt","availability":{"status":"ready"}},"catalog":{"status":"refreshed","model_count":0}}),
        )
        .await;
    };
    let (code, ()) = bounded(async { tokio::join!(ui, backend) }).await;
    assert_eq!(code.unwrap(), ExitCode::SUCCESS);
    assert!(!String::from_utf8_lossy(&out).contains("private"));
    assert!(!String::from_utf8_lossy(&err.bytes).contains(callback));
}

#[tokio::test]
async fn browser_failure_or_headless_shows_url_only_in_stderr_and_returns_safe_json() {
    let _signals = crate::test_support::signal_guard().await;
    for program in [
        None,
        Some(OsStr::new("/no/such/xdg-open")),
        Some(OsStr::new("/bin/false")),
    ] {
        let (_directory, login, mut wire) = peer().await;
        let (_writer, reader) = input_pipe();
        let mut out = Vec::new();
        let mut err = Errors::default();
        let shown = err.shown.clone();
        let ui = interact(
            login,
            true,
            || LoginInput::from_file(reader),
            program,
            &mut out,
            &mut err,
        );
        let backend = async {
            reply(
                &mut wire,
                0,
                "auth_login_authorization",
                json!({"authorization_url":URL}),
            )
            .await;
            shown.notified().await;
            reply(
                &mut wire,
                1,
                "auth_login_result",
                json!({"outcome":"signed_in","status":{"provider":"chatgpt","availability":{"status":"ready"}},"catalog":{"status":"refreshed","model_count":0}}),
            )
            .await;
        };
        let (code, ()) = bounded(async { tokio::join!(ui, backend) }).await;
        assert_eq!(code.unwrap(), ExitCode::SUCCESS);
        let stderr = String::from_utf8(err.bytes).unwrap();
        assert!(stderr.contains(URL));
        if program != Some(OsStr::new("/bin/false")) {
            assert_eq!(
                stderr,
                format!(
                    "Open this URL to sign in:\n{URL}\nComplete sign-in in your browser, or paste the callback URL here (input hidden). Ctrl-C cancels.\n"
                )
            );
        }
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("private"));
        assert_eq!(text.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["status"],
            "ready"
        );
    }
}

#[tokio::test]
async fn eof_and_ctrl_c_send_cancel_and_wait_for_the_final_result() {
    let _signals = crate::test_support::signal_guard().await;
    for eof in [true, false] {
        let (_directory, login, mut wire) = peer().await;
        let (writer, reader) = input_pipe();
        let writer = (!eof).then_some(writer);
        let mut out = Vec::new();
        let mut err = Errors::default();
        let ready = Arc::new(Notify::new());
        let input_ready = ready.clone();
        let ui = interact(
            login,
            true,
            || {
                input_ready.notify_one();
                LoginInput::from_file(reader)
            },
            None,
            &mut out,
            &mut err,
        );
        let backend = async {
            reply(
                &mut wire,
                0,
                "auth_login_authorization",
                json!({"authorization_url":URL}),
            )
            .await;
            ready.notified().await;
            if !eof {
                // 仅向持有信号测试锁的当前测试进程发送 SIGINT。
                assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGINT) }, 0);
            }
            assert_eq!(frame(&mut wire).await, json!({"action":"cancel"}));
            reply(
                &mut wire,
                1,
                "auth_login_result",
                json!({"outcome":"cancelled"}),
            )
            .await;
        };
        let (code, ()) = bounded(async { tokio::join!(ui, backend) }).await;
        assert_eq!(code.unwrap(), ExitCode::from(2));
        assert_eq!(
            serde_json::from_slice::<Value>(&out).unwrap()["status"],
            "cancelled"
        );
        assert!(!String::from_utf8(out).unwrap().contains("private"));
        drop(writer);
    }
}

#[tokio::test]
async fn disconnected_login_preserves_transport_class_and_points_to_status() {
    let _signals = crate::test_support::signal_guard().await;
    let (_directory, login, mut wire) = peer().await;
    let (_writer, reader) = input_pipe();
    let mut out = Vec::new();
    let mut err = Errors::default();
    let shown = err.shown.clone();
    let ui = interact(
        login,
        true,
        || LoginInput::from_file(reader),
        None,
        &mut out,
        &mut err,
    );
    let backend = async {
        reply(
            &mut wire,
            0,
            "auth_login_authorization",
            json!({"authorization_url":URL}),
        )
        .await;
        shown.notified().await;
        drop(wire);
    };
    let (result, ()) = bounded(async { tokio::join!(ui, backend) }).await;
    let error = result.unwrap_err();
    let local = error.downcast_ref::<LocalError>().unwrap();
    assert_eq!(local.kind, LocalErrorKind::Transport);
    assert!(local.message.contains("result unknown"));
    assert!(local.message.contains("lattice auth status"));
    assert!(!local.message.contains("private"));
    assert!(out.is_empty());
}

#[test]
fn direct_browser_completion_reaps_the_opener_and_idle_input() {
    const CHILD: &str = "LATTICE_SIWC_OPENER_CAPTURE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(browser_capture_child());
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "commands::auth::login::tests::direct_browser_completion_reaps_the_opener_and_idle_input", "--nocapture"])
        .env(CHILD, "1").output().unwrap();
    assert!(output.status.success(), "隔离的浏览器交互失败");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("private"),
        "opener 泄漏到真实 fd 1"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("private"),
        "opener 泄漏到真实 fd 2"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"status\":\"ready\""));
}

async fn browser_capture_child() {
    let _signals = crate::test_support::signal_guard().await;
    let (directory, login, mut wire) = peer().await;
    let program = directory.path().join("browser");
    let marker = directory.path().join("browser-pid");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '%s' \"$$\" > '{}.tmp'\necho \"$1\"\necho \"$1\" >&2\n/bin/mv '{}.tmp' '{}'\nexec /bin/sleep 60\n", marker.display(), marker.display(), marker.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_writer, reader) = input_pipe();
    let mut out = io::stdout();
    let mut err = io::stderr();
    let ui = interact(
        login,
        true,
        || LoginInput::from_file(reader),
        Some(program.as_os_str()),
        &mut out,
        &mut err,
    );
    let backend = async {
        reply(
            &mut wire,
            0,
            "auth_login_authorization",
            json!({"authorization_url":URL}),
        )
        .await;
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        reply(
            &mut wire,
            1,
            "auth_login_result",
            json!({"outcome":"signed_in","status":{"provider":"chatgpt","availability":{"status":"ready"}},"catalog":{"status":"refreshed","model_count":0}}),
        )
        .await;
    };
    let (code, ()) = bounded(async { tokio::join!(ui, backend) }).await;
    assert_eq!(code.unwrap(), ExitCode::SUCCESS);
    let pid: i32 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

fn terminal() -> (File, File) {
    let (mut master, mut slave) = (-1, -1);
    // 成功后立即接管两个 PTY 描述符。
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
    (unsafe { File::from_raw_fd(master) }, unsafe {
        File::from_raw_fd(slave)
    })
}

fn settings(file: &File) -> libc::termios {
    let mut value = std::mem::MaybeUninit::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(file.as_raw_fd(), value.as_mut_ptr()) },
        0
    );
    unsafe { value.assume_init() }
}

#[tokio::test]
async fn prompt_waits_for_hidden_echo_and_guard_lasts_until_terminal_result() {
    let _signals = crate::test_support::signal_guard().await;
    let (_directory, login, mut wire) = peer().await;
    let (_master, slave) = terminal();
    let original = settings(&slave);
    assert_ne!(original.c_lflag & libc::ECHO, 0);
    let reader = slave.try_clone().unwrap();
    let initialized = Arc::new(Notify::new());
    let entered = initialized.clone();
    let (release, gate) = std::sync::mpsc::channel();
    let authorized = Arc::new(Notify::new());
    let hooks = Hooks {
        authorized: Some(authorized.clone()),
        ..Hooks::default()
    };
    let mut out = Vec::new();
    let mut err = Errors {
        echo: Some(slave.try_clone().unwrap()),
        ..Errors::default()
    };
    let writes = err.writes.clone();
    let shown = err.shown.clone();
    let ui = interact_with_hooks(
        login,
        true,
        || {
            LoginInput::with_initialization(reader, move || {
                entered.notify_one();
                gate.recv_timeout(Duration::from_secs(3))
                    .map_err(io::Error::other)
            })
        },
        None,
        &mut out,
        &mut err,
        &hooks,
    );
    let backend = async {
        initialized.notified().await;
        reply(
            &mut wire,
            0,
            "auth_login_authorization",
            json!({"authorization_url":URL}),
        )
        .await;
        authorized.notified().await;
        assert_ne!(settings(&slave).c_lflag & libc::ECHO, 0);
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        release.send(()).unwrap();
        shown.notified().await;
        assert_eq!(settings(&slave).c_lflag & libc::ECHO, 0);
        reply(
            &mut wire,
            1,
            "auth_login_result",
            json!({"outcome":"signed_in","status":{"provider":"chatgpt","availability":{"status":"ready"}},"catalog":{"status":"refreshed","model_count":0}}),
        )
        .await;
    };
    let (code, ()) = bounded(async { tokio::join!(ui, backend) }).await;
    assert_eq!(code.unwrap(), ExitCode::SUCCESS);
    assert_eq!(settings(&slave).c_lflag, original.c_lflag);
}

#[tokio::test]
async fn initialization_failure_returns_before_any_paste_prompt() {
    let _signals = crate::test_support::signal_guard().await;
    let (_directory, login, mut wire) = peer().await;
    let (_writer, reader) = input_pipe();
    let mut out = Vec::new();
    let mut err = Vec::new();
    reply(
        &mut wire,
        0,
        "auth_login_authorization",
        json!({"authorization_url":URL}),
    )
    .await;
    let result = bounded(interact(
        login,
        true,
        || {
            LoginInput::with_initialization(reader, || {
                Err(io::Error::other("terminal initialization rejected"))
            })
        },
        None,
        &mut out,
        &mut err,
    ))
    .await;
    assert!(result.is_err());
    assert!(err.is_empty());
    assert!(out.is_empty());
}

#[tokio::test]
async fn eof_after_callback_cancels_before_the_gated_final_result() {
    let _signals = crate::test_support::signal_guard().await;
    let (_directory, login, mut wire) = peer().await;
    let (mut writer, reader) = input_pipe();
    let mut out = Vec::new();
    let mut err = Errors::default();
    let shown = err.shown.clone();
    let ui = interact(
        login,
        true,
        || LoginInput::from_file(reader),
        None,
        &mut out,
        &mut err,
    );
    let backend = async {
        reply(
            &mut wire,
            0,
            "auth_login_authorization",
            json!({"authorization_url":URL}),
        )
        .await;
        shown.notified().await;
        writeln!(writer, "http://callback?code=private-first").unwrap();
        assert_eq!(
            frame(&mut wire).await,
            json!({"action":"callback","url":"http://callback?code=private-first"})
        );
        // 最终结果仅在 EOF 的取消抵达后放行；第二行不得再次交付回调。
        writeln!(writer, "http://callback?code=private-second").unwrap();
        drop(writer);
        assert_eq!(frame(&mut wire).await, json!({"action":"cancel"}));
        reply(
            &mut wire,
            1,
            "auth_login_result",
            json!({"outcome":"cancelled"}),
        )
        .await;
    };
    let (code, ()) = bounded(async { tokio::join!(ui, backend) }).await;
    assert_eq!(code.unwrap(), ExitCode::from(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["status"],
        "cancelled"
    );
}

async fn terminal_result_wins_a_failed_write(callback: bool) {
    let _signals = crate::test_support::signal_guard().await;
    let (_directory, login, mut wire) = peer().await;
    let (mut writer, reader) = input_pipe();
    let mut out = Vec::new();
    let mut err = Errors::default();
    let shown = err.shown.clone();
    let writing = Arc::new(Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let hooks = Hooks {
        write: Some((writing.clone(), release.clone())),
        ..Hooks::default()
    };
    let ui = interact_with_hooks(
        login,
        true,
        || LoginInput::from_file(reader),
        None,
        &mut out,
        &mut err,
        &hooks,
    );
    let backend = async {
        reply(
            &mut wire,
            0,
            "auth_login_authorization",
            json!({"authorization_url":URL}),
        )
        .await;
        shown.notified().await;
        if callback {
            writeln!(writer, "http://callback?code=private-code").unwrap();
        } else {
            writer.shutdown(std::net::Shutdown::Write).unwrap();
        }
        writing.notified().await;
        reply(
            &mut wire,
            1,
            "auth_login_result",
            json!({"outcome":"signed_in","status":{"provider":"chatgpt","availability":{"status":"ready"}},"catalog":{"status":"refreshed","model_count":0}}),
        )
        .await;
        drop(wire);
        release.add_permits(1);
    };
    let (code, ()) = bounded(async { tokio::join!(ui, backend) }).await;
    assert_eq!(code.unwrap(), ExitCode::SUCCESS);
    assert!(
        hooks.write_failed.load(Ordering::SeqCst),
        "必须实际触发写失败"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["status"],
        "ready"
    );
}

#[tokio::test]
async fn signed_in_wins_after_callback_write_fails() {
    terminal_result_wins_a_failed_write(true).await;
}

#[tokio::test]
async fn signed_in_wins_after_cancel_write_fails() {
    terminal_result_wins_a_failed_write(false).await;
}
