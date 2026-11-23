# lattice-context 工作指引

定义纯同步、零 I/O 的模型上下文领域层：来源通道 `Channel`、规范化输入 `ContextInput`、强类型操作 `ContextOp`/`ContextEdit`，以及单遍归约 `resolve()`、校验 `validate_op()`、客观统计 `inspect()` 与模型投影 `project()`（见 [ADR 0014](../../docs/adr/0014-model-context-domain.md)）。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 声明 `#![forbid(unsafe_code)]`，保持子模块私有，并在根模块扁平重导出全部公开类型、常量与函数 |
| `src/channel.rs` | 开放受检来源标识 `Channel`、`MAX_CHANNEL_BYTES`（256 字节）与 `ChannelError` 严格解析和 serde 校验 |
| `src/types.rs` | `NormalizedUserMessage`、`ContextText`、`ToolResultText`、`ContextInput`、`ContextOp`、`ContextEdit` 及其错误类型与便捷构造 |
| `src/resolve.rs` | `PathRecord`、`PathNodeKind`、`ContextError`、`EffectiveContext`、单遍扫描归约 `resolve()`、`validate_op()` 与统一 `Step` 局部编辑清洗路径 |
| `src/inspect.rs` | `NodeStatus`、`OperatedNode`、`EffectiveEntrySummary`、`ContextInspection` 与纯客观统计 `EffectiveContext::inspect()` |
| `src/project.rs` | 模型历史投影 `EffectiveContext::project()`、`<lattice-context channel="...">` 属性转义与正文闭合标记转义 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [ADR 0014](../../docs/adr/0014-model-context-domain.md)、[工程规范](../../docs/development/engineering.md) |
| 依赖边界与纯数据白名单 | [Crate 职责与依赖边界](../../docs/architecture/crates.md#依赖边界门禁) |
| 用户输入规范化与回放签名规则 | [对话数据与模型投影](../../docs/architecture/session/conversation.md#用户图片与单轮输入规范化)、[回放与跨 provider 投影](../../docs/architecture/session/conversation.md#回放与跨-provider-投影) |

## 约束

- `Cargo.toml` 仅允许依赖 `lattice-conversation`、`serde`、`serde_json`、`thiserror`，零 I/O、零 `async`、零存储、零 `unsafe`（`#![forbid(unsafe_code)]`）。
- 直接复用 `lattice-conversation` 的 `NodeId`、`ImageRef`、`normalize_user_message`、`canonical_json`、`tool_content_digest` 与上限常量，不定义平行的规范化逻辑或上限副本。
- `resolve()` 与 `validate_op()` 共用同一套归约与应用规则，并坚守纯函数确定性、协议成对铁律、回放签名铁律与同路径引用铁律。
- `inspect()` 只产出客观字节与结构统计，不混入 `32 MiB` wire 预算或模型图片模态检查（继续归 `lattice-llm::ResolvedRequest::check_request`）。
- `project()` 不拼接 `system_prompt`（由 `ModelStepRequest::chat_request` 置顶拼接）。
- `tests/context_golden.rs` 锁定 `Channel`、强类型 serde 形状、归约与清洗不变量、`inspect()` 统计与 `project()` 标记转义的逐字节行为。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-context` |
| 修改依赖项 | `cargo xtask crate-boundary` |
| 修改文档或链接 | `cargo xtask docs-check` |
