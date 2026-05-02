# 全 async + Rust 原生改造规格

- **Spec 版本**: 0.1.0
- **日期**: 2026-05-02
- **范围**: lattice-agent / lattice-harness / lattice-plugin / lattice-cli / lattice-python
- **不动**: lattice-core（已 async + reqwest）

---

## 1. 改造目标

### 1.1 全 async

当前架构存在 sync→async 桥接层，需要通过 `block_on` / `SHARED_RUNTIME` 在 sync 函数内跑 async 代码。这不仅导致嵌套 runtime panic 风险，更让 iOS 嵌入场景完全不可用——iOS 主线程不允许 blocking 调用。

```
当前                                    →  目标
─────────────────────────────────────────────────────────────────
Agent::run() sync                       →  Agent::run() async（唯一路径）
    run_chat() sync (block_on 包装)      →    run_chat() async（直调）
    run_async() helper (block_in_place)  →    删除
    SHARED_RUNTIME (全局 LazyLock)       →    删除
    ToolExecutor::execute sync           →    ToolExecutor::execute async

Pipeline::run() sync                    →  Pipeline::run() async
PluginDagRunner::run() sync             →  PluginDagRunner::run() async
AgentRunner::run() sync                 →  AgentRunner::run() async
run_plugin_loop sync                    →  run_plugin_loop async
std::thread::sleep                      →  tokio::time::sleep

PluginAgent trait sync                  →  PluginAgent trait async
    send() sync                         →    send() async
    send_message_with_tools() sync      →    send_message_with_tools() async

MEMORY_RT (harness 全局)               →  删除，tokio::spawn 替代

CLI: run_pipeline() sync               →  run_pipeline() async
             lattice-python engine      →  外部 runtime 传入
```

### 1.2 Rust 原生

当前代码中仍存在"用 Rust 写 C"的习惯，主要是工具执行的 shell out 和错误处理的 String 类型。

```
当前                                    →   目标
─────────────────────────────────────────────────────────────────
tools.rs: grep via Command::new("grep") →  tokio::process::Command 或纯 Rust 搜索
tools.rs: bash via Command::new("sh -c")→  tokio::process::Command
tools.rs: web_search via reqwest::blocking → reqwest::Client .await
tools.rs: std::fs::read_to_string       →  tokio::fs::read_to_string
tools.rs: std::fs::read_dir             →  tokio::fs::read_dir
tools.rs: String return on errors       →  ToolError 枚举 (已存在，需集成)

PluginAgent trait                       →  错误类型从 Box<dyn Error> 改为具体枚举
    Result<String, Box<dyn Error>>      →    Result<String, AgentError>

Build.rs: python3-config Command        →  不动（构建脚本，不影响运行时）
```

### 1.3 嵌入式场景：Runtime 管理

LATTICE 被嵌入 iOS（或其他宿主）时，不能自己 `Runtime::new()`。

- 宿主应用已有自己的 tokio runtime（或者 Swift 的 Task 调度）
- LATTICE 应接受外部 `Handle`，或在无特殊配置时默认 auto-detect

```rust
// 宿主代码
let handle = tokio::runtime::Handle::current();
let lattice = LatticeBuilder::new()
    .with_runtime_handle(handle)
    .build()?;
lattice.ask("sonnet", "hello").await?;
```

设计原则：LATTICE 绝不自行创建 tokio `Runtime`，始终从外部获取 `Handle`。

---

## 2. 改造顺序（从下往上，每层可独立测试）

### Layer 0: Drive-by fixes

这些随改随修，不单独开任务：

- `tools.rs:print.rs`: grep CLI 子命令使用 `Command::new("grep")`，改为 `regex` crate 搜索
- `parse_utils.rs`: 提取的 JSON parse error 已附带 response preview（本次改动已有）

### Layer 1: ToolExecutor async（最关键的改动）

**改一个 trait + 8 个工具实现，消除所有 shell out 和 blocking IO。**

`lattice-agent/src/lib.rs`:

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

> 注：`#[async_trait]` 来自 `async-trait` crate，稳定且轻量。如果 tokio 原生支持 async trait（当前 nightly），将来可去掉。

**`lattice-agent/src/tools.rs` 改动：**

| 工具 | 当前 | 目标 |
|------|------|------|
| read_file | `std::fs::read_to_string` | `tokio::fs::read_to_string` |
| grep | `Command::new("grep")` | `tokio::process::Command` |
| write_file | `std::fs::write` | `tokio::fs::write` |
| list_directory | `std::fs::read_dir` | `tokio::fs::read_dir` |
| bash | `Command::new("sh -c")` | `tokio::process::Command` |
| patch | `std::fs::read_to_string` + `write` | `tokio::fs` |
| web_search | `reqwest::blocking::get` | 共享 `reqwest::Client` |

**PluginAgent trait 同时改 async：**

```rust
// 目标
#[async_trait]
pub trait PluginAgent {
    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;
    async fn send_message_with_tools(
        &mut self,
        message: &str,
    ) -> Result<String, Box<dyn std::error::Error>>;
    fn set_system_prompt(&mut self, _prompt: &str) {}
    fn token_usage(&self) -> u64 { 0 }
}
```

**验证标准：**
- `cargo test -p lattice-agent` 全通过
- `send_message_with_tools` 返回的字符串内容正确包含工具执行结果

### Layer 2: Agent::run() async 化

**改动文件：** `lattice-agent/src/lib.rs`

步骤分解：

**Step 2a: Agent::run() → async，删除 sync 版本**

当前 `run()` 和 `run_async()` 几乎完全重复（line 124-259），仅有 `run_chat()` vs `run_chat_async()` 的区别。ToolExecutor async 化后，sync 版本无意义。

```rust
// 目标：只有一个 async run()
pub async fn run(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent> {
    // 内容与原 run_async() 相同
    self.state.push_user_message(content);
    let mut all_events = Vec::new();
    for _ in 0..max_turns {
        // ...
        let mut events = self.run_chat_async().await;  // 直接 await，不 block_on
        // ...（和原来一样，但 tool_executor.execute() 需要 .await）
    }
}
```

**Step 2b: run_chat() sync → 删除**

`run_chat()` 是 sync 包装，内容 = `run_chat_async()` + `block_on` 包裹。删除，所有调用改调 `run_chat_async()`。

**Step 2c: 删除 SHARED_RUNTIME 和 run_async() helper**

```rust
// 删除的代码
static SHARED_RUNTIME: LazyLock<tokio::runtime::Runtime> = ...  // 删

fn run_async<F, T>(f: F) -> T { ... }  // 删
```

**Step 2d: ToolExecutor::execute() 调用改为 .await**

在 `Agent::run()` 中：
```rust
// 当前
let result = executor.execute(call);

// 目标
let result = executor.execute(call).await;
```

**验证标准：**
- `cargo test -p lattice-agent` 全通过
- `Agent::new(resolved).run("hello", 5).await` 可以编译

### Layer 3: Pipeline + AgentRunner async

**改动文件：** `lattice-harness/src/pipeline.rs`, `runner.rs`

Pipeline::run() 当前是 sync，内部创建 Agent 并调用 `runner.run()`（sync）。全 async 化后：

```rust
pub async fn run(&mut self, start_agent: &str, input: &str) -> PipelineRun {
    // ...（内容与当前相同）
    let mut runner = build_runner(&profile, resolved, self.shared_memory.clone());
    match runner.run(&current_input, agent_max_turns).await {  // .await
        // ...
    }
}
```

**AgentRunner::run() 同步改 async：**
```rust
pub async fn run(&mut self, input: &str, max_turns: u32) -> Result<serde_json::Value, AgentError> {
    let events = self.agent.run(input, max_turns).await;  // .await
    // ...
}
```

### Layer 4: PluginDagRunner async

**改动文件：** `lattice-harness/src/dag_runner.rs`

```rust
pub async fn run(
    &mut self,
    initial_input: &str,
    default_model: &str,
) -> Result<serde_json::Value, DAGError> {
    // resolve() 保持 sync（只读 env var，无 IO 阻塞）
    let resolved = lattice_core::resolve(model)?;
    let mut agent = Agent::new(resolved);
    // ...
    let result = run_plugin_loop(
        bundle.plugin.as_ref(),
        behavior.as_ref(),
        &mut agent,
        &context,
        &plugin_config,
        None,
        Some(&self.retry_policy),
        self.shared_memory.as_deref().map(|m| m as &dyn Memory),
    ).await?;  // .await
    // ...
}
```

**run_plugin_loop async：**

```rust
// 当前
pub fn run_plugin_loop(...) -> Result<RunResult, PluginError> {
    loop {
        let raw = agent.send_message_with_tools(&prompt)?;  // sync
        // ...
        std::thread::sleep(p.jittered_backoff(attempt));  // blocking
    }
}

// 目标
pub async fn run_plugin_loop(...) -> Result<RunResult, PluginError> {
    loop {
        let raw = agent.send_message_with_tools(&prompt).await?;  // async
        // ...
        tokio::time::sleep(p.jittered_backoff(attempt)).await;  // async
    }
}
```

### Layer 5: CLI 入口

**当前：** `run_pipeline()` sync，`run()` async（已调 `agent.run_async()`）

```rust
// 改动：run_pipeline sync → async
pub async fn run_pipeline(...) -> Result<()> {
    let mut pipeline = Pipeline::new(start_agent, registry, None, None);
    let result = pipeline.run(start_agent, prompt).await;  // .await
    // ...
}
```

`main.rs` 中 `Run::pipeline` 分支改为 `.await`。

### Layer 6: lattice-python（可选，影响 Python binding）

`lattice-python/src/engine.rs` 的 `SHARED_RUNTIME` 同样需要外部注入 Handle。

---

## 3. 内嵌场景的 Runtime 管理

核心原则：**LATTICE 从不自行创建 tokio Runtime。**

```rust
/// 全局存储宿主传入的 Handle，或 auto-detect。
static RUNTIME_HANDLE: std::sync::LazyLock<tokio::runtime::Handle> =
    std::sync::LazyLock::new(|| tokio::runtime::Handle::current());

/// 获取当前 Handle，优先用传入的，其次 auto-detect。
fn get_handle() -> tokio::runtime::Handle {
    RUNTIME_HANDLE.clone()
}

/// 可选：在构造阶段注入 Handle
pub struct LatticeBuilder {
    handle: Option<tokio::runtime::Handle>,
}
```

`#[tokio::main]` 场景下自动拿到当前 Handle，Swift 宿主需显式传入。

**iOS 接入伪代码：**
```swift
// Swift 侧
let lattice = LatticeEngine()
let handle = await lattice.runtimeHandle()  // 从 Rust 侧获取或传入
let result = await lattice.ask(model: "sonnet", prompt: "hello")
```

---

## 4. Rust 原生清理（随 Layer 1-2 顺便完成）

| 文件 | 改动 | 工具/类型 |
|------|------|----------|
| `tools.rs:71` | grep 改用 `tokio::process::Command` | `tokio::process::Command` |
| `tools.rs:135` | bash 改用 `tokio::process::Command` | `tokio::process::Command` |
| `tools.rs:195` | web_search 改用共享 Client | `reqwest::Client` 从 `provider.rs` 复用 |
| `tools.rs:52` | read_file 改用 tokio | `tokio::fs::read_to_string` |
| `tools.rs:87` | write_file 改用 tokio | `tokio::fs::write` |
| `tools.rs:110` | list_directory 改用 tokio | `tokio::fs::read_dir` |
| `tools.rs:156` | patch 改用 tokio | `tokio::fs::read_to_string` / `write` |
| `tools.rs` 全局 | 错误类型 | `ToolError` 枚举（已存在）替换 String |
| `print.rs:189` | grep CLI 命令 | 改为 `regex` Rust crate 搜索 |
| `lib.rs:464-476` | 重复代码消除 | `run_chat` sync 删除，只留 async 版 |

---

## 5. 验证计划

### 每层验证

```
Layer 1 完成后:
  cargo test -p lattice-agent                     # 15+ tests pass
  cargo clippy -p lattice-agent -- -D warnings     # 0 warnings
  # 手动验证: grep/bash/web_search 工具调用返回正确内容

Layer 2 完成后:
  cargo test -p lattice-agent                      # 全通过
  # 验证: SHARED_RUNTIME 和 run_async() 已删除
  # 验证: Agent::new(res).run("hello", 5).await 能编译

Layer 3 完成后:
  cargo test -p lattice-harness                    # 85+ tests pass
  # 验证: Pipeline::run() async 编译通过

Layer 4 完成后:
  cargo test -p lattice-plugin -p lattice-harness  # 全部通过
  ./target/debug/lattice run --pipeline plugin-test "test"  # 端到端 DAG

Layer 5 完成后:
  ./target/debug/lattice run -m deepseek-v4-flash "1+1=?"   # CLI 正常
  ./target/debug/lattice run --pipeline code-review "test"   # Pipeline 正常

Layer 6 (可选):
  cd lattice-python && maturin develop
  python3 -c "import lattice_core; print(lattice_core.resolve('sonnet'))"
```

### 全量验证

```bash
cargo test --workspace                    # 全部 crate
cargo clippy --workspace -- -D warnings    # 零警告
cargo build --target aarch64-apple-ios    # iOS 交叉编译验证
```

---

## 6. 不在此规格范围内

- **Sync blocking API**— 已决定只做 async。同步调用者通过 `tokio::runtime::Handle::block_on()` 自己处理
- **TUI 层** — `lattice-tui` 保持现有架构，其内部 tokio 已独立管理
- **结构化输出** — Phase 7
- **Provider 可插拔注册** — Phase 9
- **多模型并发调用** — Phase 10

---

## 7. 风险与缓解

| 风险 | 可能性 | 影响 | 缓解 |
|------|--------|------|------|
| async-trait 动态分发性能损耗 | 低 | 轻微 | 实测，如果显著则用 nightly async fn in trait |
| iOS 交叉编译依赖问题 | 中 | 中 | 先在本机编译验证，再推 iOS target |
| Pipeline Fork 并行逻辑需要特别处理 | 低 | 中 | Fork 目前用 `std::thread::spawn`，改 `tokio::spawn` |
| 外部 Handle 传入方案在 Swift 侧不易实现 | 中 | 中 | 降级方案：LATTICE 内部创建 Runtime，通过 channel 暴露 Handle |
| HAR-004: MEMORY_RT block_on 用于 memory writing，async 化后可能影响写入时序 | 低 | 低 | memory save 走 tokio::spawn，不做 await 返回确认 |
