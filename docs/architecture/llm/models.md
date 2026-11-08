---
status: active
---

# 模型引用与元数据

> 所属领域：[模型接入](README.md)

## 模型引用

- profile 的 `[agent].model`、CLI 的 `--model` 与 TUI 的选择都写作 `<provider>/<model>`。在第一个 `/` 处拆分；provider 部分按 `id` 规则校验，模型部分非空、最长 4,096 字节、不含控制字符。
- 没有别名，也没有默认模型。profile 没有写模型、会话也没有选择时，提交在接受前按配置错误拒绝。
- provider 不存在时按配置错误拒绝，说明列出可用的 provider。模型不在元数据中时照常使用，只是没有价格与推理档位，费用为未知。

## 元数据的来源与合并

| 来源 | 内容 | 更新 |
| --- | --- | --- |
| 内置快照 | xtask 从 models.dev 生成，只含内置表 `catalog` 所列的条目 | 随版本发布，变化在 PR 中审查 |
| 缓存 `cache/models.json` | 后台从 `https://models.dev/api.json` 拉取，保留内置表与 `providers.toml` 中 `catalog` 所列的条目，字段规则与快照相同 | 文件新于 24 小时则跳过自动拉取。缺失或更旧时在启动时拉取，成功后再隔 24 小时。失败从 1 分钟起倍增重试，上限 1 小时。`metadata.auto_refresh = false` 时跳过该次触发；`providers.toml` 无效时该次触发算作失败，按上述退避重试。每次触发都读当前配置。`lattice models refresh` 仍可立即刷新 |
| 账号目录 `cache/chatgpt-models.json` | 当前 ChatGPT 账号允许列出的模型标识与显示名；限制、档位和价格仍取同名 openai 元数据 | 触发和生命周期见[账号模型列表](credentials.md#账号模型列表)；自动刷新按目录自己的成功时间计算 24 小时 |
| `providers.toml` 的 `models` | 用户手写 | 用户编辑 |

- **选择来源。** 缓存存在且能解析时整份使用缓存，否则使用内置快照；缓存中缺少某个 `catalog` 条目时，这个条目使用快照。`providers.toml` 中的 `catalog` 在缓存与快照中都没有时，要等下一次刷新才有元数据；在此之前，以及 models.dev 中没有这个条目时，这个 provider 只有手写的元数据。手写的元数据按字段覆盖上述结果，只写在 `providers.toml` 中的模型由手写值单独构成。
- **保留的字段。** 模型 `id`、`name`、`limit.context`、`limit.output`、`reasoning`、`reasoning_options`、`tool_call`、`modalities.input`、`cost.input`、`cost.output`、`cost.cache_read`、`cost.cache_write` 与 `status`。其余字段丢弃。
- **没有过期的概念。** 缓存旧了只影响列表与价格的准确性，不影响请求发往哪里，不提示过期。
- **刷新失败**只记日志，继续使用上一份数据，并从 1 分钟起倍增重试，上限 1 小时。`lattice models refresh` 把失败原因返回给用户。拉取使用 HTTPS 与固定地址的连接，响应最多 32 MiB，30 秒内没有读完即放弃。新缓存先写临时文件再改名替换。
- **按来源调度。** models.dev 与已经登录的账号目录由同一个后台任务调度，各自记录新鲜度、失败次数和下一次时间。同一来源的并发触发共享结果；一个来源成功不清除另一个的失败。显式刷新分别尝试两者，成功来源照常发布，部分失败返回来源、原因及已经更新的部分。`auto_refresh` 只影响自动触发，登录、手动刷新和缺目录补拉继续有效。
- **请求路径上不联网。** 接受 Turn 与每次请求时只读上述数据。

## 可用性判定

请求网关（`availability`）统一判定 provider 与模型的可用性，准入与请求准备（`GatewayTurn::prepare`）、模型候选查询及 `lattice auth status` 共用同一规则，只按认证方式与配置事实判定：

| 状态 | 附加字段 | 含义与提示 |
| --- | --- | --- |
| `ready` | — | 配置与凭据齐备，可用于模型请求 |
| `unconfigured` | `requirement` | 尚未完成配置：`declaration` 表示 `auth = "none"` 的免凭据 provider 未在 `providers.toml` 中声明（提示写出实际 `providers.toml` 路径与 `[providers.<id>]`）；`api_key` 表示 key 类认证未在 `auth.json` 中录入凭据（提示 `lattice auth set <id>`）；`chatgpt_login` 表示 `auth = "oauth:chatgpt"` 尚未登录（提示 `lattice auth login chatgpt`） |
| `invalid` | `reason` | `auth.json` 解析或权限失败，或模型元数据与手写档位配置无效，附具体原因 |

内置表中免凭据的 provider 默认未声明，`providers.toml` 中出现对应的 `[providers.<id>]` 表（含仅写 `models` 等子表的隐式父表）即视为已声明。模型层判定先取所属 provider 的状态：provider 非 `ready` 时模型直接沿用该状态；provider 为 `ready` 时再校验该模型的推理档位配置，通过为 `ready`，出错为 `invalid`。

## 模型候选

模型候选查询 `ModelCandidateQuery` 提供两种互斥形状，供 CLI 的 `lattice models` 与 TUI 的 `/model`、`/effort` 使用：

- **候选集合与列表过滤。** 候选由已注册 provider（内置表与 `providers.toml` 合并结果）与已知模型组合得出，每项携带上下文窗口、输出上限、可用的推理档位、价格与 `availability`。普通列表额外执行两项列表过滤：隐藏 `status` 为 `deprecated` 的模型；`chatgpt` 只取[账号模型列表](credentials.md#账号模型列表)中的模型。列表过滤只控制普通列表展示，与可用性判定独立；被隐藏或不在目录中的模型仍可按完整 `<provider>/<model>` 直接请求或用 `Target` 查询。指定未注册的 provider 时按配置错误拒绝并列出可用 provider。
- **`List { provider: Option<ProviderId>, ready_only: bool }`。** 按可选 `provider` 限定范围，先过滤 deprecated 模型再计算 `availability`。`ready_only = false`（`lattice models`）返回范围内全部候选及其 `availability`；手写档位无法表达的模型自身标为 `invalid`（附原因），可用档位为 `[]`，其他模型照常列出。`ready_only = true`（`lattice models --auth` 与 TUI `/model`）在生成候选前跳过 provider 层非 `ready` 的 provider，生成后再去掉模型层非 `ready` 的候选；警告（缺 `catalog` 元数据、ChatGPT 账号目录不可用）只对保留在结果范围内的 provider 生成，被跳过的 provider 不产生模型或警告。
- **`Target(ModelRef)`。** 查询指定的 `<provider>/<model>`，保留 deprecated、账号目录外和无元数据的模型（无元数据时限制与价格为空、档位为 `[]`），返回单项候选及其 `availability` 与本地可推导的 `supported_efforts`。`chatgpt` 的 `Target` 查询只读本地元数据与缓存目录显示名，不补拉账号目录。
- **`chatgpt` 目录补拉。** `List` 查询范围包含已登录的 `chatgpt`（`ready_only = true` 下未被跳过）且本地没有合法目录时，先在异步层补拉，再在阻塞阶段合并。缺目录或补拉失败给实际警告，其他 provider 照常列出；未登录时不请求账号目录、不给警告。合法空目录和旧的有效目录均可直接使用。候选显示名取账号目录，限制、reasoning、档位和价格按同名 `openai` 元数据与 `chatgpt` 手写字段合并；无同名元数据或手写值时字段为空、档位为 `[]`。手写模型和 `openai` 列表不增加账号候选。
- **排序与目录提示。** 候选先按内置 provider 表的顺序列出，再按新增 provider `id` 排序；同一 provider 内按模型 `id` 排序。位于结果范围内的 provider 写了 `catalog` 而元数据中没有对应条目时，候选查询为它给出警告。
- **能力推导与单次准备。** 可用档位按模型元数据与协议 features 推导，不要求凭据就绪，也不使用或刷新 token；`chatgpt` 按同名 `openai` 元数据与手写覆盖推导。准入与每次发送前统一调用 `GatewayTurn::prepare`，单次读取注册表、凭据与元数据，完成 provider 查找、模型可用性核对、HTTPS 根证书检查与档位选择。
