// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! systemd `$NOTIFY_SOCKET` 非阻塞数据报通知（`READY=1` 与 `STOPPING=1`）。

use std::{
    ffi::{OsStr, OsString},
    io,
    os::unix::ffi::OsStrExt,
    path::Path,
};

use rustix::net::{
    AddressFamily, SendFlags, SocketAddrUnix, SocketFlags, SocketType, sendto, socket_with,
};

#[derive(Clone, Debug)]
pub(crate) struct Notifier {
    socket: Option<OsString>,
}

impl Notifier {
    pub(crate) fn from_env() -> Self {
        Self {
            socket: std::env::var_os("NOTIFY_SOCKET"),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_socket(socket: Option<OsString>) -> Self {
        Self { socket }
    }

    pub(crate) fn notify_ready(&self) -> io::Result<()> {
        self.send(b"READY=1")
    }

    pub(crate) fn notify_stopping(&self) -> io::Result<()> {
        self.send(b"STOPPING=1")
    }

    fn send(&self, payload: &[u8]) -> io::Result<()> {
        let Some(target) = self.socket.as_deref() else {
            return Ok(());
        };
        let addr = parse_notify_addr(target)?;
        let sock = socket_with(
            AddressFamily::UNIX,
            SocketType::DGRAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )
        .map_err(io::Error::from)?;
        let sent = sendto(&sock, payload, SendFlags::DONTWAIT, &addr).map_err(io::Error::from)?;
        if sent != payload.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "systemd 通知报文未完整写出",
            ));
        }
        Ok(())
    }
}

fn parse_notify_addr(raw: &OsStr) -> io::Result<SocketAddrUnix> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NOTIFY_SOCKET 不能为空",
        ));
    }
    if let Some(abstract_name) = bytes.strip_prefix(b"@") {
        if abstract_name.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "抽象命名空间 NOTIFY_SOCKET 名称不能为空",
            ));
        }
        SocketAddrUnix::new_abstract_name(abstract_name).map_err(io::Error::from)
    } else {
        SocketAddrUnix::new(Path::new(raw)).map_err(io::Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::net::{RecvFlags, bind, recv};
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn unset_notify_socket_is_noop() {
        let notifier = Notifier::with_socket(None);
        notifier
            .notify_ready()
            .expect("未设置 NOTIFY_SOCKET 时应为空操作");
        notifier
            .notify_stopping()
            .expect("未设置 NOTIFY_SOCKET 时应为空操作");
    }

    #[test]
    fn filesystem_notify_socket_preserves_non_utf8_bytes_and_delivers_messages() {
        let dir = tempfile::tempdir().unwrap();
        let mut raw_path = dir.path().as_os_str().as_bytes().to_vec();
        raw_path.extend_from_slice(b"/notify-\xff.sock");
        let os_path = OsString::from_vec(raw_path);
        let addr = SocketAddrUnix::new(Path::new(&os_path)).unwrap();
        let receiver = socket_with(
            AddressFamily::UNIX,
            SocketType::DGRAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )
        .unwrap();
        bind(&receiver, &addr).unwrap();

        let notifier = Notifier::with_socket(Some(os_path));
        notifier
            .notify_ready()
            .expect("文件系统 NOTIFY_SOCKET 应能发送 READY=1");
        let mut buf = [0u8; 32];
        let (len, _) = recv(&receiver, &mut buf, RecvFlags::DONTWAIT).unwrap();
        assert_eq!(&buf[..len], b"READY=1");

        notifier
            .notify_stopping()
            .expect("文件系统 NOTIFY_SOCKET 应能发送 STOPPING=1");
        let (len, _) = recv(&receiver, &mut buf, RecvFlags::DONTWAIT).unwrap();
        assert_eq!(&buf[..len], b"STOPPING=1");
    }

    #[test]
    fn abstract_notify_socket_preserves_non_utf8_bytes_and_delivers_messages() {
        let mut abstract_name = b"lattice-daemon-notify-".to_vec();
        abstract_name.extend_from_slice(&std::process::id().to_le_bytes());
        abstract_name.push(0xfe);
        let addr = SocketAddrUnix::new_abstract_name(&abstract_name).unwrap();
        let receiver = socket_with(
            AddressFamily::UNIX,
            SocketType::DGRAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )
        .unwrap();
        bind(&receiver, &addr).unwrap();

        let mut env_value = vec![b'@'];
        env_value.extend_from_slice(&abstract_name);
        let notifier = Notifier::with_socket(Some(OsString::from_vec(env_value)));
        notifier
            .notify_ready()
            .expect("抽象 NOTIFY_SOCKET 应能发送 READY=1");
        let mut buf = [0u8; 32];
        let (len, _) = recv(&receiver, &mut buf, RecvFlags::DONTWAIT).unwrap();
        assert_eq!(&buf[..len], b"READY=1");

        notifier
            .notify_stopping()
            .expect("抽象 NOTIFY_SOCKET 应能发送 STOPPING=1");
        let (len, _) = recv(&receiver, &mut buf, RecvFlags::DONTWAIT).unwrap();
        assert_eq!(&buf[..len], b"STOPPING=1");
    }

    #[test]
    fn missing_or_invalid_notify_socket_returns_io_error() {
        for invalid in [
            OsString::from(""),
            OsString::from("@"),
            OsString::from("/nonexistent-dir-for-lattice-notify/notify.sock"),
        ] {
            let notifier = Notifier::with_socket(Some(invalid.clone()));
            assert!(
                notifier.notify_ready().is_err(),
                "无效通知地址 {invalid:?} 应返回错误"
            );
        }
    }
}
