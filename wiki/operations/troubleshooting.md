# 故障排除

## 常见错误

### RateLimitError

**症状**: 429 HTTP 状态码，`retry_after` 值

**原因**: Provider 限流

**处理**: 自动重试（RetryPolicy 默认 3 次，1s 基础延迟，60s 上限，jittered exponential backoff）。如果持续限流：
- 减少并发请求
- 检查 provider dashboard 的 usage limit
- 考虑使用 alternative provider（同一模型多个 provider 情况下自动切换）

### AuthenticationError

**症状**: 401/403 HTTP 状态码

**原因**: API key 无效或缺失

**处理**:
```bash
# 检查 key 状态
lattice doctor
lattice debug <model>

# 确认环境变量
echo $ANTHROPIC_API_KEY  # 或其他 provider key

# 重新设置
export ANTHROPIC_API_KEY="sk-ant-..."
```

### ModelNotFoundError

**症状**: 模型不在 catalog 中

**原因**: 模型 ID 不存在或别名未注册

**处理**:
```bash
# 查看可用模型
lattice models
lattice models --auth-only

# 检查 catalog data
# lattice-core/src/catalog/data.json 包含所有模型定义

# 自定义模型注册（Rust API）
router.register_model(entry);
```

### ContextWindowExceededError

**症状**: 输入 token 超出模型 context_length

**原因**: 消息过长

**处理**:
- Agent 自动调用 `trim_messages()` 裁剪旧消息
- 减少 seed_messages 数量
- 使用 context_length 更大的模型

### StreamingError

**症状**: SSE stream 中途出错

**原因**: Provider 网络中断、服务端断连

**处理**: Agent 自动重试（MAX_STREAM_RETRIES=2），仅对 retryable 消息（rate_limit、overloaded、timeout、429/503/502）重试

### ToolExecution / Sandbox 拦截

**症状**: bash 命令被拒绝

**常见拒绝原因**:
- Shell metacharacter: `; | & && || $() \` \n \r` → 改用直接命令而非 shell 链式操作
- 命令不在白名单 → 修改 `allowed_commands`
- 路径不在白名单 → 修改 `allowed_write_dirs`
- curl HTTP → 自动 VetHook 转为 HTTPS；`curl | sh` 被 TirithHook 阻止

**处理**:
```toml
[behavior]
mode = "yolo"
enforce_sandbox = "allowlist"
# Custom commands:
# enforce_sandbox = "allowlist"，并在 Custom variant 中指定 allowed_commands
```

## 运行时问题

### "no reactor running" panic

**原因**: 在同步上下文中调用 async 函数（特别是 reqwest）

**处理**: 确保 tokio runtime 已初始化。CLI 使用 `#[tokio::main]`，Python 使用 `SHARED_RUNTIME`。不要在 `#[test]` 中直接调用 async 函数——使用 `#[tokio::test]`。

### RwLock poisoned

**原因**: 持有 lock 的线程 panic

**处理**: LATTICE 使用 `unwrap_or_else(|e| e.into_inner())` recovery——自动恢复而非 panic 传播。检查日志中的 "RwLock poisoned" 消息定位原 panic。

### SQLite FTS5 不可用

**症状**: SqliteMemory 初始化失败

**原因**: SQLite 编译时未启用 FTS5 extension

**处理**:
```bash
# 检查 SQLite 版本和 FTS5 支持
sqlite3 --version
# LATTICE 使用 rusqlite，bundled mode 包含 FTS5

# 确保 cargo build 时 bundled SQLite feature 启用
# lattice-agent/Cargo.toml: rusqlite with "bundled" and "fts5" features
```

### Landlock sandbox 在非 Linux 平台不可用

**原因**: Landlock 需要 Linux kernel >= 5.13

**处理**:
- macOS: 使用 resource limits (RLIMIT) 而非 Landlock
- Windows: 无沙箱可用（`execute_bash_command` 的 non-Unix 分支无限制）
- 生产环境推荐在 Linux 上运行

### Session ID 验证失败

**症状**: `validate_session_id` 返回错误

**原因**: ID 包含非法字符

**处理**: Session ID 只允许 ASCII alphanumeric + `-` + `_`，长度 <= 64。自动生成的 SHA-256 ID 应始终合法。

## 网络问题

### Private IP 拦截

**症状**: URL 被 check_url 拒绝

**原因**: URL 指向 private/link-local IP（10.x, 172.16-31.x, 192.168.x, 127.x, ::1, fe80::）

**处理**:
- 对 localhost HTTP URL：允许但限制为 `localhost`（不含 `localhost.*` 子域）
- 对 HTTPS private IP：默认拒绝（防止 SSRF + DNS rebinding）
- 如需内部 API：在 SandboxConfig 配置允许的 URL

### DNS rebinding 保护

**症状**: 域名被拒绝（如 `evil.com` 解析到 `127.0.0.1`）

**原因**: Sandbox 的 `check_url` 在请求前解析 DNS，验证目标 IP 不是 private

**处理**: 此为安全保护，不应绕过。如需访问内部服务，在 `allowed_commands` 中允许特定 curl 目标。