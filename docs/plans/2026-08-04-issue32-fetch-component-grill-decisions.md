# Issue #32 `fetch.web` Component 迁移：grilling 决议快照

- Issue：<https://github.com/Skyline10124/lattice/issues/32>
- 状态：设计 grilling 已完成一轮；本文记录已确认约束与下一轮未决项，**不是实现计划**
- 工作树：`/home/astrin/Dev/lattice-wt-issue32`
- 分支：`feat/issue-32-fetch-component`
- 起点：`origin/main` @ `33c90f95`
- 记录日期：2026-08-04
- 最近更新：2026-08-05（D16）

Issue #32 的正文与后续评论仍是验收范围的权威来源。本文只持久化本轮
grilling 新增或细化的决议，供下一次会话直接继续，不复制完整票面。

## 1. 已确认决议

### D1. Component runtime 切到 async；只让网络 import 异步

- 保持公开 `ComponentRuntime::call` 为 async API。
- 内部改用 Wasmtime async instantiate / call 路径。
- 现有 `host-control`、`workspace-observe`、`workspace-mutate` Host imports
  继续同步执行，先记录为 sync 路径，不为本票强行异步化。
- 新增的网络 Host import 是 async；DNS、连接、TLS、HTTP body 读取都在该
  future 内完成。
- Wasmtime 已启用 `async` feature，目标是选择性 async binding，而不是把所有
  Host 方法统一改成 async。

### D2. 新增封闭的 `NetworkFetchHost`，不提供通用 HTTP/WASI 权限

- 新增第三个具体 domain Host authority：`NetworkFetchHost`。当前两个是
  `WorkspaceObservationHost` 与 `WorkspaceMutationHost`；此外每次调用仍携带
  `AdmittedTool` 作为 Cell admission authority。
- guest 只获得一个面向 `fetch.web` 的窄接口：GET-only，每次 Tool call 最多
  调用一次网络 import。
- Host 内部拥有完整 redirect chain；redirect 不算 guest 的第二次网络操作。
- guest 不能选择 socket peer、已解析 IP、TLS 行为、redirect 策略，也不能开
  第二条连接。
- 禁止 WASI sockets、DNS、filesystem、process、环境变量秘密及任意未声明
  Host function。

这里的“新增”指：在当前闭集 `DomainImport`、widest Tool Host WIT、生成的
expected imports 与 runtime `StoreState` 中加入一个新的受控 domain import；
不是在 Component 中加入通用网络栈。

### D3. Host 跟随 redirect，每一跳重新完整授权

- 与旧 native path“把 3xx 原样返回给 Agent”不同，新 Host 返回 redirect
  chain 的最终响应。
- 每一跳在发起下一次连接前都重新执行：URL/scheme 校验、allowed-host policy、
  DNS 解析与 request-time pin、private/reserved 地址检查、rebinding 检查。
- 任一跳拒绝后立即停止，不连接该目标，也不把失败跳交给 guest 自行处理。
- 整条 chain 对 guest 是一次 Host operation；审计则按 hop 展开。
- redirect 上限固定为 5，不新增 profile knob。计数语义在 WIT/测试中固定为：
  初始请求的 `redirect_count = 0`，每次接受一个 `Location` 并准备下一请求时
  加一，因此最多 6 次 HTTP request。

一个 redirect hop 的粒度是一条具体 HTTP target：一个规范化 URL、它的一组
DNS candidates、最终实际连接 peer，以及该 target 的 policy/HTTP 结果。一次
DNS 返回多个地址仍属于同一 hop，不拆成多跳。

### D4. 跨 origin redirect 丢弃全部调用者 headers；凭证留给未来 browser Tool

- 初始请求拒绝连接控制或容易破坏 Host 不变量的 headers，包括 `Host`、
  `Connection`、`Content-Length`、`Transfer-Encoding`、`Upgrade`、
  `Proxy-Authorization` 等。
- 同 origin redirect 可以保留其余调用者 headers。
- origin（scheme、host、port）发生变化时，丢弃**全部**调用者提供的 headers，
  不只丢弃 `Authorization` / `Cookie`；任意 `X-Api-Key` 也可能是凭证。
- Host 为每一跳重新构造请求，不提供 Cookie jar、认证挑战登录或凭证委派。
- 需要登录态、跨站身份流或站点交互时，应由未来的浏览器控制 Tool 管理
  credential 与 origin 绑定，不扩宽 `fetch.web`。
- Host 强制 `Accept-Encoding: identity`，调用者不能覆盖。
- request/response header values 永不进入 evidence/audit。

### D5. 整条 chain 共用一次 deadline 与 cancellation

- DNS、所有连接、redirect 和 body 读取共用现有 Component call deadline；默认
  30 秒，不按 hop 重置。
- async cancellation 直接丢弃正在进行的 DNS/HTTP future，不等待该 hop 自己
  超时。
- cancellation 映射为 `CallCancelled`，deadline 映射为 `ToolTimeout`，超过
  redirect 上限映射为结构化 redirect-limit 错误。

### D6. Host 只返回最小响应投影，Component 负责公开结果 shaping

Host import 的成功结果只向 guest 暴露：

- status
- content type（可选）
- 受上限约束的 body bytes
- truncated
- final origin
- redirect count

Host 可以为安全与资源控制解析 `Location`、限制 header count/总字节数，但不把
任意 response headers 暴露给 guest。Component 按兼容规则把 body 转成当前
公开结果，并构造精确 JSON result。

Cloudflare/反自动化响应不伪装成空成功：

- `403`/`503` + HTML challenge body：保留 status 与受限 body，过滤
  `Set-Cookie` 等敏感 headers。
- `403`/`429` + 空 body：仍返回非成功 status。
- 需要执行 JS、保存 cookie 或通过交互挑战的情况不属于 `fetch.web`，留给未来
  browser Tool。

至少覆盖三条回归：带 `Set-Cookie` 的 403 challenge HTML、空 body 的 429、
带敏感 headers 的 200 正常 body。

### D7. 网络 evidence 保存脱敏 URL；取消 digest，并要求精确 invocation 关联

> 2026-08-27 review amendment: maintainer authorization after the #32 implementation review
> supersedes the earlier decision to persist query tokens in local evidence.

- 本地 evidence 对初始请求及每个 redirect hop 保存规范化 scheme、host、port 与
  path；userinfo、query、fragment 均不持久化。Host 只在内存中的 transport URL
  保留 query，因此签名 URL、API key 与 session token 不进入 durable ledger。
- 不保存 URL digest。脱敏 URL 仍能独立证明访问的 endpoint/path；digest 既不能
  恢复目标，也不能替代证据脱敏。不要保存 header values、Cookie、body 或 Host
  管理的凭证。
- 每个 hop 的 network evidence 至少包含：脱敏 target URL、DNS candidates、typed
  transport state（实际 peer / TLS / failure phase）、`http_status`（如有）、从
  transport 有界读取的原始 body bytes、实际交付 guest 的整形 body bytes，以及 policy verdict。
  `origin` 可由 URL 唯一派生，不重复持久化。
- 已连接/观察到的 hop 附着在现有 `ObservedEffect` 路径；在连接前被政策或安全
  检查阻止的 hop 附着在现有 `DeniedOperation` 路径。网络细节作为共享的可选
  `NetworkEvidence` 扩展现有 versioned evidence records，不新建平行 evidence
  根，也不回塞 `metadata_json`。
- transcript 已保存原始 Tool arguments，但起点代码把 provider Tool Call ID 同时
  当作 transcript correlation，并在进入 CEP 前丢弃；CEP 另行生成 process-local
  numeric `call_id`，evidence 又生成 store-local `evidence_id`。不能靠时间/URL
  猜测这些记录的关系。D16 将它们收敛为一个跨 Agent/session/LANCET/evidence
  的领域身份 `InvocationId`；provider 返回的调用 ID 只保留为 LLM wire metadata。

### D8. `fetch.web` 使用一个 external network Cell

Descriptor scope：

```toml
[[scopes]]
name = "external-host"
trust = "ex"
channel = "network"
```

唯一 capability：

```toml
[[capabilities]]
name = "network.fetch"
cell = "alloc:read:external-host"
```

- Host 网络 operation 与 Tool result 使用同一个 Cell，模式与
  `observe.workspace` 一致；不添加独立 result capability，也不保留 legacy
  `FetchUrl` alias。
- 两者的 concrete target 不同：operation 是 `ObservedTarget::Host`，result
  仍是进入会话上下文的 `ObservedTarget::Session`。
- `allowed_hosts` 约束具体目标，不是 capability grant。
- 默认 `max_boundary = in` 必须拒绝该 Tool；显式允许 `ex` 才可能 admission。

### D9. 网络授权采用 `ex` boundary + 非空 allowed-host 双门

- 调用需同时满足 `max_boundary = ex` 与非空 `allowed_hosts`。
- 空 `allowed_hosts` 永远拒绝网络，不再表示“所有公网 HTTPS”。
- `allow_public_https` 是公网 HTTPS master switch，不是 host grant。
- localhost 需要 `allow_localhost = true`，同时还要在 `allowed_hosts` 显式列出。
- 每个 redirect target 都重新匹配 `allowed_hosts`。
- Host security checks 不受 legacy `CepMode::Warn` / `Off` 软化。
- 这会消除当前两条旧逻辑不一致：URL validator 在 allowed-host 为空时可能允许
  公网 HTTPS，而 Enforce effect region 又会拒绝空 host policy。

### D10. 公开 result 显式报告完整性，并 additive 增加最终 origin 与 redirect 数

公开 result 保留 `status`、`body`、`content_type?`，并固定：

- `truncated`：required boolean；Component 每次成功都在实际 `value_json` 中
  输出 `true` 或 `false`，不能用字段缺失暗示 `false`。

- `final_origin?`：仅 scheme + 规范化 host + 非默认 port；不含 path/query。
- `redirect_count?`：实际跟随的 redirect 数，初始直达响应为 0。

`final_origin` / `redirect_count` 使用 additive optional 字段。Agent 因此可以确认
正文是否完整、最终站点与 redirect 数；本地 evidence 只保存脱敏 hop URLs。

### D11. `network-fetch` 成功面是一个 typed `fetch` interface

`network-fetch` 是一个深 module：guest 只学一个函数与两个 records，redirect、
DNS、pinning、policy、deadline 和 limits 全部隐藏在 Host implementation 内。

```wit
interface network-fetch {
    record header {
        name: string,
        value: string,
    }

    record fetch-request {
        url: string,
        headers: list<header>,
    }

    record fetch-response {
        status: u16,
        content-type: option<string>,
        body: list<u8>,
        truncated: bool,
        final-origin: string,
        redirect-count: u32,
    }

    fetch: func(
        request: fetch-request,
    ) -> result<fetch-response, fetch-error>;
}
```

- 没有 method、redirect、timeout、DNS、peer、cap 或 credential 参数，因此 guest
  不能把 GET-only fetch 扩成通用 HTTP client，也不能改变 Host policy。
- `body` 是受 Host cap 约束的 bytes；Component 负责公开 string shaping。
- `final-origin` 是 Host 生成的规范化 origin string，不是完整 URL；完整 initial /
  redirect URLs 只在本地 network evidence。
- WIT response 本身不直接进入 LLM context。Component 把它整形成公开 JSON，
  写入 `ToolOutput.value_json`；`CepExecutorAdapter` 序列化后形成实际的
  `GraphMessage { role: "tool", content: "{...}" }`。
- 公开 JSON 每次成功必须显式带 `"truncated": true|false`。WIT、manifest result
  schema（required）与实际进入模型 context 的 `value_json` 三层一致。
- Selective async 只标记
  `"lattice:tool-host/network-fetch.fetch": async`；exports 走 async wrapper，engine
  启用 `async_support(true)`，runtime 改用 `instantiate_async` / `call_run_async`。
  现有 control/workspace Host implementations 保持同步。

`fetch-error` 的 variants 与信息披露单独在下一项决定；D11 只锁定成功面。

### D12. WIT error 只给无 payload 粗标签；结构化错误由 Host 持有

```wit
variant fetch-error {
    invalid-request,
    denied,
    unavailable,
    limit-exceeded,
    timeout,
    cancelled,
}
```

- `invalid-request`：URL/header 格式不合法。
- `denied`：allowlist、scheme、private/reserved address、rebinding 或 Cell
  authorization 拒绝。
- `unavailable`：DNS、连接、TLS、HTTP protocol 或 body stream 失败。
- `limit-exceeded`：redirect/header/URL 等 Host 上限；body cap 不是错误，仍以
  `truncated=true` 成功返回。
- `timeout`：整条 redirect chain 超过共同 deadline。
- `cancelled`：call-scoped cancellation。

这些 WIT variants 不携带 `string`。`NetworkFetchHost` 保留内部细粒度错误，记录
evidence，并像 workspace hosts 一样按 call 保存 structured Host refusal；
`runtime::host_refusal` 优先返回该 `CepError`，guest 只把粗标签转换成逻辑失败，
不能改写分类或把 URL/token/DNS/TLS 细节带回模型。模型仍收到 Host 生成的安全、
可行动消息；完整 target 与政策事实只进入本地 evidence。

### D13. URL 与 request/response headers 使用固定 Host 上限

除 profile 已有的 body cap 外，网络元数据使用不可由 Tool 参数或 profile 放宽的
runtime constants：

| 项目 | 上限 |
|---|---:|
| 原始及每 hop 规范化 URL | 8 KiB |
| caller request headers | 32 个 |
| 单个 request header name | 128 bytes |
| 单个 request header value | 8 KiB |
| request headers 合计 | 16 KiB |
| response headers | 64 个 |
| response headers 合计 | 64 KiB |

- 所有长度按 bytes 计算。request 合计包含 caller fields 与 Host 自动添加的
  fields；response 重复字段分别计数，包括不会暴露的 `Set-Cookie`。
- 初始 URL 在解析前检查 raw bytes；解析后的规范化 URL及每个 redirect
  `Location` resolve 后的 URL 都重新检查 8 KiB 上限。
- 每个 redirect hop 分别检查 request/response header count 与 aggregate bytes。
- body 继续受 `[lancet.network].max_web_response_bytes` 控制；超过该 cap 是成功
  `truncated=true`，不是 `limit-exceeded`。
- URL/caller request header 超限属于 `invalid-request`；response header 或
  redirect metadata 超限属于 `limit-exceeded`。
- HTTP library 的内部默认上限只是更底层防线，不是 LATTICE interface contract；
  Host 必须显式执行并测试上述常量。

### D14. Network failures 复用现有 CEP code 闭集

不为 #32 增加新的 `CepErrorCode`。Host 内部细粒度错误按下表映射；WIT 标签只是
guest 回程信号，不是 CEP 分类 authority：

| 情况 | WIT | CEP code | Retriability |
|---|---|---|---|
| manifest 参数校验失败 | 未进入 WIT | `SchemaMismatch` | `Permanent` |
| URL/header 语义或输入上限错误 | `invalid-request` | `TypeError` | `Permanent` |
| capability / `max_boundary` 未准入 | 未进入 WIT | `CapabilityNotGranted` | `Permanent` |
| allowlist/scheme/private/rebinding/redirect escape | `denied` | `AmbientAuthorityDenied` | `Permanent` |
| Cell operation authorization 意外失败 | `denied` | `EffectViolation` | `Permanent` |
| DNS/connect/TLS/HTTP protocol/body stream 失败 | `unavailable` | `ExecutionFailed` | `Transient` |
| redirect/response-header metadata 超限 | `limit-exceeded` | `ToolResourceExhausted` | `Permanent` |
| 整条 chain deadline | `timeout` | `ToolTimeout` | `Transient` |
| call cancellation | `cancelled` | `CallCancelled` | `Permanent` |

- 模型只收到 Host 生成的稳定、安全消息；validation 只报告字段位置，network
  denial 不带完整 URL，unavailable 不带 DNS/IP/certificate/library error。
- `ToolResourceExhausted.resource` 使用稳定非秘密标签，例如 `redirects`、
  `network_headers`。
- HTTP non-2xx 仍是成功 result；body cap 仍是成功 `truncated=true`。
- Component trap、fuel/memory/count limits 继续走现有 `ToolTrap` /
  `ToolResourceExhausted`，不伪装成 network failure。
- `CallCancelled/Permanent` 表示不能自动重试，不禁止用户显式重新发起。

### D15. `NetworkEvidence` 每 hop 记录 typed endpoint 与 transport phase

Protocol 使用标准库的 typed IP value；`IpAddr` 是无 I/O 的纯数据类型，Serde
JSON / Schemars 表示为 operator-friendly 的规范化 IP string，SQLite 以 TEXT
存储并在读取时重新 parse：

```rust
struct NetworkEndpoint {
    ip: std::net::IpAddr,
    port: u16,
}

enum NetworkResolution {
    NotAttempted,
    Literal,
    Resolved,
    Failed,
}

enum NetworkPolicyVerdict {
    Allowed,
    Denied,
}

enum TlsVersion {
    Tls12,
    Tls13,
    Other,
}

enum TlsState {
    NotApplicable,
    NotAttempted,
    Failed,
    Established { version: TlsVersion },
}

struct TransportEvidence {
    connected_peer: Option<NetworkEndpoint>,
    tls: TlsState,
}

enum NetworkFailurePhase {
    Dns,
    Connect,
    Tls,
    HttpHeaders,
    Body,
}

struct NetworkEvidence {
    hop: u32,
    url: String,
    resolution: NetworkResolution,
    dns_candidates: Vec<NetworkEndpoint>,
    transport: TransportEvidence,
    http_status: Option<u16>,
    body_bytes_received: Option<u64>,
    body_bytes_returned: Option<u64>,
    policy_verdict: NetworkPolicyVerdict,
    failure_phase: Option<NetworkFailurePhase>,
}
```

- 不保存重复 `origin`；operator projection 从脱敏 `url` 派生。Agent-facing
  `final_origin` 保留，模型不读取 Host-only network evidence。
- Evidence 使用 `http_status`；WIT `fetch-response.status` 保持简名，因为那个
  interface 已明确是 HTTP。
- `connected_peer=None` + `failure_phase=Connect` 是 TCP connect failure；peer
  存在、`tls=Failed` + `failure_phase=Tls` 是握手失败；TLS established 但无
  status + `HttpHeaders` 是 HTTP response-head failure；有 status + `Body` 是
  body stream failure。不能再靠空字段猜测阶段。
- HTTP localhost 使用 `TlsState::NotApplicable`；HTTPS 成功必须记录实际协商的
  TLS 1.2 / 1.3（或 forward-compatible `Other`）。不记录 certificate、fingerprint、
  cipher、SNI 或原始 TLS error。
- 当前 reqwest 的成功 response 能给 `remote_addr()`，但 `TlsInfo` 只给 peer
  certificate，无法充分表达协商版本与失败阶段。Host transport implementation
  必须在 connector/rustls 层直接记录事实；不能从 URL 或错误字符串推断。
- DNS candidates 的 evidence copy 排序、去重；连接器可保留 resolver 的实际尝试
  顺序，实际成功地址由 `connected_peer` 唯一证明。

`ObservedEffect` 与 `DeniedOperation` 都 additive 增加
`network: Option<NetworkEvidence>`，不创建新的 protocol evidence root，也不 bump
两种 record version（仍为 1）。SQLite `EVIDENCE_SCHEMA_VERSION` 5→6：scalar、
transport、TLS 字段展开为 effects/denials 的 nullable/queryable columns；候选地址
使用规范化子表：

```text
evidence_network_candidates(evidence_id, seq, candidate_index, ip, port)
```

子表只是 `dns_candidates` 的 1:N 存储展开，不是第四种 evidence root。旧行全部
network NULL；v5→v6 走 additive DDL、shape assertion 和读写/迁移/attestation
兼容测试，不改写历史记录，不使用 JSON blob。

这里的 `5→6` 只描述本文起点 `33c90f95`。如果 D16 的 invocation identity
前置迁移先落地，它拥有下一 schema version；#32 的 network evidence 再从当时的
current version 增加一版，不能为了保住数字 6 把两个独立迁移挤在一起。

### D16. `InvocationId` 是唯一跨层领域身份；provider ID 只留在 LLM wire boundary

前置实现票：GitHub #139「以 InvocationId 统一 Tool 调用、执行与 evidence 身份」。
该票显式 blocks #32；#32 正文不改，通过 issue comment 记录依赖关系。

当前三种 call/evidence ID 不继续作为三个公开领域身份：

- provider Tool Call ID 不是所有协议/模型都以相同方式提供。OpenAI/Anthropic
  当前返回 opaque ID；Gemini 新协议也返回 ID 并要求 response 原样引用，而起点
  Gemini adapter 仍合成 `tc_{name}_{counter}` 且 response 不带 ID。
- CEP numeric `call_id` 是每个 `CepTransport::pair` 从 1 重启的 live execution
  counter，不能作为跨 restart 的 durable identity。
- store-generated monotonic `EvidenceId` 是当前 persistence identity，但会让同一次
  Tool invocation 在 transcript、execution、evidence 中继续拥有三种公开身份。

目标只保留一个跨 Agent、session、events/cancellation、LANCET Host、history 与
evidence 的领域身份 `InvocationId`。它由 LATTICE 生成、全局唯一，并直接作为
call-level evidence identity；SQLite 可保留不出现在领域/API 中的私有 rowid。

Provider ID 不成为第二种领域身份，也不进入 LANCET/evidence：

```rust
struct GraphToolCall {
    invocation_id: InvocationId,
    provider_call_id: Option<String>, // opaque lattice-llm wire metadata
    name: String,
    arguments: String,
}
```

- Agent-driven Tool result 只引用 `InvocationId`；provider request serializer 从对应
  assistant Tool Call 找回原始 `provider_call_id`，在协议要求时原样回传。
- Provider ID 为空或缺失是否合法由具体 provider protocol 决定；要求 ID 的协议
  必须 fail closed，不能偷偷用 `InvocationId` 冒充 provider 输出。
- 没有 provider 层的 direct CEP caller 只使用 `InvocationId`。
- Provider wire ID 不参与授权、routing、cancellation、history 或 operator evidence
  query；operator 和 transcript 通过共同的 `InvocationId` 精确关联。
- 该决议明确取代当前 store-generated monotonic `EvidenceId` 作为公开稳定身份的
  既有设计；旧 evidence/transcript 的兼容读取与 migration 必须由前置重构显式
  处理，不能在 #32 网络实现中临时猜测或双写两套 identity。

## 2. 已核对的当前代码事实

这些是 2026-08-04、起点 `33c90f95` 的导航锚点；实现前仍需重新读取当前源：

- `crates/lancet-runtime/wit/tool-host.wit` 当前只有 `host-control`、
  `workspace-observe`、`workspace-mutate`，没有 network interface。
- `xtask/src/tool_codegen.rs` 的 `DomainImport` 是闭集；新增 network import 必须
  同时进入 generated WIT、expected imports、host composition 和 drift gates。
- composition gate 精确匹配实际 imports 与 pinned expected imports，并拒绝
  `wasi:*`。runtime linker 绑定 widest WIT 本身不构成 Tool 权限；每个 artifact
  的 exact import gate 才决定它能调用什么。
- `ComponentRuntime::call` 的 API 已 async，但内部仍使用 sync Wasmtime
  instantiate / `call_run`。
- 旧 native `fetch.web` 位于 `lancet-catalog` method、`lancet-native` handler 与
  web executor；reqwest redirect 当前关闭，deadline 30 秒，Host 侧做 DNS pin、
  response cap 和 lossy UTF-8 shaping。
- `NetworkPolicyConfig` 在 `lancet-authority`；URL/DNS 中立安全原语在
  `net-guard`。
- `authorize_operation` 当前不接受 `ObservedTarget::Host`，#32 必须补 network
  Host target 的 concrete authorization，不得用 guest 自证。
- `ObservedEffect` / `DeniedOperation` 已有 versioned persistence 根，但没有
  network-specific evidence 字段。
- session graph 的 `turn/messages.jsonl` 保存 `GraphToolCall.arguments`；起点代码
  仍使用 provider/synthetic string ID、独立 CEP numeric id 与独立 EvidenceId。
  D16 的目标是让 transcript 与 evidence 共同使用 `InvocationId`，provider ID
  只在 `lattice-llm` wire projection 中出现。

## 3. 目标调用流与所有权

```text
Agent tool call
  -> Component admission (network.fetch, boundary ex)
  -> fetch.web Component validates/shapes typed params
  -> exactly one async network-fetch Host import
       -> header validation
       -> allowed-host + scheme policy
       -> DNS resolve + public/private/rebinding checks
       -> request-time peer pin + TLS/HTTP request
       -> per-redirect full revalidation (max 5)
       -> response/header/body/deadline limits
       -> per-hop observed/denied NetworkEvidence
  -> Component shapes public Tool result + result Cell
  -> runtime validates result/evidence and finalizes call
```

所有权边界：

- Source Tool manifest：params/result schema、description/version、scope、capability、
  declared network import 的唯一 authority。
- Component guest：typed parameter decoding、窄 Host request 构造、兼容的 body/result
  shaping；不负责安全授权。
- `lancet-runtime::NetworkFetchHost`：transport、redirect、policy application、DNS
  pin、安全检查、limits、network operation evidence。
- `net-guard`：保持只含中立 URL/DNS 安全原语的 leaf，不引入 HTTP client 或
  LATTICE policy。
- `lancet-authority`：继续拥有 network policy 与 Cell/concrete-target 授权语义。
- `lancet-protocol::evidence` + `lancet-audit` versioned tables：唯一结构化 evidence
  路径。

Component embed 与 native/catalog 删除必须在同一可运行切片完成，避免同名
Component/native collision 令 server fail closed。完成后删除旧 handler、executor、
catalog entry 与任何 gateway/fallback；不能保留两条 production path。

## 4. 下一轮必须解决的设计细节

以下尚未 grilling 到足以编码，不能默认为已决定：

1. D16 的 invocation identity 重构已建立为阻塞 #32 的 GitHub #139；其 ID
   encoding/generation point 与 legacy migration 由该票收敛并交付，#32 只消费结果。
2. redirect 中“DNS 已发生但连接未发起”的记录究竟落一条 denial，还是同时需要
   独立 observed DNS effect；必须避免重复 effect accounting。
3. binary/non-UTF-8 body 的精确兼容规则，以及服务端无视
   `Accept-Encoding: identity` 时如何处理压缩响应。
4. same-origin caller headers 的最终允许/拒绝清单和规范化规则。
5. `lancet-runtime` 新增 `net-guard` / HTTP client dependencies 后，根
   `AGENTS.md` dependency graph 与 crate-local ownership 文档怎样同步。
6. Issue #134 评论提到的三项低价值残留是否顺带关闭：runtime AGENTS host-control
    段落换行、mutate-edit HashLine 描述、`169/169` 测试计数口径。它们不是 #32
    核心交付，不能挤占网络迁移验收。

## 5. 下一次会话的建议起点

1. 重新读取 Issue #32 最新正文/评论与本文件，确认没有新的人类决议覆盖它。
2. 继续使用 `grilling`，从 §4 第 1 项开始，一次只收敛一个问题；每题先给推荐项。
3. 决议完 WIT、errors、evidence schema、correlation 后，再拆实现 plan/切片。
4. 实现前读取相关 crate-local `AGENTS.md`，重新核对 branch/base/status。

建议技能：`grilling`（继续未决设计）、`codebase-design`（收敛 WIT/Host 深模块
接口）、`to-issues`（只有在需要把跨层 correlation 拆成独立票时使用）、
`implement`（设计与切片定稿后再使用）。
