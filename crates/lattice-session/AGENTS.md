# lattice-session 工作指引

维护全局会话库 `lattice.redb`、会话树同步领域事务、由持久进度推导的结论、历史与轮次详情只读查询及共享领域字段类型，被 `lattice-profile`、`lattice-server` 与 `lattice-cli` 使用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 重导出领域字段类型、`SessionStore` 读写接口及 `test-utils` 测试辅助 |
| `src/model/mod.rs`、`src/model/project_path.rs` | 独立于存储机制的纯数据领域类型（`AgentName`、`TurnTimeout`、`RetryPolicy`、`ProjectPath`）与构造校验 |
| `src/store/mod.rs`、`src/store/types.rs` | `SessionStore`（含幂等写闸 `disable_writes`）、`SessionRead`、`SessionExecutionLock`、`ToolReply`、槽位详情状态（`SlotDetail`、`SlotState`、`SlotReason`）与错误 |
| `src/store/binding.rs` | 调用槽位绑定事实（`InvocationBinding`、`BindingIdentity`），经 `store` 导出 |
| `src/store/codec.rs`、`src/store/open.rs`、`src/store/validate.rs` | redb 表与格式版本 `9` 编解码、首次创建与严格打开、存在 `sessions.git` 或 `invocations.git` 时的拒绝及全库跨记录一致性校验 |
| `src/store/write.rs`、`src/store/close.rs` | 单步 `Durability::Immediate` 领域写事务（含草稿会话整体删除 `delete_draft`），`ToolReply` 终态准备（`prepare_terminal`），以及从持久 `progress` 推导尝试、槽位与 Step 结论的纯函数 |
| `src/store/read.rs`、`src/store/local.rs` | 会话树、过期草稿会话候选扫描、当前路径对话条目、轮次序号、按轮分页、轮次详情与用量只读投影，以及按会话或 Turn 范围加载记录 |
| `src/store/settings.rs`、`src/store/config.rs`、`src/store/hex_digest.rs` | 会话设置（`SessionSettings`、`SettingsVersion`、`EffectiveProfile`）、系统提示与工具目录（`tool_identity`）的内容摘要配置表及十六进制摘要编码 |
| `src/store/tests.rs` | 会话库编码、校验、事务推进与读取投影单元测试 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [会话与 Turn](../../docs/architecture/session/README.md)、[私有存储与持久化](../../docs/architecture/storage.md#会话库)、[工程规范](../../docs/development/engineering.md) |
| Step 预留、调用槽位与关闭结论 | [Step 执行与结果保存](../../docs/architecture/session/steps.md)、[重开、取消与未完成状态](../../docs/architecture/session/recovery.md#执行者消失后的-turn) |
| 事务、锁与故障重开 | [会话存储与并发](../../docs/architecture/session/concurrency.md) |
| 会话设置与用量查询 | [会话设置](../../docs/architecture/session/settings.md)、[当前路径的用量查询](../../docs/architecture/session/operations.md#当前路径的用量查询) |
| 存储演化背景与验证分层 | [ADR 0006](../../docs/adr/0006-tree-session-and-global-redb-store.md)、[ADR 0009](../../docs/adr/0009-remove-legacy-lancet-tool-executor.md)、[测试工程](../../docs/development/testing.md) |

## 约束

- `SessionStore` 封装全部 redb 表与 `Durability::Immediate` 写事务，对外只提供窄领域接口；测试辅助限定在 `#[cfg(test)]` 或 `test-utils` feature 下供下游 `dev-dependencies` 启用，确保生产路径全部经领域事务读写，见[会话库](../../docs/architecture/storage.md#会话库)。
- `Session` 在创建时永久绑定调用方传入的规范项目路径 `ProjectPath`，见[会话库与项目绑定](../../docs/architecture/session/concurrency.md#会话库与项目绑定)。
- `open` 执行全库严格核验：旧格式库与任何损坏的会话、类型、值版本、规范 JSON 或绑定摘要均整库拒绝；派生索引（`turn_index`、`invocation_index`）随节点权威记录在同一写事务更新，见[身份、版本与打开](../../docs/architecture/storage.md#身份版本与打开)与[事务与一致性](../../docs/architecture/session/concurrency.md#事务与一致性)。
- 已使用会话的树只追加节点，每次推进以单个写事务改写末端未关闭的 Step，结论只由节点已保存的 `progress` 推导；过期草稿会话由 `delete_draft` 在单写事务中复核资格并整体删除；查询保持纯只读，退回只移动会话当前节点指针，见[预留、progress 与结论](../../docs/architecture/session/README.md#预留progress-与结论)、[表、读取范围与 rewind](../../docs/architecture/session/README.md#表读取范围与-rewind)与[会话状态与草稿清理](../../docs/architecture/session/concurrency.md#会话状态与草稿清理)。
- 写事务内只做内存编码与提交，同步接口由异步调用方放入 blocking 线程执行；锁范围与提交失败后的停写重开见[事务与一致性](../../docs/architecture/session/concurrency.md#事务与一致性)与[锁与并发语义](../../docs/architecture/storage.md#锁与并发语义)。
- 调用槽位绑定（`InvocationBinding`、`BindingIdentity`）由本 crate 的 `store` 模块定义并维护，`tool_identity` 为不透明字符串；持久身份、规范 JSON 与参数摘要经 `lattice-conversation` 取得（[ADR 0009](../../docs/adr/0009-remove-legacy-lancet-tool-executor.md#5-身份与规范-json-归-lattice-conversation)）。
- 轮次与槽位详情查询只依赖存储记录，`SlotDetail` 仅投影调用身份、槽位状态、结局、机器错误码与内容摘要元数据（排除参数、结果正文、错误消息原文、schema 与冻结秘密），见[调用槽位](../../docs/architecture/session/steps.md#调用槽位)。
- 本 crate 之外：对话消息、身份、规范 JSON、模型选择值与用量数据结构由 `lattice-conversation` 负责；profile 发现与解析由 `lattice-profile` 负责；provider 请求、Turn 协调、工具执行接口与 CLI 分别由 `lattice-llm`、`lattice-server` 和 `lattice-cli` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-session` |
| 修改依赖或 `test-utils` 特性边界 | `cargo xtask harness-boundary` |
| 修改记录编码、会话树、事务或读取投影 | `cargo test -p lattice-server`、`cargo test -p lattice-cli`，并用当前构建的 `lattice` 二进制运行 `cargo test -p lattice-system-harness --test direct_tree_session` |
| 修改提交阶段或崩溃后结论推导 | 构建带 `--features lattice-server/test-fake-tools,lattice-server/test-crash-barriers` 的 `lattice` 二进制并运行 `cargo test -p lattice-system-harness --features fake-tools,crash-barriers --test direct_crash_barriers` 验证真实提交窗口 |
