# lattice-agent 工作指引

## 职责

`ModelStepDriver` 驱动一次 provider attempt：从类型化历史组装请求，按 provider `id`、模型 `id` 与推理档位（推理档位待实现，由 #315 L4 承接）经 `lattice-llm` 的请求网关打开并消费流，把取消与 deadline 交给共同流包装器，通过同步非阻塞回调转交同一次累积的显示增量并返回类型化结果。调用方在回调中先把正文与 reasoning delta 追加到当前尝试已生成的文字（会话合同称为段列表），再通知观察者；通知可以合并，显示用的文字不会因此缺失。交接规则见[观察与重连](../../docs/architecture/session.md#观察与重连)。

- `src/model_step.rs`：请求组装、打开阶段的取消与 deadline、流事件到 attempt 结果的映射。
- `src/testing.rs`：feature 控制的确定性 provider 响应。
- `tests/streaming_seam.rs`：公开可注入流接口。

## 上位文档

修改前读取[会话合同](../../docs/architecture/session.md)的模型请求、执行预算及事件语义，以及[模型接入合同](../../docs/architecture/llm.md#组成与归属)的请求网关；验证选择遵循[测试工程](../../docs/development/testing.md)。

## 边界

- 工作区依赖仅限 `lattice-llm` 和纯数据层 `lattice-conversation`；不引入 session、LANCET runtime、profile 或 store。
- 模型循环与重试策略、InvocationId 分配、Tool 绑定和分发、槽位登记与终态写入、Step/Turn 关闭由 `lattice-api::TurnCoordinator` 负责。
- 打开阶段由驱动轮询取消与 deadline。`LlmClient` 实现必须把 `control` 交给 `assemble_reply_stream`，流中的取消与 deadline 由包装器处理；两阶段使用同一轮询间隔。opening 失败保留 `ModelStepError::OpeningFailed`；流中取消、deadline 或错误进入类型化 `ModelStepAttempt`。
- provider wire 细节留在 `lattice-llm`，本层消费类型化事件；live 事件不是持久化权威。

## 验证入口

- 运行 `cargo test -p lattice-agent`。
- 测试设施要启用 feature：`cargo test -p lattice-agent --features test-utils`。
- 改了请求或结果时，再运行相关的 API Turn 测试，并用当前构建的二进制运行 harness 场景 `direct_failures`。
