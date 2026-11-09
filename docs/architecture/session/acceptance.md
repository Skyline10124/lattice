---
status: active
---

# Turn 的接受与模型步骤

> 所属领域：[会话与 Turn](README.md)

## 请求接受

执行核心持有 SessionExecutionLock 后核对身份、项目绑定与输入；新接受还验证用户消息、工具目录及执行限制。接受事务写入以下事实：

- UserMessage 节点，父节点为 Session 当前位置；新会话先写入根节点 `0`，第一条 UserMessage 为 `1`、父节点为 `0`。
- UserMessage 节点同时记下 `turn_id`、接受时间、agent 名、模型、provider、推理档位、执行限制和配置摘要。系统提示与工具目录按摘要写入配置表，已有同一摘要时不再写。项目绑定取自 Session 记录，不另存。
- TurnId 索引：TurnId 指向这个 UserMessage 节点。
- 预留的第 0 步，父节点为 UserMessage：没有尝试和回复，停在 `Reserved`。
- Session 记录的当前位置指向预留的 Step；新会话的 Session 记录与根节点在同一事务创建。
- profile 副本：新会话、提交指定了 agent，或这一轮的模型与档位和副本中的不同时，写入这一轮所用的副本，规则见[会话的 profile 副本](settings.md#会话的-profile-副本)。

这个事务提交是请求接受的持久边界。用户消息为 User 角色的 text parts，大小规则见[输入与传输限制](concurrency.md#输入与传输限制)。

| 接受时停在哪里 | 已知事实 | 后台责任与用户下一步 |
| --- | --- | --- |
| 输入、绑定、配置等在事务前被拒绝 | 尚未建立 Turn，也未发送 provider 请求 | 返回类型化拒绝；前端保留输入供用户修正。配置类拒绝给出的说明见[配置拒绝的原因](#配置拒绝的原因) |
| 事务 `commit` 返回错误 | 接受结果未知：事务要么完整生效，要么完全未生效 | 后台按[事务失败](concurrency.md#事务与一致性)关闭并重开会话库，不启动 provider；LocalTurn 按原 TurnId 查询找回，见[请求身份与找回](recovery.md#请求身份与找回) |
| 事务已提交 | 请求已接受，原用户消息及冻结事实已保存 | 后台执行并关闭原 Turn；读回这一轮或转交回执失败时回复接受结果未知，LocalTurn 按原 TurnId 查询找回；之后的失败与取消走相应关闭路径，进程退出时 Turn 停在已写入的进度 |

## 执行配置冻结

后台在准入时读取 profile，冻结进这一轮。冻结把“以后再读某个文件”变成“这次 Turn 使用的确定值”，使执行中的一轮不受 profile 文件修改的影响。provider 配置、模型元数据与凭据是后台持有的全局状态，不冻结：请求网关每次请求时按当前值解析，见[模型接入合同](../llm/README.md#组成与归属)。前端只交出选择，不读取 profile、provider 配置、元数据或凭据。

配置的来源：

- **profile。** 会话的 profile 副本；新会话或提交指定了 agent 时，由 `lattice-profile::load_agent_profile` 发现并加载该 agent 的 profile（先查项目内的 `.lattice/agents/<名字>/agent.toml`，再查配置目录下的 agents 目录，核对全局目录与文件权限，仅接受 `schema_version = 1`、`[agent]`、`[prompt]` 与可选 `[llm]`，解析字段与系统提示；出现已移除的 `[lancet]` 或 `[tools]` 时直接拒绝，见 [ADR 0009](../../adr/0009-remove-legacy-lancet-tool-executor.md#6-profile-删除-lancet-与-tools)）。
- **provider、模型、元数据与凭据。** 提交的选择或副本中的 `ModelRef`；接受时由 `GatewayTurn::prepare` 在一次准备中读取当前配置，核对 provider 与模型处于 `ready`（满足声明、凭据与端点校验，见[可用性判定](../llm/models.md#可用性判定)），并按本次档位策略选定推理档位（见[会话的 profile 副本](settings.md#会话的-profile-副本)）。输出上限、推理预算与凭据本身不在接受时冻结，由请求网关每次请求时按当前状态取得，见[元数据的来源与合并](../llm/models.md#元数据的来源与合并)与[凭据](../llm/credentials.md#凭据)。
- **TLS 根证书。** 后台启动时载入，见[配置来源与后台启动](#配置来源与后台启动)。

`FrozenTurnConfig` 是协调层在接受时构造的非秘密执行事实，供执行、结果投影、`log` 显示使用：agent 名、模型、provider、推理档位和执行限制记在 UserMessage 节点上，其余按内容摘要写入配置表。

| 字段类别 | 来源 | 冻结时点 | 接收/持有者 | 是否持久化 | 用途 |
| --- | --- | --- | --- | --- | --- |
| 提交的选择 | 前端：agent、模型、推理档位，都可以不选 | 提交时 | 后台准入 | 不单独保存；生效的值写入副本与 UserMessage 节点 | 用户在本次提交中的选择 |
| 有效 profile | profile 副本，或 agent 的 profile | 接受事务前 | 执行核心 | 是，在 profile 副本中 | 固定 agent、默认模型与档位、LLM 重试与超时 |
| 凭据 | `auth.json` | 不冻结；请求网关每次请求时取得，SIWC 的 access token 按需刷新，见[刷新](../llm/credentials.md#刷新) | 请求网关，只在内存中 | 否 | 认证 provider 请求 |
| TLS 根证书 | 后台启动时载入的内容 | 后台启动时 | provider 连接与本 Turn 的 Tool Host 信任配置 | 否 | 验证 TLS 连接 |
| `FrozenTurnConfig.agent_name/model/provider/effort` | 有效 profile、模型解析与档位选择的结果 | 接受事务前 | TurnCoordinator、UserMessage 节点及投影 | 是 | 标明此次执行身份、模型与推理档位 |
| `FrozenTurnConfig.system_prompt` | 有效 profile 加载的系统提示 | 同上 | 模型请求、配置表 | 是 | 固定本 Turn 的系统指令 |
| `FrozenTurnConfig.catalog` | 本次运行时的启用工具目录 | 同上 | 模型投影、调用登记验证、配置表 | 是 | 保存按工具名排序的 entries 及摘要；entry 含名称、不透明工具身份 `tool_identity`、描述、规范输入 schema（[ADR 0009](../../adr/0009-remove-legacy-lancet-tool-executor.md#4-会话库升版与存储收敛)） |
| `FrozenTurnConfig.limits` | profile 超时、每步尝试数及代码定义的步骤上限 | 同上 | 执行核心、UserMessage 节点 | 秒数与计数持久化，单调时钟 deadline 不持久化 | 限制后续 provider/Tool 启动，并解释尝试预算 |

持久冻结事实的类型由 `lattice-session` 维护，接受接线见[接受接线](../../../crates/lattice-api/src/turn/live.rs)。目录保存中立契约，provider wire 投影由 LLM 层按请求生成。

### 配置来源与后台启动

后台使用启动它的进程的环境，在启动时读一次，此后不随提交变化，连接到同一后台的前端共用这份配置。显式运行 `lattice daemon serve` 与前端自动启动的后台行为相同，自动启动的后台继承拉起它的前端的环境。

| 项 | 来源 |
| --- | --- |
| 配置目录 | 后台环境中的 `$HOME/.lattice`；其中有 `providers.toml`、`auth.json`、`cache/` 与 `agents/` |
| agents 目录 | 项目内的 `.lattice/agents` 与配置目录下的 `agents`，查找顺序见[执行配置冻结](#执行配置冻结)；没有其他覆盖方式 |
| TLS 根证书 | 平台根证书，按 `rustls-native-certs` 的规则从后台环境加载，所以 `SSL_CERT_FILE`、`SSL_CERT_DIR` 有效 |

- **全局 agents 目录的权限。** 配置目录下 `agents` 中的 profile 只在可信时使用：`agents` 目录、`<名字>` 目录与 `agent.toml` 都属于当前用户，不可被同组或其他用户写入（权限与 0o022 无交集，所以默认的 0755 目录与 0644 文件可用），路径的最后一段不是符号链接。外部系统提示文件位于全局 agents 目录内时，按同一规则检查。发现属主不符、同组或其他用户可写或是符号链接时，按配置错误拒绝并说明原因，不自动修改。只检查路径的最后一段，上级目录中的符号链接（例如指向别处的 `$HOME`）不受影响。项目内的 `.lattice/agents` 属于所在工作区，不做这项检查。

- 后台启动时载入 TLS 根证书，保存在内存中直到退出。个别证书文件读不出时只记 WARN，其余照常使用。单张过大或无法按 rustls 信任锚规则解析 DER 的证书跳过并记 WARN，不占名额，也不计入 DER 总量；其余按加载顺序保留前缀，达到 1,024 张或 DER 总量 4 MiB 时停止并记 WARN，之后的根证书都不信任，由它们签发的 HTTPS 服务请求失败。按上述规则筛完一张可用的根证书都不剩（包括全部过大、全部无法解析或根本没有载入到证书）时，后台记为没有可用的根证书。没有可用的根证书时，后台照常启动、照常回答查询；所选 provider 的端点是 https 的，在接受前按配置错误拒绝并说明原因，http 端点照常放行；不回退到平台根。Tool Host 按同一规则处理：没有可用的根证书时，Tool 的 https 请求在通过网络策略与单元授权后、解析与连接前拒绝，证据记为 `tls_roots_unavailable`；不在允许名单或未授权的目标仍记为策略拒绝，http 请求照常；模型元数据刷新也需要 https，同样以“no usable TLS root certificates”拒绝。
- 请求不带配置来源。后台运行期间，前端环境的变化不影响它；要让新的环境生效，用 `lattice daemon stop` 停止后台后重新启动。前端的 `HOME` 或 `XDG_STATE_HOME` 不同时通常解析出另一个状态根，连到的是另一个后台。
- 后台不清空、也不替换启动环境。所以后台创建或打开 Git 库时一律使用隔离方式，不读用户与全局的 Git 配置。
- 工作区 `.env` 不再加载；环境变量不提供 key、endpoint 或模型，见[endpoint 的来源](../llm/providers.md#endpoint-的来源)。

### 配置安全边界

后台按 Turn 冻结 profile，执行中不重读它，不切换共享环境或 cwd。provider 配置与凭据由请求网关每次请求时取得，SIWC 的 access token 在请求前按需刷新，见[模型接入合同](../llm/credentials.md#刷新)。凭据只在 `auth.json` 与后台内存中，不进入会话库、profile 副本、公开 DTO、Debug、错误回显或日志。

provider 与 Tool Host 共享后台启动时载入的 TLS 根内容，各 Turn 独立构造信任配置。Tool 网络继续遵守 [LANCET 逐跳授权与地址固定](../lancet.md#网络获取)。

### 配置拒绝的原因

新执行的配置在接受前被拒绝时，用户需要知道该改哪里。配置由后台读取，报错面向的是配置的主人，所以后台把原始诊断经脱敏后带回前端，包括 profile 的查找、权限校验、已移除或禁止的顶层节（如 `[lancet]`、`[tools]`）、TOML 解析与字段构造（给出文件路径、节名与字段名）、`[agent].name` 与所选不一致、`providers.toml` 与 `auth.json` 的解析、provider 与模型未满足的可用性要求（未声明、未录入 API key、未登录 ChatGPT 或配置无效）、模型引用与带来源归属的推理档位错误（`--effort`、`agent.toml` 的 `[agent].effort` 或会话 profile 副本），以及没有可用的根证书时所选 provider 的端点是 https。`session send`、`run` 与 TUI 内提交对同一问题给出同一句说明，`kind` 为 `bad_request`，CLI 退出 2。

说明由 [`ConfigRejection`](../../../crates/lattice-api/src/config_rejection.rs) 承载。它只有一个构造入口，文字在其中经 [`redact`](../../../crates/lattice-api/src/redact.rs) 处理，被替换处写作 `[REDACTED]`。脱敏按以下顺序执行：

| 顺序 | 替换什么 |
| --- | --- |
| 1 | 已知值：`auth.json` 中的全部凭据值；长度不足 8 个字符的跳过 |
| 2 | URL 中的用户名与密码 |
| 3 | `Bearer`、`Basic`、`Token` 之后的值 |
| 4 | 凭据类查询参数的值，例如 `api_key`、`token`、`password`、`secret`、`sig` |
| 5 | 带已知前缀、长度不少于 20 个字符的密钥，例如 `sk-`、`sk-ant-`、`AIza`、`ghp_`、`glpat-`、`AKIA` |
| 6 | 连续 40 个以上、同时含字母和数字的 `[A-Za-z0-9_-]` 串 |

规则参考 `rtb-redact` 的规则集并按本项目调整。第 6 条的字符集不含 `/`、`+`、`=`，文件路径因此保留；SessionId 与 UUID 不足 40 个字符，纯字母的长标识符不含数字，都不受影响。脱敏按值、位置与形状识别，是尽力而为：既不在 `auth.json` 中、形状又不在上表内的短秘密会原样显示。凭据只保存在 `auth.json`，profile 与 `providers.toml` 都不存放凭据。

TOML 解析错误给出路径、行号与解析器的错误描述，不附源码行。说明是单行文字：控制字符换成空格，空白折叠，最多 1024 个字符，超出时以 `…` 截断。

以下拒绝保持固定文本：提交输入超限或无法解析、持久配置校验，以及后台其余的 `ApiError::BadRequest(String)`。

## 模型请求与执行预算

一次 provider 请求以 `ModelAttemptKey` 标识，包含 TurnId、步骤序号和该步骤的尝试序号。一次尝试对应一个被 provider 接收的请求：HTTP 库对 provider 明确没有处理的请求的重发（复用的空闲连接在请求发出前已被服务端关闭；HTTP/2 `GOAWAY(NO_ERROR)` 或 `REFUSED_STREAM`，RFC 9113 §8.7）计入同一次尝试，不另记尝试，只在日志中留下记录，见 [`lattice-llm`](../../../crates/lattice-llm/AGENTS.md)。协调层每次先在 Step 节点新增这次尝试并提交，此时它是 `Sent`，停在这里的结论为 `Stopped { ProcessExit }`；再让 ModelStepDriver 打开和消费流。暂行写法（见 #331）：尝试前重新解析配置失败时，仍写入一次 `Sent`，provider id 取自模型引用，协议取本 Turn 最近一次成功解析的结果，随即以不可重试的 `ProviderError` 结束且不发送请求。请求结束时，把这次尝试推进到 `Ended`，带上实际终态：

```text
AttemptEnd = Completed { step }
           | Stopped { cause, diagnostic }

cause = ProviderError { retryable }
      | Cancelled
      | Deadline
      | OutputLimit
      | InvalidBatch
      | ProcessExit
```

| 尝试结果 | 保存什么及下一动作 |
| --- | --- |
| `Stopped { ProviderError { retryable: true } }`，该步骤尝试预算未尽 | 同一事务写入终态与等待时长，等待后以下一尝试序号请求。等待时间取冻结的指数退避与 provider 建议等待（`retry-after-ms` 或 `retry-after`）中较长者；`max_delay_ms` 只限制指数退避，不限制建议等待。已收到多少流内容不影响重试资格，流在结束事件之前中断也可重试。事务提交后，视图推送这个 Step 的新值，条目中这次尝试带有重试提示，表示重试已经安排；下一次尝试开始时，条目中出现新的 `Sent` 尝试。等待期间收到取消或 Turn deadline 已到时不再请求，按[关闭 Step 与 Turn](steps.md#关闭-step-与-turn)以相应原因停止 |
| `Stopped`，原因不可重试，或可重试但该步骤尝试预算已尽 | 写入终态，同一事务以 `Stop` 关闭 Step，Turn 随之关闭，`cause` 取自该尝试。这次失败没有重试提示，由 `TurnSummary` 的 notice 说明，同一次失败只有一条面向用户的说明 |
| provider 建议等待超过 60 秒，或不短于 Turn 剩余时间 | 该尝试记为 `Stopped { ProviderError { retryable: false } }`，立即以 `Stop` 关闭，不等到 deadline，也不截短等待后重发；诊断保留 provider 的原因与建议等待，用户看到的是限流而不是超时 |
| `Completed`，回复无工具调用 | 尝试终态、回复与 Step 的 `Finish` 关闭在同一事务写入，见[关闭 Step 与 Turn](steps.md#关闭-step-与-turn) |
| `Completed`，回复含工具调用 | 尝试终态与完整回复、调用槽位在同一事务写入，进入[调用槽位](steps.md#调用槽位)的执行 |
| 完整输出的批次校验失败 | 写入 `Stopped { InvalidBatch }`，整批拒绝、不重试，以 `Stop` 关闭 |
| 进程突然退出，实际结果未保存 | 这次尝试停在 `Sent`，结论为 `Stopped { ProcessExit }`；Step 停在 `Requesting`，结论同为进程退出，此后没有写入，见[执行者消失后的 Turn](recovery.md#执行者消失后的-turn) |

被重试取代的失败尝试留在 Step 节点里，连同为它安排的等待时长。带有等待时长的尝试就是安排过重试的尝试：Step 条目为每个这样的尝试带一条重试提示，Warn 级，文字取该尝试的诊断，并带上等待时长。视图与历史查询给出同一种条目，所以重新打开会话后看到的重试提示与实时一致。导致 Step 停止的那次尝试没有等待时长，不另给提示，由 Turn 结局的 notice 说明。

流式阶段没有副作用，工具只在完整回复后执行，所以重发一段半截回复只花费 token 与时间；半截文字不保存。实时观察者可能先看到一段部分文字，随后看到新尝试从头开始，前端按尝试边界处理。`Deadline`（Turn 墙钟上限）与 `OutputLimit`（单次回复字节上限）沿用现有门禁，只作为原因记录；它们的数值与粒度由 [#222](https://github.com/Skyline10124/lattice/issues/222) 另行裁决。

模型步骤上限为 32，profile 不能放宽。每步尝试数由 profile 冻结，最多 11 次，重试不额外消耗模型步骤。达到步骤上限时保留此前已写入的节点，以步骤级原因 `Stop` 关闭 Turn。

流组装按 UTF-8 字节持续计量：

| 对象 | 协议上限 |
| --- | --- |
| 单 ToolCall 参数 | 1 MiB |
| 助手正文 | 2 MiB |
| reasoning | 2 MiB |
| 一次尝试总量：正文、reasoning、各调用的参数、工具名与 provider call ID，以及各片段的回放状态之和 | 16 MiB |

参数在流式阶段按收到的原文计量；回复结束、参数解析为规范对象后，规范形式比原文长的部分补入总量。同一元数据重复给出相同的值时不重复计入。超限立即停止消费，记录 `OutputLimit`。写入会话库时按同一算法、同一组常量对完整回复独立核对一次，参数按规范形式计量；这是持久边界上的检查，超限的回复不写入。这些是对话协议与组装器的限制，计量类型在 [`lattice-conversation`](../../../crates/lattice-conversation/src/lib.rs)，不能解释为容量或延迟的测量结论。单 ToolCall 参数上限与 LANCET 单文件工作区写入上限 `MAX_WORKSPACE_WRITE_BYTES`（1 MiB）一致：写入内容随参数传入，超过该大小的调用无法成功执行。

Turn 的单调时钟 deadline 在接受事务提交后开始，限制后续 provider 请求与 Tool 分发。槽位结果与 Step 关闭等必要写入不受 Turn deadline 约束；后台重启后不继续执行未关闭的 Turn，也不重建剩余执行时间。每次尝试分别保存按[用量换算](../llm/usage.md#用量换算)得出的用量：输入、缓存读、缓存写、输出与其中的推理，各项都可以缺失，缺失表示未报告；失败与重试尝试的已知用量也保留。每次尝试同时保存请求网关给出的 `usage_kind` 与可选 `cost`（`CostEstimate { usd, exact }`，未知为 null），Turn 的 Complete 或 Partial 由保存的尝试判定与汇总溢出派生，见[关闭 Step 与 Turn](steps.md#关闭-step-与-turn)。费用结果的含义见[费用估算](../llm/usage.md#费用估算)。
