# lattice-profile 工作指引

## 职责

维护单 Agent `agent.toml` 的严格 schema、默认模板、发现、只读加载、allowlist 校验和供 API/authority 消费的纯转换数据，不负责运行时编排、会话图、CLI 或 Tool 执行。

`schema.rs` 维护配置类型，`defaults.rs` 维护内置模板，`discovery.rs` 定位 profile，`load.rs` 加载与校验，`error.rs` 定义公开错误；`lib.rs` 只重导出公开 profile 接口。

## 上位文档

修改前读取 [Cell 准入](../../docs/architecture/cell.md)、[会话配置与预算](../../docs/architecture/session.md)、[工作区策略](../../docs/architecture/workspace.md)及[测试工程](../../docs/development/testing.md)。

## 局部约束

- 未知、不支持的字段和 scalar 拼写错误必须被严格拒绝，不静默接受。每个新增配置节或表都要有严格反序列化测试。
- 拒绝已退役的 `lancet.mode`、`[lancet.effects]`、顶层 `[workspace]`、`[lancet.isolation]` 与 `[lancet.command]`，不恢复兼容语义；`FORBIDDEN_SECTIONS` 预扫描提供明确诊断，模板只生成 `[lancet.workspace]` 策略节。
- `[tools].enabled` 是暴露与准入第一阶段。类型化 `[lancet.capabilities]` 的 `deny`、`max_boundary`（默认 `in`）与它共同转换为 `CapabilityAdmissionPolicy`；运行时组成拒绝未知 deny 名称，Component Descriptor 声明所需能力。
- schema 命名区分工具暴露与权限策略，仅为 profile 实际拥有的选择增加字段；策略数据保持与协议、authority 和 API 转换一致。
- canonicalization 失败返回 `ProfileError::SystemPathNotFound`，不回退不安全路径。缺失文件、含 symlink 的父路径和相对项目根解析的行为必须明确。
- 可选 `[llm].timeout_secs` 为 `1..=3600`，越界保留字段级诊断。
- 外部策略文件 `[lancet.policy].path` 的加载数据供按 Turn 组成使用，不能借此引入执行编排。

## 验证入口

- `cargo test -p lattice-profile`，覆盖严格解析、默认值、发现及 LANCET 配置节。
- 策略配置变更关联 authority 默认值与 API 转换、Turn 组成测试。
- 路径安全变更覆盖缺失、symlink 前缀和拼写错误拒绝。
