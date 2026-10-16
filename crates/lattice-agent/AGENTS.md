# lattice-agent 工作指引

## 职责

`ModelStepDriver` 驱动一次 provider attempt：从类型化历史组装冻结请求、打开并消费流、执行协议上限与计量，通过同步非阻塞回调发送观察事件并返回类型化结果。调用方在任何有损通知之前维护权威段列表，交接规则见[会话观察](../../docs/architecture/session.md#观察与重连)。

- `src/model_step.rs`：请求投影、流轮询、取消与 deadline、计量和 attempt 组装。
- `src/testing.rs`：feature 控制的确定性 provider 响应。
- `tests/streaming_seam.rs`：公开可注入流接口。

## 上位文档

修改前读取[会话合同](../../docs/architecture/session.md)的模型请求、执行预算及事件语义；验证选择遵循[测试工程](../../docs/development/testing.md)。

## 边界

- 工作区依赖仅限 `lattice-llm` 和纯数据层 `lattice-conversation`；不引入 session、LANCET runtime、profile 或 store。
- 模型循环与重试策略、InvocationId 分配、Tool 绑定和分发、槽位登记与终态写入、Step/Turn 关闭由 `lattice-api::TurnCoordinator` 负责。
- 取消与 deadline 同时覆盖流打开和消费。opening 失败保留 `ModelStepError::OpeningFailed`；流中取消、deadline 或错误进入类型化 `ModelStepAttempt`。
- provider wire 细节留在 `lattice-llm`，本层消费类型化事件；live 事件不是持久化权威。

## 验证入口

- `cargo test -p lattice-agent`。
- 测试设施：`cargo test -p lattice-agent --features test-utils`。
- 请求或结果变化：相关 API Turn 测试及使用当前二进制的 `direct_failures` 场景。
