# lattice-agent 工作指引

驱动单次模型请求尝试的流打开、取消与 deadline 控制及增量回调转发，被 `lattice-server` 的 Turn 协调层调用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 重导出 `ModelStepDriver`、`LlmClient` 与尝试请求、事件、结果类型 |
| `src/model_step.rs` | 从类型化历史组装 `ModelStepRequest`、轮询打开阶段取消与 deadline、转发流增量并映射 `ModelStepAttempt` |
| `src/testing.rs` | `test-utils` feature 启用的确定性 `LlmClient` 测试替身 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [Turn 的接受与模型步骤](../../docs/architecture/session/acceptance.md#模型请求与执行预算)、[模型接入](../../docs/architecture/llm/README.md#组成与归属)、[工程规范](../../docs/development/engineering.md) |
| 流增量回调与观察交接 | [重开、取消与未完成状态](../../docs/architecture/session/recovery.md#观察与重连) |
| 验证范围选择 | [测试工程](../../docs/development/testing.md) |

## 约束

- `Cargo.toml` 的工作区依赖限定为 `lattice-llm` 与 `lattice-conversation`，消费类型化流事件并产出类型化 `ModelStepAttempt`，使单次尝试驱动独立于存储和工具运行时。
- `ModelStepDriver` 在流打开阶段按固定间隔轮询取消与 deadline，`LlmClient` 实现把同一 `control` 交给 `lattice_llm::assemble_reply_stream` 并沿用同一轮询间隔处理流中取消与 deadline；打开失败返回 `ModelStepError::OpeningFailed`，流中取消、超时、限额或传输错误归入 `ModelStepAttempt`，以精确区分请求未打开与流中终止。
- `ModelStepDriver` 通过同步非阻塞回调交出与完整回复同源的显示增量，调用方先将正文与 reasoning 增量追加到当前尝试的段列表再唤醒观察者，保证通知合并时显示文字完整连续，见[观察与重连](../../docs/architecture/session/recovery.md#观察与重连)。
- 本 crate 之外：provider wire 协议编解码由 `lattice-llm` 负责；持久会话记录由 `lattice-session` 负责；模型循环与重试预算、`InvocationId` 分配、Tool 绑定与分发、槽位登记及 Step/Turn 关闭由 `lattice-server` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-agent` |
| 修改流接口、取消/deadline 或 `src/testing.rs` | `cargo test -p lattice-agent --features test-utils`（含 `tests/streaming_seam.rs`） |
| 修改请求组装或 `ModelStepAttempt` 结果语义 | `cargo test -p lattice-server`，并用当前构建的 `lattice` 二进制运行 `cargo test -p lattice-system-harness --test direct_failures` |
