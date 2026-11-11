---
status: active
---

# 会话设置

> 所属领域：[会话与 Turn](README.md)

会话设置（`SessionSettings`）保存在会话库中，是会话下一轮将使用的配置的唯一权威。每个会话在 `configure(New)` 创建时写入初始设置，之后通过 `configure(Existing)` 更新。

## 会话设置的内容

| 字段 | 类型与约束 | 说明 |
| --- | --- | --- |
| `profile` | `EffectiveProfile` | agent 快照，含 agent 名（`AgentName`）、可选描述、默认模型（`Option<ModelRef>`）、默认推理档位（`Option<Effort>`）、Turn 超时（`TurnTimeout`）与重试策略（`RetryPolicy`） |
| `system_prompt` | `String`（上限 2 MiB） | 有效系统提示正文，随会话设置内联保存 |
| `model` | `Option<ModelRef>` | 当前选择的 provider `id` 与模型 `id`，可以为空 |
| `effort` | `Option<Effort>` | 当前选择的推理档位，可以为空；为空时模型请求省略档位参数 |

会话设置不包含 provider 配置、模型元数据与凭据；这些数据由请求网关在查询能力或发送请求时从全局状态读取，见[模型接入合同](../llm/README.md#组成与归属)，价格的生效时机见[费用估算](../llm/usage.md#费用估算)。

## 设置与一轮的配置

- **表示下一轮的配置。** 接受一轮时，后台从当前会话设置与本轮工具目录冻结这一轮的执行配置，见[执行配置冻结](acceptance.md#执行配置冻结)；这一轮实际使用的 agent、provider、模型与推理档位记在它的 UserMessage 节点上。
- **与历史轮次独立。** 最后一轮之后通过 `configure` 修改过设置时，会话设置与最后一轮 UserMessage 节点上记录的配置不同。
- **当前 agent。** 会话的当前 agent 就是会话设置中的 `profile.name`。
- **`rewind` 保持设置不变。** `rewind` 只移动会话的当前节点指针，会话设置原封不动。
- **磁盘 profile 的作用边界。** 修改磁盘上的 `agent.toml` 或提示文件只影响此后显式给出 `agent` 的 `configure` 调用；未给出 `agent` 时，会话继续使用已保存在会话设置中的 `profile` 与 `system_prompt`。

## 设置版本

设置版本（`SettingsVersion`）是对完整 `SessionSettings` 规范 JSON 计算的带域前缀 SHA-256 摘要（前缀 `lattice-session-settings-v1\0` 与 8 字节大端长度后接规范 JSON 字节），编码为 64 位小写十六进制字符串：

- 内容相同则版本相同；把设置改回原值会恢复原来的 `SettingsVersion`。
- Session 记录的 `created_at` 与 `updated_at` 时间戳不参与计算。
- 提交请求可携带 `expected_settings` 核对当前设置版本，见[请求身份与找回](recovery.md#请求身份与找回)。

## configure 的计算基准与保存

`configure` 接收目标会话（`New(S)` 或 `Existing(S)`）、规范工作区 `workspace`（IPC 中为字节数组），以及可选的 `agent`、`model`、`effort`，返回 `ConfigureReply { head, adjustment }`（其中 `adjustment` 为 `Option<EffortAdjustment { from, to }>`）；公开操作总览见[实现模块与公开操作](operations.md#实现模块与公开操作)。

计算基准按请求参数确定：

- **给出 `agent` 时**：按 `workspace` 查找并加载该 agent 的 profile（优先 `<workspace>/.lattice/agents/<agent>/agent.toml`，未找到时查全局 `agents/<agent>/agent.toml`），以新加载的 `EffectiveProfile`、`system_prompt`、`profile.model` 与 `profile.effort` 为基准。
- **未给出 `agent` 时**（仅适用于 `Existing(S)`）：以当前会话设置为基准，沿用其中的 `profile`、`system_prompt`、`model` 与 `effort`。
- **目标模型**：请求显式给出的 `model` 覆盖基准模型；未给出 `model` 时保留基准模型。

保存按目标会话分支执行：

- **`New(S)`**：必须给出 `agent`（缺失返回 BadRequest）。`S` 已存在时返回 Conflict，原因为 `session_exists`。会话库尚未存在时按需创建，见[首次创建](../storage.md#首次创建)。校验通过后在单个写事务中写入三项记录：
  1. Session 记录：`project_path`、`current = 0`、`created_at = updated_at = now`；
  2. 根节点 `(S, 0)`；
  3. 会话设置。
- **`Existing(S)`**：会话不存在时返回 NotFound；先核对 Session 记录的 `project_path` 与请求 `workspace` 一致，不一致时返回 Conflict，原因为 `binding_mismatch`。核对与解析通过后，在单个写事务中写入新会话设置，并将 Session 记录的 `updated_at` 刷新为当前时间；每次成功的 `configure(Existing)` 都会刷新 `updated_at`，即使设置内容没有变化。

## 档位规则

推理档位的解析与回退只在 `configure` 的 `resolve_settings` 中执行，共五个分支：

| 分支 | 条件 | 结果 | 回复中的 `adjustment` |
| --- | --- | --- | --- |
| 1 | 目标模型为空，且给出了 `effort` | 按配置错误拒绝，提示先选择模型 | — |
| 2 | 目标模型为空，且没有给出 `effort` | 保留基准档位；因此 `model` 为空时，`effort` 也可能不为空 | `None` |
| 3 | 目标模型非空，且给出了 `effort` | 严格校验：目标模型支持则采用；不支持则拒绝，诊断来源为 `(from --effort)` | `None` |
| 4 | 目标模型非空、没有给出 `effort`，且目标模型与基准模型相同 | 严格使用基准档位：为空则保持为空，模型支持则保留；不支持则拒绝，诊断来源在给出 `agent` 时为 `(from [agent].effort in <路径>)`，未给出 `agent` 时为 `(from session settings)` | `None` |
| 5 | 目标模型非空、没有给出 `effort`，且目标模型与基准模型不同 | 依次尝试基准档位与 agent 快照中的默认档位（`profile.effort`），取目标模型支持的第一个；都不支持时结果档位为空 | 结果与基准档位不同时为 `Some({ from, to })`，相同时为 `None` |

模型的档位能力取自 `lattice_llm::model_supported_efforts`：读取 `providers.toml`、模型元数据与 provider `features`，不读取凭据；配置或元数据错误如实返回，不当作空候选。推理档位与协议请求参数的对应关系见[推理档位](../llm/reasoning.md#推理档位)。

## 校验与失败

- **在 `configure` 中完成**：`workspace` 规范路径核验，profile 的查找、权限核验、TOML 与提示文件解析及字段构造，目标模型的档位匹配，以及会话设置自身的完整性校验。
- **在 `submit` 中检查**：目标模型非空，provider 的配置就绪、凭据可用性与 HTTPS 端点的 TLS 根证书，以及对会话设置中模型与档位的复核。因此会话设置可以先行选用凭据尚未就绪的 provider 或保存空模型。

两阶段的配置拒绝分类与路径脱敏规则见[配置拒绝的原因](acceptance.md#配置拒绝的原因)。

## 并发、找回与生命周期

- **并发与观察**：`configure` 与 `rewind` 相同，占用会话的 `Owner` 槽并取得 `SessionExecutionLock`；会话正在准入或执行一轮时返回 Busy。`configure` 成功后，该会话的 `watch` 观察者收到更新后的会话头。锁与忙碌原因、[草稿会话](../../reference/glossary.md)及逾期清理规则见[操作锁](concurrency.md#操作锁)与[会话状态与草稿清理](concurrency.md#会话状态与草稿清理)。
- **幂等与找回**：参数相同且所依赖的配置文件没有变化时，`configure` 得到相同的会话设置；后台不维护请求去重表。结果不明时的重发与找回规则见[configure 的找回](recovery.md#configure-的找回)。
