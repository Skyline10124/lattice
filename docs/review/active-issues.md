# LATTICE 活跃问题追踪

**最后更新**: 2026-05-05
**状态**: 46 项已解决问题移入 [resolved-issues.md](resolved-issues.md)；37 项活跃（13 VULN + 10 DS + 14 审计 — 29 已解决、6 部分、10 开放）

---

## 严重度定义

| 等级 | 含义 | 响应要求 |
|------|------|---------|
| P0 | 对外可用性阻断 / CRITICAL 安全漏洞 | 立即修复 |
| P1 | 运行时正确性/安全缺陷 | 本迭代修复 |
| P2 | 设计/安全/维护问题 | 计划中 |
| P3 | 代码质量改进 | 延后 |

> 安全漏洞严重度为 CRITICAL = P0, HIGH = P1, MEDIUM = P2, LOW = P3

---

## 统计

| 等级 | 数量 |
|------|------|
| P0 (CRITICAL) | 0 |
| P1 (HIGH) | 0 |
| P2 (MEDIUM) | 0 |
| P3 (LOW) | 1 |
| **总计** | **1** |

---

## P0 — CRITICAL 安全漏洞（2 项）

### P0-05: bash 命令执行无超时控制 [VULN-010]

| 字段 | 值 |
|------|-----|
| **ID** | P0-05 / VULN-010 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/sandbox.rs + tools.rs |
| **文件** | `sandbox.rs:20` / `tools.rs:135` |
| **状态** | Resolved — tokio::time::timeout wraps bash execution using max_command_timeout (default 30s) |

**描述**: max_command_timeout: u32 = 30 已定义但 tools.rs 中的 bash 执行完全未使用。permissive 模式下 sleep 99999 即永久阻塞 agent 线程。

**修复**: `tools.rs` 已用 `tokio::time::timeout(Duration::from_secs(max_command_timeout), tokio::process::Command::new("sh").args(["-c", cmd]).output())` 包裹 bash 执行。超时后返回 `ToolError::CommandError("timed out")`。`sh -c` 仍用于命令解析，但超时确保不会无限阻塞。

---

### P0-06: Agent 配置文件无完整性校验 [VULN-028]

| 字段 | 值 |
|------|-----|
| **ID** | P0-06 / VULN-028 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-harness/src/profile.rs |
| **文件** | `profile.rs:114-118` |
| **状态** | Resolved (2026-05-05) — SHA-256 sidecar enforced by default; auto-generated on first load; watcher monitors sidecar changes |

**描述**: agent.toml 配置文件无数字签名或哈希校验。攻击者可添加 default=true 的 handoff 规则重定向到恶意 agent。watcher 自动 hot-reload 无完整性检查。

**修复**: (1) `strict` 模式默认开启（`LATTICE_STRICT_CONFIG=0` 显式关闭）。(2) strict 模式下首次加载自动生成 `.toml.sha256` sidecar，后续加载校验不匹配则拒绝。(3) watcher 同时监控 `agent.toml` 和 `agent.toml.sha256`，sidecar 删除/篡改触发 reload 并因校验失败而拒绝加载。

---

## P1 — HIGH 安全漏洞（3 项）

### P1-02: 文件操作 TOCTOU 竞态条件 [VULN-013]

| 字段 | 值 |
|------|-----|
| **ID** | P1-02 / VULN-013 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/tools.rs |
| **文件** | `tools.rs:49-63, 90-99` |
| **状态** | Resolved (2026-05-05) — all tool handlers canonicalize before sandbox check; write_file/patch reject when parent doesn't exist; list_directory canonicalizes before read_dir |

**描述**: check_read/check_write 和实际 fs 操作之间无原子性。并发攻击可替换 symlink 指向后再执行操作。

**根因**: 检查-使用时间差。无 O_NOFOLLOW 或 canonicalize 后二次检查。

**修复**: (1) read_file、list_directory、grep 均在操作前 canonicalize 路径并在 canonical 路径上做 sandbox check。(2) write_file canonicalize 父目录后拼接文件名，父目录不存在时拒绝写入而非 fallback 到原始路径。(3) patch 同样 canonicalize 父目录，非存在路径拒绝。(4) sandbox 的 resolve_path 保留 fallback（组件匹配在非规范路径上仍安全），但 tools.rs 层已全面 canonicalize-first。

---

### P1-04: sandbox write allowlist substring matching [VULN-002]

| 字段 | 值 |
|------|-----|
| **ID** | P1-04 / VULN-002 |
| **首次报告** | 2026-05-01 |
| **来源** | [VULN-002](../issues/VULN-002-sandbox-write-allowlist-substring-bypass.md) |
| **组件** | lattice-agent/src/sandbox.rs |
| **文件** | `sandbox.rs:97` |
| **状态** | Resolved (2026-05-05) — component-boundary matching replaces byte-prefix starts_with; is_path_prefix prevents src-hack/ matching src/

**描述**: check_write() 使用 path.contains(prefix) 子串匹配。evil-lattice-core/ 匹配 lattice-core/ 前缀。结合 symlink 可写入任意位置。

**根因**: 子串匹配而非路径规范化 + 目录前缀匹配。

**修复**: (1) `matches_allowlist` 改用 `is_path_prefix` 组件边界匹配——比较路径组件序列而非字节前缀。`/home/user/src-hack/` 不再匹配 `/home/user/src`。(2) 支持相对路径和 cwd 解析的完整匹配链。(3) 新增 `test_allowlist_component_boundary_not_byte_prefix` 测试覆盖。

---

### P1-09: sandbox sensitive-file 过滤非规范 [VULN-003]

| 字段 | 值 |
|------|-----|
| **ID** | P1-09 / VULN-003 |
| **首次报告** | 2026-05-01 |
| **来源** | [VULN-003](../issues/VULN-003-sandbox-sensitive-file-filter-noncanonical.md) |
| **组件** | lattice-agent/src/sandbox.rs |
| **状态** | Resolved (2026-05-05) — wildcard patterns added; sensitive list expanded to 35+ entries; component matching with glob support

**描述**: 敏感文件过滤使用 path.contains(".env") 子串匹配，不解析路径。可能误挡合法文件，也可能被编码或 symlink 绕过。

**根因**: 路径未 canonicalize，匹配逻辑使用子串而非路径组件匹配。

**修复**: (1) `path_matches_sensitive` 已使用路径组件匹配（非子串），支持 `*` 通配符。(2) 新增 `.env.*`、`*.pem`、`*.key`、`*.p12`、`*.pfx`、`id_*`、`.ssh/id_*` 等通配符模式。(3) 敏感文件列表从 16 项扩展至 35+ 项，覆盖 `.env.development/.staging/.test`、`.aws/config`、`.gcp/service-account.json`、`.azure/credentials`、`.ssh/id_ecdsa`、`.ssh/config`、`.gnupg`、`.pypirc`、`.gem/credentials`、`.netrc` 等。(4) 新增 `component_matches_pattern` 函数支持 `*` glob 语法。

---

## P2 — MEDIUM 安全/设计问题（7 项）

### P2-01: 敏感文件列表严重不完整 [VULN-015]
**组件**: lattice-agent/src/sandbox.rs | **文件**: `sandbox.rs:32-73` | **状态**: Resolved (2026-05-05) — expanded to 35+ entries with wildcard patterns (.env.*, *.pem, *.key, id_*, .ssh/id_*)

**描述**: 仅 6 项。缺少 .git/config、~/.ssh/id_*、~/.aws/credentials、.npmrc 等。

### P2-02: DNS rebinding 绕过 HTTPS SSRF [VULN-016]
**组件**: lattice-core/src/router.rs | **状态**: Resolved (2026-05-05) — DNS resolution + private IP check in validate_base_url

**描述**: DNS rebinding 服务（nip.io, xip.io）可解析域名到内网 IP，绕过 host 检查。

**修复**: (1) `validate_base_url()` 对 HTTPS URL 做同步 DNS 解析，检查 resolved IP 是否为私有/保留地址。(2) 新增 `is_private_or_reserved()` 覆盖 IPv4（10/172.16/192.168/127/0/169.254）和 IPv6（fc00/fe80/::ffff mapped）。(3) 保守策略：198.18/15 和 100.64/10 不拒绝（CDN/VPN 环境可能合法使用）。(4) DNS 解析失败不拒绝，仅拒绝已知私有 IP。

### P2-07: HTTP header 注入 via header: 前缀 [VULN-022]
**组件**: lattice-core/src/lib.rs + transport/gemini.rs | **状态**: Resolved (2026-05-05) — denylist→whitelist; gemini.rs header: path now also validated

**描述**: provider_specific 的 header: 机制可注入任意 HTTP 头，无白名单。

**修复**: (1) `SENSITIVE_HEADER_NAMES` denylist 改为 `ALLOWED_INJECTED_HEADERS` whitelist——仅允许 x-request-id、x-correlation-id、anthropic-version、openai-beta 等已知安全 header。(2) gemini.rs 的 `header:` 前缀路径原本未经过 `validate_injected_header`，现已添加校验。(3) 更新测试覆盖：rejects sensitive、rejects unknown、accepts whitelisted 三组。

### P2-08: 凭证通过错误响应体泄露 [VULN-023]
**组件**: lattice-core/src/errors.rs + lib.rs | **状态**: Resolved (2026-05-05) — scrub_credentials() removes API key patterns before truncation; Retry-Already consumed via header-first path

**描述**: 非 2xx 响应体存入错误消息，可能含 API key 回显。

**修复**: (1) 新增 `scrub_credentials()` 函数，在 `truncate_body()` 之前执行正则替换。(2) 覆盖 sk-ant- (Anthropic)、sk- (OpenAI)、Bearer token、JSON credential field 四类模式，替换为 `[REDACTED]`。(3) Anthropic key pattern 优先匹配（更长前缀），避免被通用 sk- pattern 截断。(4) 新增 5 个测试覆盖各类 credential scrubbing 场景。

### P2-10: LATTICE_AGENTS_DIR 环境变量毒化 [VULN-033]
**组件**: lattice-harness + lattice-cli | **状态**: Resolved (2026-05-05) — centralized safe_agents_dir() with canonicalize + system-path rejection; all 4 command files consolidated

**描述**: 环境变量可覆盖 agent 目录到攻击者控制的路径。

**修复**: (1) 新增 `safe_agents_dir()` 和 `validate_agents_dir()` 到 `commands/mod.rs`，所有命令（validate/list/new/run/pipeline）统一调用。(2) canonicalize 后拒绝 /etc /usr /var /sys /proc /dev /boot /root /tmp 等系统关键路径。(3) 清除 4 个文件的重复 `agents_dir()`/`default_agents_dir()`/`agents_dir_path()` 函数和未使用的 `PathBuf` import。

### P2-11: Python binding lock poisoning [VULN-005]
**组件**: lattice-python | **状态**: Resolved (2026-05-05) — run_async returns PyResult instead of panicking; convert_core_error has GIL-less fallback

**描述**: Python GIL/lock 交互在异常路径上可能导致宿主进程 panic。

**修复**: (1) `run_async()` 返回 `PyResult<T>` 而非 `T`——`blocking_recv()` 失败时返回 `PyRuntimeError` 而非 `.expect()` panic，消除 GIL/unwinding 交互风险。(2) `convert_core_error()` 中 `try_attach().expect()` 改为 `.unwrap_or_else()` fallback，GIL 不可用时仍返回有效 `PyErr`（无属性但消息完整）。(3) `chat_complete` 和 `stream_chat` 调用点分别处理两层 Result。

### P2-12: schema-retry JSON fallback [VULN-006]
**组件**: lattice-bus/src/runner.rs | **状态**: Resolved (2026-05-05) — post-loop validation gate; invalid schema is hard error; raw text wrapping marked

**描述**: schema 校验失败时的 JSON fallback 行为可能隐藏畸形 agent 输出。

**修复**: (1) Schema retry loop 后增加 `schema_valid` 门控——所有 retry 耗尽后若仍不符合 schema 则返回错误而非静默传递。(2) `output_schema` JSON/JSON Schema 解析失败改为硬错误（`Err`）而非 `warn!() + None` 跳过。(3) 非 JSON 输出 fallback 包装增加 `_raw_text: true` 标记，下游可区分结构化输出 vs 文本 fallback。

---

## P3 — LOW 代码质量问题（1 项）

### P3-05: SQLite FTS5 查询构造脆弱 [VULN-030]
**组件**: lattice-harness/src/memory/sqlite.rs | **状态**: Partial (3b68a95) — uses parameterization, format!() still present

# 深扫新增工程问题追踪

> 本分区记录 2026-05-02 全代码库深扫发现的**非 VULN 编号工程问题**。其中部分问题会影响真实 provider 可用性、profile-driven orchestration 正确性、插件语义闭环、Python/TUI/CLI 对外体验。安全类问题仍使用上方 VULN/P0-P3 编号追踪。

## 深扫新增统计

| 等级 | 数量 |
|------|------|
| DS-P0 | 2 |
| DS-P1 | 4 |
| DS-P2 | 4 |
| DS-P3 | 1 |
| **总计** | **11** |

---

## DS-P0 — 对外可用性 / 运行正确性阻断（2 项）

### DS-P0-02: resolve() 可能返回缺凭证或空 base_url 的伪可用模型

| 字段 | 值 |
|------|-----|
| **ID** | DS-P0-02 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/router.rs + catalog/data.json |
| **状态** | Resolved (2026-05-05) — resolve() rejects Missing creds + empty base_url; inspect_model() for diagnostics |

**描述**: 普通 catalog resolve、`provider_override`、permissive `provider/model` fallback 的 credential/base_url 语义不一致。部分路径即使 credential missing 或 provider default base_url 为空，也可能返回 `Ok(ResolvedModel)`。

**修复**: `resolve()` 在所有路径（provider_override、priority-sorted、credentialless）上拒绝 `CredentialStatus::Missing` 和空 base_url，返回 `LatticeError::Config`。新增 `inspect_model()` API 允许诊断命令（doctor/debug）获取 Missing 状态模型。`resolve_permissive` 已不存在。

---

### DS-P0-03: Harness profile 的 system prompt / tools / executor 未完整装配到 Agent

| 字段 | 值 |
|------|-----|
| **ID** | DS-P0-03 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-harness/src/pipeline.rs + runner.rs + micro_agent.rs |
| **状态** | Resolved (2026-05-05) — lattice-harness crate deleted; modules absorbed into agent/bus/plugin |

**描述**: 标准 pipeline 构建 `AgentRunner` 时主要执行 `Agent::new(resolved)` 和可选 memory 注入，未完整应用 profile 中的 `system.prompt`、`tools.enabled`、tool executor、sandbox policy 等配置。

**修复**: `lattice-harness` crate 已整体删除。Pipeline、AgentRunner、MicroAgent、profile 等模块已迁移至 agent/bus/plugin crate，装配逻辑在新位置重新实现。

---

## DS-P1 — 本迭代应修复的工程问题（4 项）

### DS-P1-03: Pipeline fork 缺少取消、冲突和资源控制语义

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-03 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-harness/src/pipeline.rs |
| **状态** | Resolved (2026-05-05) — harness deleted; pipeline migrated to lattice-bus with fork timeout/abort |

**描述**: fork branch 出错时，其他 spawned tasks 可能继续后台运行；partial fork result 可能继续 merge/route；多个 branch 产生不同 next target 时没有冲突策略。

**影响**: LLM/API 资源可能泄露，pipeline 结果不确定，fork handoff 在失败场景下不可审计。

**建议修复**: 为 fork 添加 branch 上限、timeout、cancellation、all/any 成功策略、next target 冲突策略和 deterministic merge policy。

---

### DS-P1-04: PluginRunner 未完整执行 Plugin trait 语义

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-04 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-plugin/src/lib.rs + erased_runner.rs |
| **状态** | Resolved (2026-05-05) — system_prompt, tools, timeout, output_schema, preferred_model all wired in run_plugin_loop |

**描述**: `Plugin` 暴露 `system_prompt()`、`tools()`、`preferred_model()`、`output_schema()`，`PluginConfig` 暴露 timeout/context_check，但 runner 未完整应用这些语义。

**影响**: 插件 API 看起来成熟，但运行时只完成 prompt → agent → parse → behavior 的部分闭环，开发者会误以为 schema、tools、preferred model、timeout 已生效。

**建议修复**: runner 应调用 `agent.set_system_prompt()`，注入 plugin tools，应用 preferred model 或显式声明由外部 agent 决定，执行 output schema validation，落实 timeout/context check。

---

### DS-P1-06: Bus pub/sub capability 未强制，delivery policy 基本未实现

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-06 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-bus/src/lib.rs |
| **状态** | Resolved (2026-05-05) — subscribe/publish capability enforced; AtLeastOnce logs handler errors; empty list = no restriction |

**描述**: `AgentBusConfig` 有 subscribe/publish/rpc，但只有 RPC whitelist 有部分检查。pub/sub API 没有 caller identity，无法强制 publish/subscribe 权限；`AtLeastOnce`、`subscriber_buffer`、handler error/backpressure 等语义未落实。

**影响**: profile 中的 bus capability 容易被误认为安全边界；高负载下 pub/sub 也缺乏可靠 delivery/backpressure 行为。

**建议修复**: `subscribe`/`publish` 增加 agent identity 并校验配置；delivery policy 未实现前标记 unsupported；handler 使用 bounded queue，并记录/传播 handler error。

---

### DS-P1-07: Memory API 职责重复，clone_box 默认实现可能丢失真实后端

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-07 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-agent/src/memory + lattice-bus/src/assembly.rs |
| **状态** | Resolved (2026-05-05) — Agent::with_memory takes Arc<dyn Memory>; SqliteMemory::clone_box panics on failure; assembly uses Arc::clone |

**描述**: `Agent`、`AgentRunner`、`Pipeline`、`MicroAgent` 都有 memory 概念，但职责边界不清。`Memory::clone_box` 默认返回新的 in-memory backend，若真实后端未覆盖，clone 后可能丢失 SQLite/共享后端语义。

**修复**: `Agent::with_memory` 签名改为 `Arc<dyn Memory>`，所有调用方共享同一实例。`SqliteMemory::clone_box` 失败时 panic 而非静默回退空 InMemoryMemory。`assembly.rs` 使用 `Arc::clone` 替代 `clone_box()`。

---

## DS-P2 — 计划中修复的设计/产品问题（4 项）

### DS-P2-01: Gemini native transport 已实现但默认 catalog 策略不清

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-01 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/transport/gemini.rs + streaming.rs + catalog/data.json |
| **状态** | Resolved (2026-05-05) — Design decision: catalog Gemini uses OpenAI-compatible endpoint (v1beta/openai) by default; GeminiTransport native path available for custom routing via api_protocol override |

**描述**: Gemini native streaming transport/parser 存在，但 catalog 默认 Gemini provider 更像走 OpenAI-compatible endpoint；文档中对 Gemini streaming/non-streaming 的描述也不一致。

**修复**: 明确策略——catalog 中 Gemini provider 使用 `chat_completions` 协议走 OpenAI 兼容端点（`base_url: v1beta/openai`），这是首选路径。`GeminiTransport` native transport 保留供 `api_protocol: "gemini"` 自定义路由使用。

---

### DS-P2-03: Python API 缺少 streaming、Agent/tool loop、credentials injection

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-03 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-python/src/* |
| **状态** | Partial (2026-05-05) — credentials injection + provider_override added; streaming still eager-collect; no Agent/tool loop |

**描述**: Python 当前比 resolver-only 更进一步，已有 non-streaming `chat_complete()`，但未暴露 streaming iterator/callback、Agent loop、tool continuation、credentials map、provider override。

**修复**: (1) `LatticeEngine(credentials={"KEY": "value"})` 构造函数注入凭据，不再仅依赖环境变量。(2) `resolve_model(model, provider_override="openrouter")` 可强制指定 provider。(3) 真正流式和 Agent 循环仍待实现。

---

### DS-P2-04: TUI 未接入主 CLI，且 UI 多轮与模型上下文可能不一致

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-04 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-tui + lattice-cli |
| **状态** | Resolved (2026-05-05) — Agent messages() accessor + history carry-forward on rebuild; model switch now preserves conversation |

**描述**: `lattice-cli` 无子命令时只提示 TUI 未实现，真实 TUI 是独立 `lattice-tui` binary；TUI UI 保存 messages，但每次 submit 可能新建 Agent，导致界面像多轮，模型实际只看到单轮上下文。

**修复**: (1) 添加 `Agent::messages()` 访问器暴露 `state.messages`。(2) TUI `app.rs` 在 agent rebuild（模型切换等）时提取旧 agent 的完整消息历史（含 tool calls/results），通过 `seed_messages()` 传入新 agent，优先于丢失工具上下文的 session 路径。

---

### DS-P2-06: ErrorClassifier 未消费 HTTP Retry-After header，限流等待不准

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-06 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/lib.rs + errors.rs |
| **状态** | Resolved (2026-05-05) — classify_with_retry_header() passes header directly; HTTP-date + seconds parsing via chrono; body injection hack removed |

**描述**: 非 2xx 响应路径只读取 body 并调用 `ErrorClassifier::classify(status, body, provider)`；`Retry-After` HTTP header 未传入分类器。

**修复**: 新增 `classify_with_retry_header()` 方法，`parse_retry_after_header()` 支持秒数和 RFC 2822 HTTP-date。`lib.rs` 和 `gemini.rs` 所有错误路径均直接传递 header 值，优先于 body 字段解析。旧的 `classify()` 保持向后兼容。

---

## DS-P3 — 延后处理的代码质量/体验问题（1 项）

### DS-P3-02: TUI 长消息渲染、滚动、终端恢复仍是 early-stage

| 字段 | 值 |
|------|-----|
| **ID** | DS-P3-02 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-tui/src/ui.rs + event.rs |
| **状态** | Partial (1d1ed59) — TerminalGuard added, rendering still early-stage |

**描述**: TUI 消息高度、长文本、滚动方向、多行输入、bracketed paste 恢复、panic 后终端恢复等体验仍比较粗糙。

**建议修复**: 基于内容高度布局；明确 scroll model；退出时恢复 bracketed paste；增加 panic hook 恢复 terminal state。

---

### DS-P3-03: OpenAICompatTransport 存在但 dispatcher 默认路径未真正使用

| 字段 | 值 |
|------|-----|
| **ID** | DS-P3-03 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/transport/openai_compat.rs + dispatcher.rs |
| **状态** | Resolved (2026-05-05) — deleted openai_compat.rs; ChatCompletionsTransport directly covers all OpenAI-compatible providers |

**描述**: `OpenAICompatTransport` 曾作为 ChatCompletionsTransport 的包装层存在，但两者都有 `TransportBase`，形成双层包装冗余。Dispatcher 始终直接用 `ChatCompletionsTransport::default()`。

**修复**: 删除 `openai_compat.rs`（219行），清理 `transport/mod.rs` pub mod/pub use，更新 `chat_completions.rs` 文档，移除 integration test 中的 redundant test module。ChatCompletionsTransport 自身已完整覆盖所有 OpenAI-compatible providers。

---

# 深度工程审计新增问题（2026-05-03 第二轮）

> 本分区记录第二轮深度审计发现的 runtime 阻断、架构断连、内存安全和产品性问题。

## 审计新增统计

| 等级 | 数量 |
|------|------|
| P0 | 0（5 项已全部修复） |
| P1 | 2 |
| P2 | 3 |
| P3 | 6 |
| **总计** | **11** |

---

## P0 — Runtime 阻断 / 架构断连 / Silent Failure（0 项 — 全部已修复）

> 5 项 P0 问题已全部修复并移入 resolved-issues.md。

---

## P1 — 本迭代应修的问题（2 项）

### P1-01: AgentState 和 Pipeline 使用 unwrap() 在 Mutex 上

**组件**: lattice-agent/src/memory/mod.rs, lattice-core/src/router.rs | **状态**: **Partially Resolved** (2026-05-05, active-issues-pass1)

**描述**: 多处 `Mutex.lock().unwrap()`，无 poison 恢复。一旦 panic，后续所有操作都会 panic。

**建议修复**: 关键路径使用 `.unwrap_or_else(|e| e.into_inner())`。

**修复**: 当前 runtime 路径中 `InMemoryMemory` 和 `ModelRouter::credential_cache` 已改为 poison recovery。测试用 `ENV_MUTEX.lock().unwrap()` 暂未纳入本项；SQLite memory 仍选择 poison 后返回空结果/丢弃写入，需另行评估数据一致性策略。

**验证**: `cargo test -p lattice-agent`、`cargo test -p lattice-core`。

---

### P1-02: Python Engine Mutex Poison 恢复不安全

**组件**: lattice-python/src/engine.rs | **状态**: Resolved (2026-05-05) — Mutex→RwLock; read() for all operations; poison recovery via unwrap_or_else

**描述**: 三个方法在 Mutex poison 后调用 `e.into_inner()`，但未验证内部数据完整性。into_inner() 返回可能半初始化的 router。

**修复**: (1) `Mutex<ModelRouter>` 改为 `RwLock<ModelRouter>`——所有操作（resolve/list_models/list_authenticated_models）都是 &self 读操作，RwLock 允许并发读且 poison 恢复语义更清晰。(2) 移除冗余的 `catch_unwind` 包装，改为直接的 `.read().unwrap_or_else(|e| e.into_inner())`。(3) RwLock poison 后 into_inner() 恢复的数据完整性风险低——ModelRouter 的唯一可变状态 `credential_cache` 由内部自有 Mutex 管理，写入是原子的 HashMap insert。

---

## P2 — 设计/产品/维护性问题（4 项）

### P2-03: Pipeline dry_run() 只跟踪默认路由

**组件**: lattice-bus/src/pipeline.rs:674-757 | **状态**: Resolved (2026-05-05) — BFS traversal explores all conditional + default + fork paths; cycle detection via DFS reachability

**修复**: Replaced single-path DFS (only default rules) with BFS that enqueues ALL rule targets (conditional + default + fallback + fork). Added post-traversal `can_reach_self()` DFS for true cycle detection (distinguishes A→B→A cycles from A→C, B→C DAG merges). Terminal detection now checks for agents with no outgoing targets (including rules with no target set).

---

### P2-04: AgentProfile system_prompt() 静默回退

**组件**: lattice-bus/src/profile.rs:290 | **状态**: Resolved (2026-05-05) — system_prompt() returns Result; file read / path traversal / canonicalize failures are hard errors

**修复**: Changed `system_prompt() -> String` to `system_prompt() -> Result<String, Box<dyn Error>>`. Four silent fallback paths eliminated: absolute/`..` paths, canonicalize failure (file not found), path outside cwd, and read failure all return `Err`. Updated `assemble_agent()` return type to `Result`; pipeline and micro_agent call sites handle assembly errors explicitly. Added `BusError::Config` variant for micro_agent error propagation.

---

### P2-05: PipelineEvent::Token 从未发射

**组件**: lattice-bus/src/events.rs | **状态**: Resolved (2026-05-05) — removed dead Token variant; added #[non_exhaustive]

**修复**: `PipelineEvent::Token` 从未被构造——删除死代码变体，添加 `#[non_exhaustive]` 防止未来静默破坏。

---

### P2-06: CLI -p mode 和 run subcommand 命名混乱

**组件**: lattice-cli/src/main.rs:29-30 | **状态**: Resolved (2026-05-05) — renamed `-p`/`--print` to `-q`/`--quick`; deleted `print.rs`; quick mode now calls `run::run()` with `max_turns=1`, unified execution path

**修复**: (1) `--print`/`-p` → `--quick`/`-q`，语义明确为"快速单次查询（无工具执行）"。(2) 删除 `print.rs`（251行独立执行路径 + 手动2工具循环），`-q` 内部调用 `run::run()` 并设置 `max_turns=1`，消除代码分叉。

---

## P3 — 代码质量/体验问题（7 项）

### P3-01: Agent::new() 依赖字段声明顺序

**组件**: lattice-agent/src/lib.rs:82-90 | **状态**: Resolved (2026-05-05) — pre-clone resolved before AgentState::new() consumes it, eliminating field-order dependency

---

### P3-02: MicroAgentHandle watch_and_deregister 消耗 self

**组件**: lattice-bus/src/micro_agent.rs:58-69 | **状态**: Resolved (2026-05-05) — handle now stores Arc<dyn Bus> internally; watch_and_deregister(self) no longer requires external bus parameter

---

### P3-03: Bus echo_agent_loop 吞掉 send error

**组件**: lattice-bus/src/lib.rs:252-258 | **状态**: Resolved (2026-05-05) — replaced `let _ = send()` with `.is_err()` + tracing::warn

---

### P3-04: --json mode pipeline run 缺少工具调用信息

**组件**: lattice-cli/src/commands/run.rs:320-336 | **状态**: Partial (2026-05-05) — full output now emitted (was truncated to 200 chars); tool call detail requires AgentRunner architecture change

---

### P3-05: Events send() unchecked

**组件**: lattice-bus/src/events.rs | **状态**: Resolved (2026-05-05) — send() returns receiver count; no silent discard

---

### P3-06: PluginRegistry register() 需要 &mut self

**组件**: lattice-plugin/src/registry.rs | **状态**: Resolved (2026-05-05) — internal HashMap wrapped in RwLock; register() now takes &self; get() returns Arc<PluginBundle>

---

### P3-07: Python maturin 发布配置缺失

**组件**: lattice-python/pyproject.toml | **状态**: Resolved (2026-05-05) — added description, readme, license, classifiers, authors, project.urls, module-name, python-source

---

## 交叉引用

- [安全审计问题索引](../issues/README.md) — 所有 36 个 VULN 的详细描述和复现步骤
- [VULN-001 换行符注入](../issues/VULN-001-sandbox-newline-bypass.md)
- [VULN-002 写白名单子串匹配](../issues/VULN-002-sandbox-write-allowlist-substring-bypass.md)
- [VULN-003 敏感文件过滤非规范](../issues/VULN-003-sandbox-sensitive-file-filter-noncanonical.md)
- [VULN-004 进程环境变量污染](../issues/VULN-004-process-env-cross-request-contamination.md)
- [VULN-005 Python 绑定锁中毒](../issues/VULN-005-python-binding-lock-poisoning-dos.md)
- [VULN-006 Schema-retry JSON fallback](../issues/VULN-006-agent-json-fallback-can-mask-invalid-structured-output.md)
- [resolved-issues.md](resolved-issues.md) — 已解决问题的完整归档---

