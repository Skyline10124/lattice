# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
"""场景专用 TLS 外壳：POST 原样转给既有 fake provider，GET 返回固定正文。"""
import json
import pathlib
import socket
import socketserver
import ssl
import sys
import threading

root = pathlib.Path(sys.argv[1])
upstream_file = root / "upstream-port"
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(root / "cert.pem", root / "key.pem")
lock = threading.Lock()
stats = {"handshakes": 0, "rejected_handshakes": 0, "get_paths": [], "posts": 0, "errors": []}


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(2)
        try:
            stream = context.wrap_socket(self.request, server_side=True)
        except ssl.SSLError:
            with lock:
                stats["rejected_handshakes"] += 1
            return
        with stream:
            stream.settimeout(30)
            with lock:
                stats["handshakes"] += 1
            try:
                header = bytearray()
                while not header.endswith(b"\r\n\r\n"):
                    byte = stream.recv(1)
                    if not byte:
                        raise EOFError("incomplete HTTP header")
                    header.extend(byte)
                    if len(header) > 16384:
                        raise ValueError("header too large")
                method, path, version = header.split(b"\r\n", 1)[0].decode("ascii").split(" ")
                if version != "HTTP/1.1":
                    raise ValueError("wrong version")
                if method == "POST" and path == "/chat/completions":
                    with lock:
                        stats["posts"] += 1
                    # 请求语法、捕获、脚本次序和 gate 仍由原 fake provider 负责。
                    lengths = [int(line.split(b":", 1)[1]) for line in header.split(b"\r\n")
                               if line.lower().startswith(b"content-length:")]
                    if len(lengths) != 1 or not 0 <= lengths[0] <= 1048576:
                        raise ValueError("invalid body length")
                    body = bytearray()
                    while len(body) < lengths[0]:
                        part = stream.recv(lengths[0] - len(body))
                        if not part:
                            raise EOFError("incomplete body")
                        body.extend(part)
                    upstream_port = int(upstream_file.read_text())
                    with socket.create_connection(("127.0.0.1", upstream_port), timeout=30) as upstream:
                        # 外壳每条连接只转发一个请求：要求上游响应后关闭，响应头随之告诉产品不要复用。
                        upstream.sendall(header[:-2] + b"Connection: close\r\n\r\n" + body)
                        while part := upstream.recv(65536):
                            stream.sendall(part)
                elif method == "GET":
                    with lock:
                        stats["get_paths"].append(path)
                    body = b"frozen TLS tool body"
                    stream.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\nContent-Length: "
                                   + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
                else:
                    raise ValueError("unexpected request")
            except (OSError, ValueError, EOFError) as error:
                with lock:
                    stats["errors"].append(type(error).__name__)


class Server(socketserver.ThreadingTCPServer):
    # 每个操作有 socket deadline，退出时等待在途线程，Rust 驱动另有进程组预算。
    daemon_threads = False


with Server(("127.0.0.1", 0), Handler) as server:
    server.timeout = 0.1
    ready = root / "ready.json"
    ready.write_text(json.dumps({"port": server.server_address[1]}))
    while not (root / "stop").exists():
        server.handle_request()
(root / "stats.json").write_text(json.dumps(stats))
print(json.dumps(stats))
