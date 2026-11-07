---
status: active
---

# 凭据与 Sign in with ChatGPT

> 所属领域：[模型接入](README.md)

## 凭据

### `auth.json`

凭据保存在配置目录下的 `auth.json`（默认 `~/.lattice/auth.json`），只由后台读写：

```json
{
  "version": 1,
  "providers": {
    "deepseek": { "type": "api_key", "key": "<key>" },
    "chatgpt": {
      "type": "chatgpt",
      "host_id": "<host id>",
      "client_id": "<issued client id>",
      "account": { "subject": "<sub>", "email": "<email>" },
      "tokens": {
        "access_token": "<token>", "refresh_token": "<token>", "id_token": "<token>",
        "expires_at": "<RFC 3339>", "scopes": ["…"]
      }
    }
  }
}
```

- **权限。** 文件与所在目录属于当前用户。配置目录（默认 `~/.lattice`）新建时为 0700，`auth.json` 为 0600；二者都不能向同组或其他用户开放。属主不符、权限更宽或是符号链接时，后台拒绝使用并说明原因，不自动修改。目录不满足时，`lattice auth set` 与需要 key 的提交被拒绝。
- **写入。** 后台进程内串行写入：先在同目录写临时文件（0600）并 fsync，再改名替换，最后 fsync 目录。
- **大小。** 文件最多 1 MiB；单个 key 1～16,384 字节，不含控制字符。
- **严格解析。** 不认识的 `version` 或结构错误时整个文件无效，所有需要凭据的操作按配置错误拒绝；`lattice auth status` 将依赖凭据的 provider 标为 `invalid` 并报告文件错误。
- **不外泄。** 凭据不进入会话库、profile 副本、日志、诊断、公开 DTO 或 Debug 输出。配置拒绝的脱敏以 `auth.json` 中的全部凭据值为已知值，见[会话合同的配置拒绝](../session/acceptance.md#配置拒绝的原因)。

### 凭据命令

`lattice auth` 是前端命令，只负责界面，经私有 IPC 交给后台执行：

| 命令 | 后台的动作 |
| --- | --- |
| `lattice auth set <provider>` | 从终端（不回显）或标准输入读取 key，校验后写入；key 不作为命令行参数 |
| `lattice auth remove <provider>` | 删除该 provider 的 key；对 `chatgpt` 先按[登出](#登出)处理，再删除 `client_id` 与账号信息，保留 host ID |
| `lattice auth login chatgpt` | SIWC 登录，见下节 |
| `lattice auth logout chatgpt` | SIWC 登出 |
| `lattice auth status` | 每个已注册 provider 的[可用性状态](models.md#可用性判定)：`ready`、`unconfigured`（附所缺要求 `declaration`、`api_key` 或 `chatgpt_login` 及操作提示）或 `invalid`（`auth.json` 无效及原因）；`chatgpt` 另给账号邮箱与 access token 的到期时间。不返回凭据本身 |

只有 `auth` 为 `bearer`、`x-api-key` 或 `x-goog-api-key` 的 provider 能 `set`。

ChatGPT 的 logout 与 remove 都返回远端撤销状态：`confirmed`（撤销已确认）、`unconfirmed`（本地已清理，远端撤销未确认）或 `not_needed`（没有 token，无需撤销）。撤销只尝试一次，未确认时提示用户可到 ChatGPT 设置断开。remove 另给 `removed`：是否存在被移除的注册、账号或 token；仅有 host ID 时为 false。API key 删除仍只报告 `removed`。本地清理失败按错误返回，不能用撤销状态代替成功确认。

登录的授权 URL 只交给专用交互界面，打开浏览器失败或无头时写入 stderr；粘贴输入不回显，Ctrl-C 或输入结束取消登录。`--json` 只输出最终的非敏感结果，登录取消退出 2；授权 URL、回调 URL 和 token 不进入日志或最终 JSON。连接交换和停止顺序由[会话合同](../session/operations.md#凭据命令与私有交换)维护。

原顶层命令 `lattice doctor` 只诊断环境变量与工作区 `.env` 中的凭据与 endpoint 来源，随这两个来源一起删除；按别名解析 endpoint 的 `lattice resolve` 随别名表一起删除；provider 可用性状态由 `lattice auth status` 给出，模型候选与可用性由 `lattice models` 给出。

## Sign in with ChatGPT

SIWC 按 OpenAI 的[开源工具接入方式](https://developers.openai.com/siwc/token-sharing-open-source)实现，只用于内置 provider `chatgpt`。

### 固定的 provider 与 endpoint

`chatgpt` 的协议、endpoint 与认证方式固定为表中的值：OpenAI 的接入文档只给出 `https://api.openai.com/v1`，token 的 audience 也是它。`providers.toml` 不能改写它们，见[用户配置](providers.md#用户配置-providerstoml)。

### 登录

登录在后台执行，前端只负责界面。整个登录是一次 IPC 交换，前端断开或取消即中止：

1. **准备。** 本机的 host ID 在首次登录前生成（随机 UUID）并保存，此后不变。后台为这次登录生成 `state`、OIDC `nonce` 与 PKCE verifier（S256），在 `127.0.0.1:1455` 启动回调监听，回调地址为 `http://127.0.0.1:1455/auth/callback`。端口被占用时登录失败并说明原因。
2. **授权 URL。** 后台把 `https://auth.openai.com/api/accounts/authorize` 的授权 URL 交给前端：`response_type=code`、`resource=https://api.openai.com/v1`、scope `openid profile email offline_access resource.invoke chatgpt.tokens.use.direct`、`ext_agent_host_id`、`state`、`nonce` 与 `code_challenge`。首次登录时 `client_id=dynamic_agent_client` 并带 `agent_name_hint=LATTICE`；已有注册时用保存的 `client_id`，带 `id_token_hint`（登出后没有保留则不带），不带 `agent_name_hint`。
3. **用户授权。** 前端打开浏览器或显示 URL。无头机器上，用户把浏览器最终跳转的 URL 粘贴给前端，前端转交后台，后台按回调同样处理。
4. **校验回调。** 先校验 `state`；回调带 `error` 时停止。首次登录的回调带回正式的 `client_id`，没有带回时登录不成立；再次登录的回调可以不带 `client_id`，带回与保存的不同时拒绝。
5. **换取 token。** 用正式的 `client_id`、`code`、`code_verifier` 与同一个 `redirect_uri`、`resource` 向 `https://auth.openai.com/api/accounts/oauth/token` 换取 token。首次登录的回调已经带回正式 `client_id`，而换码返回 `invalid_grant` 时，丢弃 `code`，将该 `client_id` 与 host ID 保存为不含 token 的记录，本次登录失败；下次登录复用该 `client_id`。再次登录换码失败不覆盖已有凭据。
6. **校验 id token。** 按 OpenAI 的 JWKS 校验签名，核对 issuer、audience（正式的 `client_id`）、有效期与 `nonce`；授予的 scope 必须含 `chatgpt.tokens.use.direct`。再次登录时，新 id token 的账号必须与保存的一致。
7. **保存。** 写入 `auth.json` 的 `chatgpt` 项，然后拉取[账号模型列表](#账号模型列表)，把登录状态与目录结果返回前端。目录失败不撤销登录：成功结果带 `catalog.status = refreshed` 与 `model_count`，失败带 `catalog.status = failed` 与安全的 `warning`；登录仍成功退出，文本界面另显示警告。

### 刷新

- 有效期取 token 响应的 `expires_in`，换算成 `expires_at` 保存，不写死。
- 凭据模块在每次使用 access token 之前检查：剩余有效期短于 300 秒（包括已经过期）时，先刷新并写回，再返回可用的 token。推理请求与账号模型列表请求都经同一个取 token 入口，调用方不接触到期时间，也没有单独的刷新入口。准入、`auth status` 与档位查询不用 token，不触发刷新；接受 Turn 时只确认已经登录。不设定时刷新。
- 刷新用保存的 `client_id` 与 refresh token 请求 token 端点。成功后 access token、到期时间、scope 与新的 refresh token 一起写回 `auth.json`。
- 刷新不校验或替换响应中的 `id_token`，保留登录时验证并保存的那份，供再次登录的 `id_token_hint` 与账号身份使用。
- 同一时刻需要刷新的多个请求只发起一次刷新，其余等它的结果。
- 刷新失败按原因区分：
  - token 端点返回 `invalid_grant`、`invalid_refresh_token`、`token_expired`、`refresh_token_expired`、`refresh_token_invalidated` 或 `refresh_token_reused`：refresh token 已失效。清除 token，保留 `client_id` 与 host ID；这次尝试按不可重试的认证错误停止，提示重新登录。
  - `invalid_client`：注册配置有误，按不可重试的错误停止，保留材料，提示重新登录（`lattice auth remove chatgpt` 后再登录会重新注册）。
  - 网络故障、超时与 5xx：凭据保留；这次尝试按可重试的 provider 错误停止，由协调层按现有重试预算重试，诊断写明是刷新失败。

### 登出

撤销 refresh token 后删除 access、refresh 与 id token，保留 `client_id`、账号信息、host ID 与账号目录缓存，供再次登录复用。撤销返回空的 HTTP 200 即成功；撤销未确认时仍删除这些 token，并告诉用户远端撤销未确认。remove 另删除注册、账号和目录缓存，保留 host ID；同一后台会拒绝清理后的旧目录请求发布缓存。

### 账号模型列表

- 经统一的 `access_token()` 入口取得可用 token，请求固定 `GET https://api.openai.com/v1/models`。响应读取 `models[]`，只保留 `visibility` 为 `list` 的项，以 `slug` 作为模型名、`display_name` 作为显示名。连接复用网关的 TLS 根、地址固定、禁代理和禁重定向约束，不增加目录重试循环。
- 在登录保存成功、后台元数据自动刷新、`lattice models refresh` 时拉取；普通候选查询包含 chatgpt 且没有合法目录时也补拉。未登录不联网。登录即使已有目录也刷新；有合法旧目录时查询只读本地，合法空列表表示当前没有可列出的模型。
- 缓存位于配置目录下的 `cache/chatgpt-models.json`，包含 `version = 1`、UTC RFC3339 的 `fetched_at` 与 `models` 数组，各模型仅含 `slug`、`display_name`。缓存不绑定账号；换账号须先 remove，logout 保留。发布沿用 models.dev 的同目录临时文件、文件同步和改名替换。
- 列出的 slug 使用模型 ID 的共同校验，显示名非空且不含控制字符；非法或重复 ID、坏结构和未知缓存版本使本次目录无效。拉取失败保留旧缓存；不存在或损坏的缓存按缺目录处理。合法空列表仍缓存并记录成功时间。
- 推理档位、限制与价格按同名模型取 `openai` 条目的元数据，再按字段应用 `chatgpt` 手写覆盖。账号目录只限定普通候选集合，不成为请求准入白名单。

### 请求与错误

- 请求 `POST https://api.openai.com/v1/responses`，`store: false`、`stream: true`；`response.completed` 正常完成；`response.incomplete` 的 `max_output_tokens`、`content_filter` 按[适配器职责](protocols.md#适配器的职责)保存回复并映射 finish_reason，其他原因、`response.failed` 与流中断按错误停止。
- HTTP 429 且错误码为 `subscription_sharing_usage_limit_exceeded`，以及流中 `response.failed` 的同一码，均按不可重试的 provider 错误处理，优先于 `x-should-retry` 等重试提示。client/transport 统一分类并提示到 ChatGPT 设置查看用量，保留实际 HTTP 状态码、错误码与 request ID；协议适配器只翻译事件。其他 `subscription_sharing_*` 码沿用现有错误规则。
