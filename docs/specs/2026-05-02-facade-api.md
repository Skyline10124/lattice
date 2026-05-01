# lattice-core 门面 API 设计规格

## 版本

- **Spec 版本**: 0.1.0
- **目标版本**: lattice-core 0.2.0
- **日期**: 2026-05-02

## 设计原则

1. **零认知成本**: Rust 开发者不曾用过 LLM 库，加一行 dep 就能用 AI
2. **渐进复杂度**: 简单场景一句，复杂场景按需展开
3. **Feature flag 控制依赖**: 默认只有 `resolve + chat`，`streaming`/`tools`/`structured-output` 按需
4. **不破坏现有 API**: `resolve()` 和 `chat()` 保留不动，门面是上层封装

## API 分层

```
Layer 3: Lattice (门面)       ← 本次 spec 的范围
Layer 2: chat() / resolve()    ← 现有，不动
Layer 1: Provider / Transport  ← 现有，不动
```

## 目标 API

### Level 0: 一句推理（最简）

```rust
use lattice_core::Lattice;

// 同步（blocking）
let answer: String = Lattice::new("sonnet")
    .tell("用 Rust 写个快速排序")?;

// 异步
let answer: String = Lattice::new("sonnet")
    .tell_async("用 Rust 写个快速排序").await?;
```

`Lattice::new(model)` 自动 resolve → chat → 收集完整响应 → 返回 String。参数取默认值（temperature=0.7, max_tokens=4096）。

### Level 1: 带系统提示

```rust
let answer = Lattice::new("sonnet")
    .system("你是一个 Rust 专家，只输出代码不解释")
    .tell("用 Rust 写个快速排序")?;
```

### Level 2: 流式输出

```rust
let mut stream = Lattice::new("sonnet")
    .stream("解释什么是 Rust 的所有权模型")?;

while let Some(event) = stream.next().await {
    match event {
        StreamEvent::Token { content } => print!("{}", content),
        StreamEvent::Done { .. } => break,
        _ => {}
    }
}
```

`stream()` 返回 `Pin<Box<dyn Stream<Item = StreamEvent> + Send>>`，与现有 `chat()` 返回类型兼容。

### Level 3: 带工具

```rust
let answer = Lattice::new("sonnet")
    .tools(&[read_file_tool(), bash_tool()])
    .tell("读一下 README.md 的第一行")?;
```

工具调用由门面自动处理——内部走 agent loop：`send → 收到 ToolCall → 执行 → submit_tools → 继续`，用户不需要手动管 tool loop。

### Level 4: 完整配置

```rust
let response = Lattice::new("sonnet")
    .system("你是 Rust 专家")
    .temperature(0.3)
    .max_tokens(2048)
    .tools(&[search_tool()])
    .chat("这段代码有什么问题？")?;

println!("{}", response.content);
println!("{:?}", response.tool_calls);
println!("{} tokens used", response.usage.total_tokens);
```

`chat()` 返回 `ChatResponse`（与现有 `chat_complete()` 相同），包含 content/reasoning_content/tool_calls/usage/finish_reason。

## Feature Flag 设计

```toml
[dependencies]
lattice-core = "0.2"                        # resolve + chat + Lattice
lattice-core = { version = "0.2", features = ["streaming"] }  # + stream()
lattice-core = { version = "0.2", features = ["tools"] }      # + agent loop
lattice-core = { version = "0.2", features = ["full"] }       # 全都要
```

| feature | 解锁 | 新增依赖 |
|---------|------|---------|
| (无) | `Lattice::new().tell()` | reqwest, serde, serde_json |
| `streaming` | `Lattice::new().stream()` | futures |
| `tools` | `Lattice::new().tools().tell()` | (引入 agent crate 逻辑) |
| `tokens` | `response.usage` token 估算 | tiktoken-rs |
| `structured` | `Lattice::new().schema::<T>().tell()` | (provider-specific logic) |
| `full` | 以上全部 | — |

## Lattice struct 设计

```rust
pub struct Lattice {
    model: String,
    system_prompt: Option<String>,
    messages: Vec<Message>,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    tools: Vec<ToolDefinition>,
    // 内部状态
    resolved: Option<ResolvedModel>,
}
```

`Lattice::new(model)` 返回 builder 模式。调用 `tell()`/`stream()`/`chat()` 时触发 `resolve()` + 执行。

## 错误处理

```rust
pub enum LatticeError {
    Resolve(ModelNotFound),           // 模型未找到
    Auth(AuthenticationError),        // API key 缺失或无效
    RateLimit(RateLimitError),        // 额满
    ProviderUnavailable(String),      // 服务端不可用
    StreamLost(String),               // 流中断
    ToolFail { tool: String, error: String },
}
```

`tell()` 在成功时返回 `String`，流中断但有部分内容时也返回部分结果（不丢数据）。

## 与现有 API 的共存

现有 API 保持不变：
```rust
// 现有方式（兼容）
let resolved = lattice_core::resolve("sonnet")?;
let stream = lattice_core::chat(&resolved, &messages, &tools).await?;

// 新方式（门面）
let answer = Lattice::new("sonnet").tell("hello")?;
```

两者可以混用：先用 `Lattice::new()` 拿到 `resolved` 再走底层 `chat()`。

## 参考实现（伪代码）

```rust
impl Lattice {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system_prompt: None,
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: vec![],
            resolved: None,
        }
    }

    pub fn tell(&mut self, prompt: &str) -> Result<String, LatticeError> {
        let resolved = self.ensure_resolved()?;
        let messages = self.build_messages(prompt);
        let response = chat_complete(&resolved, &messages, &self.tools)?;
        Ok(response.content.unwrap_or_default())
    }

    fn ensure_resolved(&mut self) -> Result<&ResolvedModel, LatticeError> {
        if self.resolved.is_none() {
            self.resolved = Some(resolve(&self.model)?);
        }
        Ok(self.resolved.as_ref().unwrap())
    }
}
```

## 不在此 spec 范围内

- 结构化输出（`schema::<T>()`）— 放到 Phase 7 的插件系统里
- Provider 可插拔注册 — 放到 Phase 9 的统一网关
- 多模型并发调用 — 放到 Phase 10 的专家架构

---

## 现状 → 目标态 差距清单

> 以下标注 **actual** 为代码现状，**target** 为本 spec 目标。

### 已有（无需改动）

| 项 | 说明 |
|----|------|
| `lattice_core::resolve()` | 模型解析，无需改动 |
| `lattice_core::chat()` | SSE 流式推理，无需改动 |
| `lattice_core::chat_complete()` | 收集完整 ChatResponse，无需改动 |
| `types::Message / ToolDefinition / ToolCall / ChatResponse` | 类型系统现有，无需改动 |
| `errors::LatticeError` | 错误枚举现有，spec 中门面封装复用 |

### 需要新增

| # | 项 | 文件 | 描述 |
|---|-----|------|------|
| 1 | `Lattice` struct | `lattice-core/src/lattice.rs` | builder 结构体，含 model/system_prompt/temperature/max_tokens/tools 字段和内部 resolved 缓存 |
| 2 | `Lattice::new(model)` | 同上 | 构造函数 |
| 3 | `.system()` `.temperature()` `.max_tokens()` | 同上 | builder setter，返回 `&mut Self` |
| 4 | `.tools()` | 同上 | 设置工具列表 |
| 5 | `.tell(prompt) -> Result<String>` | 同上 | 同步推理，内部调 resolve + chat_complete |
| 6 | `.tell_async(prompt) -> impl Future<Output = Result<String>>` | 同上 | 异步版，用 tokio runtime |
| 7 | `.stream(prompt) -> Result<impl Stream>` | 同上 | 流式推理，内部调 resolve + chat |
| 8 | `.chat(prompt) -> Result<ChatResponse>` | 同上 | 完整推理，返回 ChatResponse |
| 9 | `build_messages()` 内部方法 | 同上 | 将 system_prompt + 历史 + 新 prompt 组装成 Vec<Message> |
| 10 | 更新 `lattice_core/src/lib.rs` | `lib.rs` | `pub mod lattice; pub use lattice::Lattice;` |
| 11 | Feature flag: `default` (无 streaming/tools) | `Cargo.toml` | 默认 dep 仅 reqwest + serde + serde_json |
| 12 | Feature flag: `streaming` | `Cargo.toml` | 引入 futures，解锁 `.stream()` |
| 13 | Feature flag: `tools` | `Cargo.toml` | 引入 agent loop 逻辑，解锁 `.tools()` |
| 14 | Feature flag: `full` | `Cargo.toml` | streaming + tools + tokens，全部 |

### 实现顺序建议

```
第1轮: Lattice struct + .new() + .tell() + .chat()  ← 最核心，不需要任何 feature flag
第2轮: .system() + .temperature() + .max_tokens()   ← builder 补全
第3轮: feature flag ("streaming") + .stream()        ← 流式
第4轮: feature flag ("tools") + .tools() + agent loop ← 工具自动执行
第5轮: feature flag ("full") + 单元测试               ← 收尾
```

### 验证标准

第一轮完成后，以下代码应可编译并返回推理结果：

```rust
use lattice_core::Lattice;

let answer = Lattice::new("sonnet").tell("1+1=?")?;
assert!(!answer.is_empty());
```
