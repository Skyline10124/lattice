# 快速开始

## 30 秒

```bash
cargo build --release
./target/release/lattice run "1+1=?" -m deepseek-v4-flash
# → 1+1=**2**
```

## 你需要

- Rust 1.80+
- 至少一个 API key（环境变量）

```bash
export DEEPSEEK_API_KEY="***"
export ANTHROPIC_API_KEY="***"
export OPENAI_API_KEY="***"
# ... 或其他任一 provider
```

## 代码中使用

```rust
// 底层 API — 直接推理
use lattice_core::{resolve, chat_complete, types::*};

let resolved = resolve("sonnet")?;
let messages = vec![Message::user("Hello!")];
let response = chat_complete(&resolved, &messages, &[]).await?;
println!("{}", response.content.unwrap_or_default());

// Agent API — 带工具执行
use lattice_agent::{Agent, DefaultToolExecutor};

let resolved = resolve("sonnet")?;
let mut agent = Agent::new(resolved)
    .with_tools(default_tool_definitions())
    .with_tool_executor(Box::new(DefaultToolExecutor::new(".")));
let events = agent.run("读一下 Cargo.toml", 10).await;
```

> 所有 API 都是 async。从 `#[tokio::main]` 或任何 tokio runtime 上下文中调用。

## Pipeline（插件 DAG）

```bash
# 创建 agent profile 并运行 pipeline
lattice run --pipeline code-review "审查一下 src/main.rs"

# 使用 plugin DAG 编排
lattice run --pipeline plugin-test "分析一下代码"
```

## CLI 常用命令

```bash
# 列出已加载的 agent
lattice list agents

# 会话管理
lattice sessions list
lattice sessions show <id>

# 会话统计
lattice stats

# 配置管理
lattice config init
lattice config get core.default_model
lattice config set core.default_model gpt-4

# 继续上一次会话
lattice run -c "继续上次的话题" -m sonnet

# Debug 模式（model 为位置参数，--prompt 为可选）
lattice debug sonnet --resolve-only
lattice debug deepseek-v4-flash --prompt "Hello"

# Bus 状态
lattice bus
```

## Python

```bash
cd lattice-python && maturin develop
```

```python
import lattice_core
engine = lattice_core.LatticeEngine()
resolved = engine.resolve_model("sonnet")
# Python 目前只支持 resolve，chat 走 Rust
```

## 下一步

- [第一个调用](first-call.md)
- [安装详情](installation.md)
