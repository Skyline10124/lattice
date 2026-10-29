---
status: active
---

# 工具开发与操作

## 目的与范围

本指南说明仓库内 Source Tool 开发、profile 配置和操作员检查。工具组成与调用规则由 [LANCET 合同](../architecture/lancet.md)维护，权限由 [Cell 合同](../architecture/cell.md)维护，文件效果由[工作区合同](../architecture/workspace.md)维护。外部 Bundle 安装、宿主自动重建与活动版本切换仍是未实现的[安装设计](../architecture/lancet.md#bundle-安装设计)，不能按下面的仓内构建命令推定已提供安装产品。

## 开发来源与生成

每个 `tools/<tool>/manifest.toml` 是该 Tool 的参数、结果、Cell、imports 与身份维护源。先修改 manifest 和对应源码，再生成并核验派生材料，不单独维护第二份 schema。

```sh
cargo xtask tool-codegen
cargo xtask tool-codegen --check
cargo xtask tool-build
cargo xtask tool-build --verify
cargo xtask lancet-boundary
```

生成与构建入口分别是 [`tool_codegen.rs`](../../xtask/src/tool_codegen.rs) 和 [`tool_build.rs`](../../xtask/src/tool_build.rs)。这些命令会写生成物或构建产物；执行前确认本次授权和现有修改。工具链安装、宿主与 guest 的不同构建目标见[验证操作指南](validation-guide.md#安装与构建产物)。

`tool-build --verify` 的警告与失败处理遵循[测试工程合同](testing.md#ci-与发布证据)。跨机一致记录不会自动使摘要漂移成为硬门。

## Profile 配置

配置文件为单 Agent 的 `agent.toml`。读取位置与加载行为由 [`discovery.rs`](../../crates/lattice-profile/src/discovery.rs)、[`load.rs`](../../crates/lattice-profile/src/load.rs)维护；字段、默认模板分别见 [`schema.rs`](../../crates/lattice-profile/src/schema.rs) 和 [`defaults.rs`](../../crates/lattice-profile/src/defaults.rs)。日常起步示例见[根 README](../../README.md#快速开始)。

| 配置节 | 用途 |
| --- | --- |
| `[tools]` | `enabled` 选择向模型暴露且可进入准入的封闭目录工具 |
| `[lancet]` | LANCET 配置入口 |
| `[lancet.project_root]` | 项目路径策略范围，不是 ProjectStoreKey 的来源 |
| `[lancet.observation]` | 工作区观察限制与预算 |
| `[lancet.network]` | 网络访问策略 |
| `[lancet.workspace]` | 工作区读写策略 |
| `[lancet.policy]` | 外部策略文件位置 |
| `[lancet.capabilities]` | 按能力名称 `deny` 与 `max_boundary` 控制准入 |

启用工具不等于允许其全部 Host 操作。具体准入顺序和未知名称拒绝见 [Cell 准入](../architecture/cell.md#调用准入)，路径可见性与写权限见[工作区路径策略](../architecture/workspace.md#路径策略)。不要使用已退役的 `[workspace]`、`lancet.mode` 或 `[lancet.effects]` 配置。

### 配置与启动环境

后台在每次接受前按 agent 名读取 profile，先查会话绑定项目的 `.lattice/agents/<名字>/agent.toml`，再查后台启动环境中的 `$HOME/.lattice/agents/<名字>/agent.toml`，并加载系统提示、解析和冻结有效策略。CLI/TUI 只提交 agent 选择；agents 目录没有覆盖变量。
agent 名必须是单个普通路径段。profile、外部提示与外部策略只读取普通文件；`agent.toml` 与外部策略原始文件各最多 16 MiB，解析后的系统提示最多 2 MiB，均按字节计量。

后台从用户级 `~/.lattice/providers.toml` 读取 provider 配置，从 `~/.lattice/auth.json` 读取凭据；key 用 `lattice auth set` 录入，用法见[根 README](../../README.md#2-配置模型凭据)。环境变量与工作区 `.env` 不提供 key、endpoint 或模型，项目中的 `providers.toml` 不被读取。`lattice models` 经后台列出模型候选、限制、价格与凭据状态。

后台默认在启动时与此后每 24 小时刷新 `~/.lattice/cache/models.json`。用户级 `providers.toml` 的 `[metadata] auto_refresh = false` 可关闭自动刷新，后台每次触发时读取当前设置，无需重启；配置无效时跳过本次自动刷新并记录原因。手动更新使用 `lattice models refresh`，不受开关影响。Anthropic Messages 的 `max_tokens` 使用合并后模型元数据中的输出上限，缺少时使用 32,768。

后台启动与 Git 隔离规则见[会话合同](../architecture/session.md#配置来源与后台启动)。

## 语义编辑

`mutate__create` 用于目标尚不存在的新文件，`create_parents` 的用法见 [create manifest](../../tools/mutate-create/manifest.toml)。`mutate__edit` 用于已有文件，不删除文件；从 `observe__workspace` 的结果原样复制 `content_hash` 到 `expected_hash`。

[edit manifest](../../tools/mutate-edit/manifest.toml)维护实际参数形状：`replace_text` 使用 `old/new`，`insert_before`、`insert_after` 使用 `anchor/content`，`occurrence` 从 1 开始。同一文件的多个修改放入一次 `edits` 数组，按顺序应用。

匹配与诊断的单一实现是 [`mutate-shared`](../../tools/mutate-shared/src/lib.rs)。缺失或歧义匹配要求重新规划；no-op、trailing newline 状态改变和已有空文件插入限制按[文件变更合同](../architecture/workspace.md#请求与安装)拒绝。不要用手写 hashline、guest 自判 stale 或重放旧请求绕过 Host 的前提核验。Host 的 WriteLimits 在安装前仍有效。

## Audit 与 status

在对应 Local Workspace 根目录运行。查询经 Local IPC 由后台读取全局会话库，按 Session 的项目绑定过滤，无需当前 profile/provider 凭据或 Component 组成。tail 是 `--tail` 选项，不是子命令；replay 同时需要 SessionId 和 TurnId。

```sh
lattice audit --help
lattice audit --json
lattice audit --tail --since 0 --json
SESSION_ID="替换为会话 ID"
TURN_ID="替换为 TurnId"
lattice audit replay --session "$SESSION_ID" --turn "$TURN_ID" --json
lattice lancet status --json
```

`--since` 是排他的全局 ChangeSeq 游标，`0` 从日志起点读取；续读使用响应的 `next_after`。其他项目的变化也会推进扫描位置，所以过滤后空页仍可能推进游标。查询在一个读事务中完成，不再接受 `--at` 或 ledger OID；Registered、Started 及终态都可读。过滤、分页参数见 [`commands/audit.rs`](../../crates/lattice-cli/src/commands/audit.rs)，语义见[操作员接口](../architecture/lancet.md#操作员接口)。DTO 不含原始参数、结果正文或 schema；这些是 Host-only 操作，不向 Agent 注册为 Tool，也不会重新执行工具。

## Doctor

```sh
lattice lancet doctor
lattice --json lancet doctor
```

`lancet doctor` 检查 Local Workspace 规范化与生成封闭目录的组成，任一检查失败以非零状态退出。JSON 是同一检查的结构化输出。它不验证完整 Tool 调用、持久终态、Bundle 安装或实际权限配置。

## 开发状态诊断与重建边界

先通过只读查询确认实际状态。没有 Owner 的遗留 Turn 停在已写入的进度，结局见[执行者消失后的 Turn](../architecture/session.md#执行者消失后的-turn)；后台不重新联系 provider 或分发 Tool，下一次新提交或 `rewind` 直接进行。不要把删除目录重建作为普通恢复手段，旧 #139-B 的清理步骤不授权处理当前状态。

确需开发状态重建时，先由人类明确选择绝对 STATE_ROOT、Local Workspace、ProjectStoreKey 和数据归属，逐项列出受影响对象。全局 `lattice.redb` 保存所有项目的会话，不能为处理单个项目随意删除；项目 `workspace-history.git` 单独核验。发现旧 `sessions.git` 或任意项目 `invocations.git` 时，后台拒绝打开并列出路径，由人决定删除或存档，不迁移、不兼容读取。所需原证据先保全，任何删除、搬移、重新创建都须有对应授权，不用广泛 glob 操作其他项目。路径以[存储布局](../architecture/storage.md#私有状态根)为准，不从旧日志猜测。

重建后通过不带 `--session`、带 `--agent` 的正式 `session send` 入口验证新 Session 和一次无工具调用的 Turn，核对 `list`、`log`、`turn --json` 与操作员输出。旧 Session/Turn 不承诺跨 hard cut 可读；会话 `rewind` 不恢复文件。没有实际运行这些验证时，只能报告完成文件处理，不能宣称恢复闭环。

## 验证与出处

按[测试工程合同](testing.md)选用最小充分验证集；运行配方见[验证操作指南](validation-guide.md)。本指南承接旧 `docs/LANCET.md` 的操作资料及 #31 切片 C 的现行用法，规则不再由旧入口维护。
