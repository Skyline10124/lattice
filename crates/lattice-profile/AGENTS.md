# lattice-profile 工作指引

## 职责

维护单 Agent `agent.toml` 的发现、全局目录与文件权限检查、严格 schema 解析、领域字段构造、系统提示加载、默认模板，以及供 API/authority 消费的 LANCET 策略配置数据；依赖 `lancet-protocol`、`lattice-conversation` 与 `lattice-session`，不负责运行时编排、会话库持久化、CLI 或 Tool 执行。

`load.rs` 提供唯一的生产加载入口 `load_agent_profile(name, paths, expected_uid)` 与常规文件读取 `read_regular_file`，并负责全局 `agents` 目录与文件的属主、写权限与符号链接检查（`check_protected`）；`discovery.rs` 只定位项目与全局 profile 并识别来源；`schema.rs` 公开以领域类型（`AgentName`、`ModelRef`、`Effort`、`TurnTimeout`、`RetryPolicy`）表达的配置结构，原始 TOML 标量只留在内部私有解析结构中；`defaults.rs` 维护内置模板；`error.rs` 定义带路径、节名、字段名与原始值（或 TOML 行号）的公开错误；`lib.rs` 只重导出公开 profile 接口。

## 上位文档

修改前读取 [Cell 准入](../../docs/architecture/cell.md)、[会话配置与预算](../../docs/architecture/session.md)、[工作区策略](../../docs/architecture/workspace.md)及[测试工程](../../docs/development/testing.md)。

## 局部约束

- 生产代码统一经 `load_agent_profile` 一次完成发现、全局权限校验、严格反序列化、`[agent].name` 与所选 `AgentName` 一致性核对、领域字段构造及系统提示读取，不暴露绕过发现或权限检查的按路径加载入口。
- 未知、不支持的字段和 scalar 拼写错误必须被严格拒绝，不静默接受。每个新增配置节或表都要有严格反序列化测试；字段构造失败保留文件路径、节名、字段名与原始值诊断。
- 拒绝已退役的 `lancet.mode`、`[lancet.effects]`、顶层 `[workspace]`、`[lancet.isolation]` 与 `[lancet.command]`，不恢复兼容语义；`FORBIDDEN_SECTIONS` 预扫描提供明确诊断，模板只生成 `[lancet.workspace]` 策略节。
- `[tools].enabled` 是暴露与准入第一阶段。类型化 `[lancet.capabilities]` 的 `deny`、`max_boundary`（默认 `in`）与它共同转换为 `CapabilityAdmissionPolicy`；运行时组成拒绝未知 deny 名称，Component Descriptor 声明所需能力。
- schema 命名区分工具暴露与权限策略，仅为 profile 实际拥有的选择增加字段；策略数据保持与协议、authority 和 API 转换一致。
- canonicalization 失败返回 `ProfileError::SystemPathNotFound`，不回退不安全路径。全局 `agents` 目录下的目录、profile 与外部系统提示文件必须属于 `expected_uid`、与 `0o022` 无写权限交集且末段不是符号链接；缺失文件、含 symlink 的父路径和相对项目根解析的行为必须明确。
- 可选 `[llm].timeout_secs` 经 `TurnTimeout::new` 构造（`1..=86400`），重试参数经 `RetryPolicy::new` 构造，模型引用经 `ModelRef::parse` 构造，越界或格式无效保留字段级诊断。
- 外部策略文件 `[lancet.policy].path` 的路径随 profile 加载产出，供协调层按 Turn 组成解析，不能借此引入执行编排。

## 验证入口

- 运行 `cargo test -p lattice-profile`，覆盖严格解析、默认值、profile 发现与 LANCET 配置节。
- 改了策略配置时，再检查 authority 的默认值、API 的转换与 Turn 组成测试。
- 改了路径安全时，测试要覆盖路径缺失、symlink 前缀与拼写错误被拒绝。
