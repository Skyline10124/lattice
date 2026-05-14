# 部署手册

## 系统要求

- Rust 1.85+ (workspace Cargo.toml defines MSRV)
- Linux (primary), macOS (secondary), Windows (limited — no Landlock sandbox)
- Linux kernel ≥ 5.13 (for Landlock sandbox support)
- SQLite 3 with FTS5 extension (for SqliteMemory)
- tokio async runtime (no external runtime dependency)

## 安装方式

### 从源码构建

```bash
# 构建 CLI 二进制
cargo build --release
# 二进制位于 target/release/lattice

# 开发模式构建（更快，调试信息）
cargo build
```

### 环境变量

LLM provider 凭证通过环境变量配置：

```bash
# Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."

# OpenAI
export OPENAI_API_KEY="sk-..."

# Google Gemini
export GOOGLE_API_KEY="AIza..."

# DeepSeek
export DEEPSEEK_API_KEY="sk-..."

# OpenRouter (multi-provider proxy)
export OPENROUTER_API_KEY="sk-or-..."

# Ollama (local, no key needed)
# base_url defaults to http://localhost:11434
```

凭证优先级：`ModelRouter::with_credentials()` override > 环境变量 > 无凭证 (credentialless providers)

### 配置文件

```bash
# 全局配置
~/.lattice/config.toml

# Agent profiles
~/.lattice/agents/*.toml   # 或项目目录 .lattice/agents/

# Session 持久化
~/.lattice/sessions/*.json

# Orchestrator 配置
~/.lattice/orchestrator.toml
```

### config.toml 格式

```toml
[default]
model = "sonnet"        # 默认模型
provider = "anthropic"  # 默认 provider override
stream = true           # 默认流式

[bus]
timeout_rpc_secs = 30
delivery_policy = "at-most-once"
subscriber_buffer = 1024
max_concurrent_calls = 32
```

## 运行模式

### CLI 单次调用

```bash
lattice run sonnet --prompt "Explain Rust traits"
lattice run gpt-4o --file input.txt --no-stream
```

### CLI TUI 模式

```bash
lattice tui --model sonnet
```

交互式终端界面，支持：
- 流式聊天渲染
- Slash 命令（/help, /quit, /clear, /model, /sessions）
- Session 自动保存

### Pipeline 模式

```bash
lattice bus --json  # 查看 pipeline 状态
```

Pipeline 配置通过 agent profile TOML 文件：

```toml
[agent]
name = "reviewer"
model = "sonnet"

[system]
prompt = "You are a code reviewer..."

[[handoff.rules]]
condition = { field = "confidence", op = ">", value = "0.5" }
target = "refactor"
```

### Python 嵌入

```python
from lattice_core import LatticeEngine

engine = LatticeEngine()
resolved = engine.resolve_model("sonnet")
result = engine.chat_complete(resolved, [{"role": "user", "content": "Hello"}])
```

## 安全配置

### 默认安全模式 (strict)

- 命令白名单检查
- 路径读写限制
- Shell metacharacter 拒绝（`; | & && || $() \` \n \r`）
- URL HTTPS-only（localhost HTTP 允许但拒绝 private IP）
- DNS rebinding 保护

### YOLO 模式 (permissive)

`YoloSandboxPolicy` 控制放宽级别：

| Policy | TOML 值 | 效果 |
|--------|---------|------|
| `EnforceCommandAllowlist` | `"allowlist"` 或缺省 | 允许 yolo 但仍用命令白名单（默认 grep/find/ls/ps） |
| `NoBash` | `"nobash"` | 完全禁用 bash 工具 |
| `Custom { allowed_commands }` | TOML 不直接支持 | 自定义命令白名单 |

```toml
[behavior]
mode = "yolo"
enforce_sandbox = "allowlist"  # 或 "nobash"
```

### HookChain 安全扩展

```rust
// VetHook: 拦截 curl 命令，HTTP→HTTPS 转换
// TirithHook: 阻止危险命令模式 (rm -rf /, curl | sh)
```

## Hot Reload

Agent profiles 和 plugin manifests 支持 hot reload：

```bash
# Agent watcher — 监控 ~/.lattice/agents/ 目录变更
# Plugin watcher — 监控 plugin manifest 目录变更
```

使用 `notify` crate（cross-platform file system watcher）。macOS 需 `macos_kQueue` feature。

## 网络配置

### WebSocket 事件流 (feature-gated)

```bash
# 启用 axum feature 构建
cargo build --features ws

# WebSocket 端点
/ws  # PipelineEvent stream
```

默认只允许 localhost origin。`WS_SECRET` 环境变量控制认证。

### 自定义 base_url

```bash
# 通过环境变量覆盖 provider base URL
export ANTHROPIC_BASE_URL="https://custom-proxy.example.com"
```

## 故障排除

参见 [troubleshooting.md](troubleshooting.md)
