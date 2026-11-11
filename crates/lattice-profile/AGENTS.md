# lattice-profile 工作指引

负责单 Agent `agent.toml` 的发现、全局目录权限检查、严格 schema 解析、系统提示读取与默认模板生成，产出供 `lattice-api` 使用的 Agent 配置数据。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 重导出公开 profile 类型、默认模板与加载入口 |
| `src/load.rs` | 唯一生产加载入口 `load_agent_profile`、禁止顶层配置节预扫描、常规文件读取 `read_regular_file`，以及全局 `agents` 属主、写权限与末段符号链接检查 |
| `src/discovery.rs` | 项目 `.lattice/agents` 与全局 `agents` 目录定位及来源识别 |
| `src/schema.rs` | 私有严格反序列化结构及以领域类型公开的 `AgentProfile`（`AgentSection`、`LlmSection`） |
| `src/defaults.rs` | 内置默认 `agent.toml` 模板生成 |
| `src/error.rs` | 带文件路径、节名、字段名与原始值或 TOML 行号的 `ProfileError` |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [会话设置](../../docs/architecture/session/settings.md)、[Turn 的接受与模型步骤](../../docs/architecture/session/acceptance.md#执行配置冻结)、[工程规范](../../docs/development/engineering.md) |
| 全局 `agents` 权限与系统提示路径 | [配置来源与后台启动](../../docs/architecture/session/acceptance.md#配置来源与后台启动) |
| 字段数值与大小限制 | [会话存储与并发](../../docs/architecture/session/concurrency.md#输入与传输限制)、[测试工程](../../docs/development/testing.md) |

## 约束

- 生产代码统一通过 `load_agent_profile(name, paths, expected_uid)` 一次完成目录发现、全局权限校验、顶层配置节预扫描、严格反序列化、`[agent].name` 与目标 `AgentName` 一致性核对、领域字段构造及系统提示加载，使所有加载入口共享同一套安全边界。
- 私有 TOML 解析结构全部使用 `#[serde(deny_unknown_fields)]` 与强类型枚举拒绝未知字段和无效标量；`FORBIDDEN_SECTIONS` 预扫描按小写顶层键拦截禁止的 `[lancet]`、`[tools]`（给出从 `agent.toml` 删除该节的诊断）与 `[workspace]`、`[security]` 等顶层节，`defaults.rs` 生成的默认模板仅含 `[agent]`、`[prompt]` 与 `[llm.retry]`（[ADR 0009](../../docs/adr/0009-remove-legacy-lancet-tool-executor.md#6-profile-删除-lancet-与-tools)）。
- 公开 `schema.rs` 字段在解析边界经 `AgentName::parse`、`ModelRef::parse`、`TurnTimeout::new`（`1..=86400`）与 `RetryPolicy::new` 构造为领域类型，失败时在 `ProfileError` 中保留文件路径、节名、字段名与原始值或行号，见[配置拒绝的原因](../../docs/architecture/session/acceptance.md#配置拒绝的原因)与[输入与传输限制](../../docs/architecture/session/concurrency.md#输入与传输限制)。
- 系统提示路径规范化失败直接返回 `ProfileError::SystemPathNotFound`；位于全局 `agents` 目录下的目录、`agent.toml` 与外部系统提示文件均须满足属主等于 `expected_uid`、权限与 `0o022` 无交集且路径末段为非符号链接常规文件或目录，见[配置来源与后台启动](../../docs/architecture/session/acceptance.md#配置来源与后台启动)。
- 本 crate 的内部依赖仅限 `lattice-conversation` 与 `lattice-session`。
- 本 crate 之外：会话配置与执行协调由 `lattice-api` 负责；会话设置持久化由 `lattice-session` 负责；CLI 入口由 `lattice-cli` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-profile`（未知字段拒绝由 `deny_unknown_fields` 结构保证，测试锁定领域字段校验、禁止顶层节诊断、发现顺序及默认模板往返） |
| 修改全局权限或系统提示路径解析 | 在 `cargo test -p lattice-profile` 中覆盖路径缺失、父级符号链接放行、末段符号链接拒绝与 `0o022` 写权限拒绝 |
| 修改公开 profile 类型或错误诊断 | `cargo test -p lattice-api` |
