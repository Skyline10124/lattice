# 快速开始

## 30 秒

```bash
cd artemis-core && cargo build --release
```

```rust
let resolved = artemis_core::resolve("sonnet")?;
let response = artemis_core::chat_complete(&resolved, &messages, &[])?;
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
cd artemis-python && maturin develop
```

```python
import artemis_core
engine = artemis_core.ArtemisEngine()
resolved = engine.resolve_model("sonnet")
# Python 目前只支持 resolve，chat 走 Rust
```

## 下一步

- [第一个调用](first-call.md)
- [安装详情](installation.md)
