# 全 async + Rust 原生改造 — 设计文档

- **日期**: 2026-05-02
- **来源 spec**: `docs/specs/2026-05-02-async-rust-native-refactor.md` v0.1.0
- **范围**: lattice-agent / lattice-harness / lattice-plugin / lattice-cli
- **不动**: lattice-core（已 async + reqwest）、lattice-tui、lattice-python（可选 Layer 6）

---

## 1. 设计目标

代码质量驱动：消除 sync→async 桥接层（`SHARED_RUNTIME` + `run_async()` + `MEMORY_RT`），用纯 Rust 替代 shell-out 工具，统一错误类型。

### 1.1 当前问题

```
Agent::run() sync
  → run_chat() sync (block_on 包装)
    → run_chat_async() async（真正的实现）
  → ToolExecutor::execute() sync（blocking IO）
  → std::thread::sleep（阻塞）
  → SHARED_RUNTIME（全局 LazyLock<Runtime>）
  → MEMORY_RT（harness 全局）
```

### 1.2 目标架构

```
CLI (sync)
  → Pipeline::run() sync → SYNC_RT.block_on(...)
    → AgentRunner::run().await
      → Agent::run().await
        → ToolExecutor::execute().await（async IO）
        → tokio::time::sleep().await
```

全程无 `run_async()` / `block_in_place` / `SHARED_RUNTIME`。

---

## 2. 五层改造

### Layer 1: ToolExecutor async + Rust 原生工具

**Trait 变更：**

```rust
// 当前
pub trait ToolExecutor: Send + Sync {
    fn execute(&self, call: &ToolCall) -> String;
}

// 目标
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> String;
}
```

**7 个工具：**

| # | 工具 | 当前 | 目标 |
|---|------|------|------|
| 1 | read_file | `std::fs::metadata` + `std::fs::read_to_string` | `tokio::fs::metadata` + `tokio::fs::read_to_string` |
| 2 | grep | `std::process::Command::new("grep")` | `regex` crate + `tokio::fs::read_dir` 递归搜索 |
| 3 | write_file | `std::fs::write` | `tokio::fs::write` |
| 4 | list_directory | `std::fs::read_dir` | `tokio::fs::read_dir`，sort 在 Vec 收集后 |
| 5 | bash | `std::process::Command::new("sh")` | `tokio::process::Command::new("sh")` |
| 6 | patch | `std::fs::read_to_string` + `std::fs::write` | `tokio::fs::read_to_string` + `tokio::fs::write` |
| 7 | web_search | `reqwest::blocking::get` | 共享 `reqwest::Client`（`DefaultToolExecutor` 加 `http_client` 字段） |

**grep 纯 Rust 实现：**
- 输入: `pattern` (正则), `path` (目录或文件)
- 逻辑: `tokio::fs::read_dir` 递归 + `regex::Regex::is_match` 逐行
- 输出: `"file:line_num:content"`（与 grep -rn 兼容）
- 约束: max_depth=32, 跳过 >1MB 文件, 跳过含 null 字节文件, visited set 防符号链接循环

**错误类型（内部，不暴露到 trait）：**

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    SandboxViolation(String),
    IoError { path: String, error: std::io::Error },
    RegexError(String),
    HttpError(String),
    CommandError(String),
    SizeLimit { limit: usize, actual: usize },
    FileNotFound(String),
}
```

`ToolError` 通过 `Display` 转 String 返回，不改 `execute()` 签名。

**Agent 内部 bridge：** Layer 1 阶段，`Agent::run()` 保持 sync，`executor.execute()` 通过现有 `run_async()` 桥接：

```rust
let result = run_async(executor.execute(call));
```

**PluginAgent trait 同时 async：**

```rust
#[async_trait]
pub trait PluginAgent {
    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;
    async fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;
    fn set_system_prompt(&mut self, _prompt: &str) {}
    fn token_usage(&self) -> u64 { 0 }
}
```

MockAgent 测试自动兼容（方法体无 `.await`，`#[async_trait]` 处理 boxing）。

**新增依赖：** `lattice-agent/Cargo.toml` — `async-trait = "0.1"`, `regex = "1"`

**验证：** `cargo test -p lattice-agent`（MockAgent 测试兼容）

---

### Layer 2: Agent::run() async + SHARED_RUNTIME 删除

**改动清单：**

| 操作 | 项 |
|------|-----|
| 删除 | `SHARED_RUNTIME`（全局 LazyLock） |
| 删除 | `run_async()` helper（block_in_place/block_on 包装） |
| 删除 | `run_chat()` sync 版本（仅是 `run_chat_async` + block_on） |
| 删除 | `Agent::run()` sync 版本 |
| 重命名 | `run_chat_async()` → `run_chat()` |
| 重命名 | `run_async()` async → `run()` |
| 修改 | `send_message()` → 调 `run_chat().await` |
| 修改 | `submit_tools()` → async |
| 修改 | `run_plugin_loop` → `send_message_with_tools().await` |

**Agent::run() 最终形态：**

```rust
pub async fn run(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent> {
    self.state.push_user_message(content);
    let mut all_events = Vec::new();

    for _ in 0..max_turns {
        self.state.trim_messages(context_len, 15);
        let mut events = self.run_chat().await;  // 直接 .await
        // retry loop 不变
        // executor.execute(call).await（Layer 1 已 async）
    }
    all_events
}
```

**Upper cascade：**

```
Agent::run() async
  ├─ AgentRunner::run() → async（await agent.run()）
  ├─ PluginRunner::run() → async（await agent.send_message_with_tools()）
  ├─ Pipeline::run_async() → 去掉内部 run_async() 包装
  ├─ Pipeline::run() sync → SYNC_RT.block_on(self.run_async(...))
  └─ MicroAgent::register/start → 局部 RT.block_on() 替代 MEMORY_RT
```

**三套 bridge 处理：**

| Bridge | 处理 |
|--------|------|
| `SHARED_RUNTIME` + `run_async()` | 删除 |
| `MEMORY_RT` | 删除，MicroAgent 内局部 `static RT: LazyLock<Runtime>` 替代 |
| `futures::executor::block_on`（测试） | 不动 |

**MicroAgent 方案：** 保持 sync 方法，内部用局部 lazy Runtime：

```rust
// micro_agent.rs
use std::sync::LazyLock;
static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("tokio runtime")
});

pub fn register(&self, event: PipelineEvent) {
    RT.block_on(self.bus.register(event));
}
```

**Pipeline::run() sync wrapper：**

```rust
static SYNC_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("tokio runtime")
});

pub fn run(&mut self, start_agent: &str, input: &str) -> PipelineRun {
    SYNC_RT.block_on(self.run_async(start_agent, input))
}
```

**Fork 并行：** `Pipeline::run_async()` 已用 `tokio::spawn`，`std::thread::spawn` 分支删除。

**新增依赖：** `lattice-harness/Cargo.toml` — `async-trait = "0.1"`
**新增依赖：** `lattice-plugin/Cargo.toml` — `tokio = { version = "1", features = ["time"] }`

**验证：** 
- `cargo test -p lattice-agent`（SHARED_RUNTIME 已删除）
- `cargo test -p lattice-harness`（MEMORY_RT 已删除）

---

### Layer 3: AgentRunner + Pipeline 收尾

纯机械改动，无决策点：
- `AgentRunner::run()` async（可能已在 Layer 2 完成）
- `Pipeline::run_async()` 去掉内部 `run_async()` 桥接
- `run_fork()` sync 删除
- `MEMORY_RT` 删除

**验证：** `cargo test -p lattice-harness`

---

### Layer 4: PluginDagRunner + run_plugin_loop async

```rust
pub async fn run_plugin_loop(...) -> Result<RunResult, PluginError> {
    loop {
        let raw = agent.send_message_with_tools(&prompt).await?;
        // ...
        tokio::time::sleep(p.jittered_backoff(attempt)).await;
    }
}
```

**验证：** `cargo test -p lattice-plugin -p lattice-harness`
`cargo run -- run --pipeline code-review "test"`

---

### Layer 5: CLI 入口

- `run_pipeline()` async → 内部 `pipeline.run_async().await`
- `main.rs` 中 `Run::pipeline` 分支 → `.await`

**验证：**
- `cargo run -- "1+1=?"`
- `cargo run -- run --pipeline code-review "test"`

---

## 3. 依赖变更

| Crate | 新增 |
|-------|------|
| `lattice-agent` | `async-trait = "0.1"`, `regex = "1"` |
| `lattice-plugin` | `tokio = { version = "1", features = ["time"] }` |
| `lattice-harness` | `async-trait = "0.1"` |

---

## 4. 不在此设计范围内

- lattice-tui — 保持现有架构，tokio 独立管理
- lattice-python — Layer 6，可选
- 结构化输出 — Phase 7
- Provider 可插拔注册 — Phase 9
- Facade API (`Lattice` struct) — 独立 spec: `docs/specs/2026-05-02-facade-api.md`

---

## 5. 风险

| 风险 | 缓解 |
|------|------|
| `#[async_trait]` 动态分发性能 | trait object 已存在（Box<dyn ToolExecutor>），额外 boxing 可忽略 |
| grep 纯 Rust 实现与 grep -rn 语义差异 | 保持输出格式兼容，LLM 只读内容不 parse 格式 |
| Pipeline Fork 行为变化 | `run_fork_async` 已有测试，`tokio::spawn` 比 `std::thread::spawn` 更安全 |
| MockAgent 测试破坏 | `#[async_trait]` 对无 `.await` 方法体透明 |
