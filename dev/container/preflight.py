#!/usr/bin/env python3
"""检查开发容器实际需要的系统能力，失败时保留明确错误。"""
import ctypes
import os
import platform
import pty
import socket
import tempfile
from pathlib import Path

assert os.getuid() != 0, '开发命令必须以普通用户运行'
assert platform.machine() == 'x86_64', '当前镜像固定为 Linux x86_64'

class OpenHow(ctypes.Structure):
    _fields_ = [('flags', ctypes.c_uint64), ('mode', ctypes.c_uint64), ('resolve', ctypes.c_uint64)]

libc = ctypes.CDLL(None, use_errno=True)
libc.syscall.restype = ctypes.c_long
directory = os.open('/workspace', os.O_PATH | os.O_DIRECTORY)
try:
    how = OpenHow(os.O_RDONLY | os.O_CLOEXEC, 0, 0x08)
    fd = libc.syscall(437, directory, b'Cargo.toml', ctypes.byref(how), ctypes.sizeof(how))
    if fd < 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code), 'openat2(/workspace/Cargo.toml)')
    os.close(fd)
finally:
    os.close(directory)

with tempfile.TemporaryDirectory(prefix='lattice-container-') as temp:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as unix:
        unix.bind(str(Path(temp) / 'test.sock'))
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as server:
        server.bind(('127.0.0.1', 0))
        server.listen(1)
        with socket.create_connection(('localhost', server.getsockname()[1]), timeout=2):
            pass

master, slave = pty.openpty()
os.close(master)
os.close(slave)
for folder in (Path('/workspace/target'), Path(os.environ['CARGO_HOME'])):
    with tempfile.TemporaryFile(dir=folder) as writable:
        writable.write(b'container volume probe')
print('preflight: ordinary user, openat2, Unix socket, IPv4 localhost, PTY and writable volumes passed')
