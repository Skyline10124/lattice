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

### 配置与凭据诊断

工作区 `.env` 不能覆盖已有进程环境，也不能设置 `LATTICE_*`、provider endpoint 后缀 `*_BASE_URL` / `*_API_BASE` / `*_API_URL` / `*_ENDPOINT`、`*_PROXY`、`HOME` / `XDG_*`、`SSL_CERT_FILE` / `SSL_CERT_DIR`。匹配不区分大小写。受信 shell/CI 进程环境与检出仓库的 `.env` 不是同一信任来源；API key 的允许加载不授权仓库改写策略、端点、代理或证书信任。

使用顶层 `lattice doctor` 检查 credential/endpoint 来源、applied/shadowed/dropped 键与解析错误，不通过启动告警回显敌意输入。输出不能泄露 credential 值，代理 URL 的 userinfo 须脱敏。读取行为与诊断实现见 [`env_boundary.rs`](../../crates/lattice-cli/src/env_boundary.rs)和 [`commands/doctor.rs`](../../crates/lattice-cli/src/commands/doctor.rs)。旧记录中的改用 HOME `.env` 或新增 `--env-file` 方案并非现行授权。

## 语义编辑

`mutate.create` 用于目标尚不存在的新文件，`create_parents` 的用法见 [create manifest](../../tools/mutate-create/manifest.toml)。`mutate.edit` 用于已有文件，不删除文件；从 `observe.workspace` 的结果原样复制 `content_hash` 到 `expected_hash`。

[edit manifest](../../tools/mutate-edit/manifest.toml)维护实际参数形状：`replace_text` 使用 `old/new`，`insert_before`、`insert_after` 使用 `anchor/content`，`occurrence` 从 1 开始。同一文件的多个修改放入一次 `edits` 数组，按顺序应用。

匹配与诊断的单一实现是 [`mutate-shared`](../../tools/mutate-shared/src/lib.rs)。缺失或歧义匹配要求重新规划；no-op、trailing newline 状态改变和已有空文件插入限制按[文件变更合同](../architecture/workspace.md#请求与安装)拒绝。不要用手写 hashline、guest 自判 stale 或重放旧请求绕过 Host 的前提核验。Host 的 WriteLimits 在安装前仍有效。

## Audit 与 status

在对应 Local Workspace 根目录运行。当前 CLI 的 tail 是 `--tail` 选项，不是 `audit tail` 子命令；replay 同时需要 SessionId 和 TurnId。

```sh
lattice audit --help
lattice audit --json
lattice audit --tail --since <ledger_oid> --json
lattice audit replay --session <SessionId> --turn <TurnId> --json
lattice lancet status --json
```

查询和 replay 可用 `--at <ledger_oid>` 固定视图。`--since` 是排他的 ledger OID 游标，不能使用旧数字 audit ID。过滤选项与参数解析见 [`commands/audit.rs`](../../crates/lattice-cli/src/commands/audit.rs)；固定 tip、顺序及生命周期语义见 [InvocationStore 操作员接口](../architecture/lancet.md#invocationstore-与操作员接口)。这些是 Host-only 操作，不向 Agent 注册为 Tool，也不会重新执行工具。

## Doctor

```sh
lattice lancet doctor
lattice --json lancet doctor
```

`lancet doctor` 检查 Local Workspace 规范化与生成封闭目录的组成，任一检查失败以非零状态退出。JSON 是同一检查的结构化输出。它不验证完整 Tool 调用、持久终态、Bundle 安装或实际权限配置，也不同于顶层 `lattice doctor` 的配置诊断。

## 开发状态诊断与重建边界

持久化失败先按 [Session 重开](../architecture/session.md#重开取消与未完成状态)关闭旧 Turn，不重新联系 provider 或分发 Tool，不把已有目录删除重建作为普通恢复手段。旧 #139-B 的 no-import hard cut 与清理步骤是历史操作，不授权清理当前状态。

确需开发状态重建时，先由人类明确选择绝对 STATE_ROOT、Local Workspace、ProjectStoreKey 和数据归属，逐项列出将受影响的会话与项目库。全局 `sessions.git` 与项目的 `invocations.git` / `workspace-history.git` 分别核验；默认保留全局会话，不以广泛 glob 删除其他项目数据。所需原证据先保全，任何删除、搬移、重新创建都须有对应授权。路径规则见[存储布局](../architecture/storage.md#私有状态根)，不从旧日志路径猜测当前目标。

重建后使用正式创建入口验证新 Session 与零 Tool Turn；旧 Session/Turn 与 WorkspaceHistory 不承诺跨 hard cut 可读或可 Rewind。没有运行上述验证时，只能报告完成文件处理，不能宣称恢复闭环。

## 验证与出处

按[测试工程合同](testing.md)选用最小充分验证集；运行配方见[验证操作指南](validation-guide.md)。本指南承接旧 `docs/LANCET.md` 的操作资料及 #31 切片 C 的现行用法，规则不再由旧入口维护。
