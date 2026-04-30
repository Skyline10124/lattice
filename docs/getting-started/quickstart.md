# 快速开始

## 30 秒

```bash
cd lattice-core && cargo build --release
```

```rust
let resolved = lattice_core::resolve("sonnet")?;
let response = lattice_core::chat_complete(&resolved, &messages, &[])?;
```

## 你需要

- Rust 1.80+
- 至少一个 API key（环境变量）

```bash
export DEEPSEEK_API_KEY="sk-xxx"
export ANTHROPIC_API_KEY="sk-xxx"
export OPENAI_API_KEY="sk-xxx"
# ... 或其他任一 provider
```

## Python

```bash
cd lattice-python && maturin develop
```

```python
import lattice_core
engine = lattice_core.ArtemisEngine()
resolved = engine.resolve_model("sonnet")
# Python 目前只支持 resolve，chat 走 Rust
```

> **注意**: `Agent.send_message()` 目前需要 `#[tokio::main]`。同步代码请使用 `lattice_core::chat_complete()` 配合 `futures::executor::block_on()` 和 tokio runtime。

## 下一步

- [第一个调用](first-call.md)
- [安装详情](installation.md)
