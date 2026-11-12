// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_local::SensitiveLoginUrl;
use std::{
    fs::File,
    io::{self, IsTerminal, Read},
    net::Shutdown,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::net::UnixStream,
    },
    thread::JoinHandle,
};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

const MAX_URL_BYTES: usize = 16_384;

pub(super) enum InputEvent {
    Ready,
    Callback(SensitiveLoginUrl),
    Ended,
}

pub(super) struct LoginInput {
    events: mpsc::UnboundedReceiver<io::Result<InputEvent>>,
    wake: UnixStream,
    worker: Option<JoinHandle<()>>,
}

impl LoginInput {
    pub(super) fn stdin() -> io::Result<Self> {
        // 独占自己的 fd；不修改 stdin 的阻塞标志，也不占用全局 StdinLock。
        let fd = unsafe { libc::fcntl(io::stdin().as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Self::from_file(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn from_file(input: File) -> io::Result<Self> {
        Self::start(input, || Ok(()))
    }

    fn start(
        mut input: File,
        initialize: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let (wake, cancellation) = UnixStream::pair()?;
        // 就绪、首行回调与结束事件至多各一条；后续输入只丢弃，不积累消息。
        let (sender, events) = mpsc::unbounded_channel();
        let worker = std::thread::spawn(move || {
            let echo = match initialize().and_then(|()| Echo::hide(&input)) {
                Ok(echo) => echo,
                Err(error) => {
                    let _ = sender.send(Err(error));
                    return;
                }
            };
            if sender.send(Ok(InputEvent::Ready)).is_err() {
                return;
            }
            let result = read(&mut input, &cancellation, &sender);
            let _ = sender.send(result);
            // EOF 或输入错误只报告事件；回显保护持续到界面交互收尾。
            let _ = wait(&cancellation, None);
            drop(echo);
        });
        Ok(Self {
            events,
            wake,
            worker: Some(worker),
        })
    }

    pub(super) async fn next(&mut self) -> io::Result<InputEvent> {
        self.events
            .recv()
            .await
            .ok_or_else(|| io::Error::other("login input worker failed"))?
    }

    #[cfg(test)]
    pub(super) fn with_initialization(
        input: File,
        initialize: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        Self::start(input, initialize)
    }
}

impl Drop for LoginInput {
    fn drop(&mut self) {
        let _ = self.wake.shutdown(Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Echo {
    fd: i32,
    original: libc::termios,
}
impl Echo {
    fn hide(file: &File) -> io::Result<Option<Self>> {
        if !file.is_terminal() {
            return Ok(None);
        }
        let fd = file.as_raw_fd();
        let mut original = std::mem::MaybeUninit::uninit();
        // fd 在 worker 内存活，只有 tcgetattr 成功后才读取 termios。
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let original = unsafe { original.assume_init() };
        let guard = Self { fd, original };
        let mut hidden = original;
        hidden.c_lflag &= !(libc::ECHO | libc::ECHONL);
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(guard))
    }
}
impl Drop for Echo {
    fn drop(&mut self) {
        // 保留规范行编辑；取消时丢弃尚未提交的敏感输入并恢复原设置。
        let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.original) };
    }
}

fn wait(cancellation: &UnixStream, input: Option<&File>) -> io::Result<bool> {
    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: cancellation.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: input.map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // 两个 fd 都由 worker 独占；取消唤醒 poll 后一定能回收线程。
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Ok(descriptors[0].revents == 0);
    }
}

fn read(
    input: &mut File,
    cancellation: &UnixStream,
    sender: &mpsc::UnboundedSender<io::Result<InputEvent>>,
) -> io::Result<InputEvent> {
    let mut bytes = Zeroizing::new(Vec::new());
    let mut delivered = false;
    while wait(cancellation, Some(input))? {
        let mut byte = [0];
        if input.read(&mut byte)? == 0 {
            return Ok(InputEvent::Ended);
        }
        if delivered {
            continue;
        }
        if byte[0] == b'\n' {
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            if bytes.len() > MAX_URL_BYTES {
                return Err(invalid());
            }
            let url = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
            if sender
                .send(Ok(InputEvent::Callback(SensitiveLoginUrl::new(url.into()))))
                .is_err()
            {
                return Ok(InputEvent::Ended);
            }
            zeroize::Zeroize::zeroize(&mut *bytes);
            delivered = true;
        } else {
            bytes.push(byte[0]);
            if bytes.len() > MAX_URL_BYTES + 1 {
                return Err(invalid());
            }
        }
    }
    Ok(InputEvent::Ended)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "callback URL must be UTF-8 and at most 16384 bytes",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    async fn callback(input: &mut LoginInput) -> io::Result<Option<SensitiveLoginUrl>> {
        loop {
            match input.next().await? {
                InputEvent::Ready => {}
                InputEvent::Callback(url) => return Ok(Some(url)),
                InputEvent::Ended => return Ok(None),
            }
        }
    }

    fn pipe() -> (UnixStream, File) {
        let (writer, reader) = UnixStream::pair().unwrap();
        use std::os::fd::IntoRawFd;
        (writer, unsafe { File::from_raw_fd(reader.into_raw_fd()) })
    }

    #[tokio::test]
    async fn line_eof_and_invalid_input_have_distinct_results() {
        for (value, valid) in [
            (b"http://callback?code=private\r\n".as_slice(), true),
            (b"\xff\n", false),
        ] {
            let (mut writer, reader) = pipe();
            let mut input = LoginInput::from_file(reader).unwrap();
            writer.write_all(value).unwrap();
            let result = callback(&mut input).await;
            assert_eq!(result.is_ok(), valid);
            if valid {
                assert!(result.unwrap().unwrap().expose() == "http://callback?code=private");
            }
        }
        let (writer, reader) = pipe();
        let mut input = LoginInput::from_file(reader).unwrap();
        drop(writer);
        assert!(callback(&mut input).await.unwrap().is_none());
    }

    #[test]
    fn drop_wakes_and_joins_an_idle_reader() {
        let (_writer, reader) = pipe();
        let input = LoginInput::from_file(reader).unwrap();
        drop(input);
    }

    #[tokio::test]
    async fn maximum_input_accepts_crlf_but_overflow_needs_no_line_ending() {
        for (line, valid) in [
            (format!("{}\r\n", "x".repeat(16_384)), true),
            (format!("{}\n", "x".repeat(16_385)), false),
            ("x".repeat(16_386), false),
        ] {
            let (mut writer, reader) = pipe();
            let mut input = LoginInput::from_file(reader).unwrap();
            writer.write_all(line.as_bytes()).unwrap();
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(3), callback(&mut input))
                    .await
                    .unwrap();
            if valid {
                assert_eq!(result.unwrap().unwrap().expose().len(), 16_384);
            } else {
                let error = result.err().unwrap();
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                assert!(!error.to_string().contains(&line));
            }
        }
    }

    #[tokio::test]
    async fn terminal_keeps_line_editing_hides_paste_and_restores_echo() {
        let (mut master, mut slave) = (-1, -1);
        // openpty 成功后，两个描述符都立即交给 File 管理。
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
        let settings = || {
            let mut value = std::mem::MaybeUninit::uninit();
            assert_eq!(
                unsafe { libc::tcgetattr(slave.as_raw_fd(), value.as_mut_ptr()) },
                0
            );
            unsafe { value.assume_init() }
        };
        let original = settings();
        let mut input = LoginInput::from_file(slave.try_clone().unwrap()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while settings().c_lflag & libc::ECHO != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            settings().c_lflag & libc::ICANON,
            original.c_lflag & libc::ICANON
        );
        master.write_all(b"aX\x7fb\n").unwrap();
        assert!(callback(&mut input).await.unwrap().unwrap().expose() == "ab");
        assert_eq!(settings().c_lflag & libc::ECHO, 0);
        master.write_all(b"\x04").unwrap();
        assert!(callback(&mut input).await.unwrap().is_none());
        assert_eq!(settings().c_lflag & libc::ECHO, 0);
        drop(input);
        assert_eq!(settings().c_lflag, original.c_lflag);
        assert_eq!(settings().c_cc, original.c_cc);
    }
}
