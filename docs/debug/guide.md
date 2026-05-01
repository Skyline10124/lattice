# LATTICE Debug 指南

**最后更新**: 2026-05-01

---

## 1. 常见问题与排查

### 1.1 模型解析失败

**症状**: `resolve("model-name")` 返回 `LatticeError::ModelNotFound`

**排查步骤**:

1. 确认模型名是否在 catalog 中：`LATTICE models` 或检查 `lattice-core/src/catalog/data.json`
2. 检查别名映射：`LATTICE debug model-name`
3. 检查环境变量：`echo $ANTHROPIC_API_KEY` / `echo $OPENAI_API_KEY` 等
4. 如果是 `provider/model` 格式，检查 `resolve_permissive()` 的 provider_defaults 表

**常见原因**:
- 模型名拼写错误（如 `sonnet-4` 应为 `sonnet`）
- 环境变量未设置或为空
- catalog 中该模型没有匹配当前凭证的 provider

### 1.2 chat() 返回 Streaming 错误

**症状**: `StreamEvent::Error { message }` 或 `LatticeError::ProviderUnavailable`

**排查步骤**:

1. 检查 HTTP status：当前 `StreamEvent::Error` 不携带 status code（P1-1），需查看日志
2. 开启 debug 日志：`RUST_LOG=lattice_core=debug cargo run`
3. 检查 base_url 是否正确：`resolved.base_url` 应为 `https://api.openai.com/v1` 等
4. 检查 API key 是否有效：`curl -H "Authorization: Bearer $KEY" https://api.openai.com/v1/models`

**常见原因**:
- API key 过期或余额不足
- base_url 配置错误（如 OpenRouter 需要 `https://openrouter.ai/api/v1`）
- 模型 ID 不匹配 provider（如用 Anthropic key 调 OpenAI 模型）

### 1.3 Agent.send_message() 挂起

**症状**: 调用 `send_message()` 后程序无响应

**排查步骤**:

1. 确认是否在 `#[tokio::main]` 上下文中调用
2. 使用 `send_message_async()` 替代
3. 检查 `run_async()` helper 是否正确处理嵌套运行时

**根因**: `send_message()` 内部用 `SHARED_RUNTIME.block_on()`。在 tokio runtime 内部调用 `block_on()` 会死锁。`run_async()` helper 已修复此问题（用 `Handle::try_current()` + `block_in_place`）。

### 1.4 Gemini resolve 成功但 chat 失败

**症状**: `resolve("gemini-pro")` 成功，但 `chat()` 返回 `LatticeError::Config`

**根因**: `chat()` 只支持 `OpenAiChat` 和 `AnthropicMessages` 协议。Gemini 的 `GeminiGenerateContent` 协议未实现 chat 路径。

**临时方案**: 通过 OpenAI 兼容 provider（如 opencode-go）访问 Gemini 模型。

---

## 2. 调试工具

### 2.1 CLI 命令

```bash
# 查看模型解析详情
LATTICE debug sonnet

# 列出所有可用模型
LATTICE models

# 列出已认证的模型（有 API key 的）
LATTICE models --authenticated

# 运行健康检查
LATTICE doctor

# 验证 pipeline 配置
LATTICE validate pipeline-name
```

### 2.2 环境变量

| 变量 | 用途 |
|------|------|
| `RUST_LOG` | 日志级别：`lattice_core=debug` 查看详细 HTTP/SSE 日志 |
| `ANTHROPIC_API_KEY` | Anthropic 凭证 |
| `OPENAI_API_KEY` | OpenAI 凭证 |
| `GEMINI_API_KEY` | Gemini 凭证 |
| `DEEPSEEK_API_KEY` | DeepSeek 凭证 |
| `OPENROUTER_API_KEY` | OpenRouter 凭证 |
| `LATTICE_AGENTS_DIR` | Agent TOML 目录（默认 `~/.lattice/agents/`） |

### 2.3 日志系统

```rust
use lattice_core::init_debug_logging;
init_debug_logging(); // 输出到 stderr，级别由 RUST_LOG 控制
```

---

## 3. 已知 Bug 追踪

| ID | 描述 | 严重度 | 状态 |
|----|------|--------|------|
| P0-1 | Python API 只有 resolver | P0 | Open |
| P1-1 | ErrorClassifier 未贯通 streaming | P1 | Open |
| P1-2 | Gemini chat() 不支持 | P1 | Open |
| P1-3 | Agent.run() UTF-8 截断 | P1 | Open |
| P1-4 | InMemoryMemory 全局状态测试污染 | P1 | Open |
| P2-1 | bash 工具命令注入风险 | P2 | Open |
| P2-2 | resolve 无法区分 credentialless vs missing | P2 | Open |
| P2-3 | Agent memory/token_pool 未接入 | P2 | Open |
| P2-4 | extract_confidence() 默认 1.0 | P2 | Open |

完整问题清单见 [active-issues.md](../review/active-issues.md)。已解决问题见 [resolved-issues.md](../review/resolved-issues.md)。

---

## 4. Router Mutex 污染

**症状**: `router::tests` 中 8 个测试因全局 Mutex 污染而失败。

**根因**: `ModelRouter::credential_cache` 使用 `Mutex<HashMap>`。一个测试 panic 导致 Mutex 中毒，后续测试全部失败。

**临时方案**: `cargo test -p lattice-core -- --test-threads=1`

**长期方案**: 改用 `parking_lot::Mutex`（不会中毒）或 `DashMap`。
