# LATTICE 活跃问题追踪

**最后更新**: 2026-05-03
**状态**: ALL ISSUES RESOLVED — see [resolved-issues.md](resolved-issues.md) for archive

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
| P0 (CRITICAL) | 7 |
| P1 (HIGH) | 9 |
| P2 (MEDIUM) | 12 |
| P3 (LOW) | 8 |
| **总计** | **36** |

---

## P0 — CRITICAL 安全漏洞（7 项）

### P0-01: sandbox check_command() 未阻止 \n/\r 换行符命令注入 [VULN-001]

| 字段 | 值 |
|------|-----|
| **ID** | P0-01 / VULN-001 |
| **首次报告** | 2026-05-01 |
| **来源** | [VULN-001](../issues/VULN-001-sandbox-newline-bypass.md) |
| **组件** | lattice-agent/src/sandbox.rs |
| **文件** | `sandbox.rs:119` |
| **状态** | Resolved (6d5b044) |

**描述**: check_command() 的 metacharacter 黑名单未包含 \\n 和 \\r。sh -c 将换行符解析为命令分隔符。攻击路径: {"command": "cargo test\\nrm -rf /"} → cargo 通过 allowlist → 执行两条命令。

**根因**: 黑名单是基于符号 shell 操作符（;、|、&& 等）枚举构建的，未考虑 POSIX shell 也将空白字符（\\n, \\r）作为命令分隔符处理。

**建议修复**: 将 \\n 和 \\r 添加到 metacharacter 黑名单。根本方案: 放弃 sh -c，用 std::process::Command 直接执行二进制。

---

### P0-02: check_command() 未阻止 & 后台命令注入 [VULN-007]

| 字段 | 值 |
|------|-----|
| **ID** | P0-02 / VULN-007 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/sandbox.rs |
| **文件** | `sandbox.rs:119` |
| **状态** | Resolved (bdd6341) |

**描述**: metacharacter 黑名单包含 "&&" 但不包含独立 "&"。sh -c 将 & 解析为后台操作符。{"command": "cargo test & rm -rf /"} 通过所有检查。

**根因**: 黑名单模型不完整。独立 & 字符被遗漏。

**建议修复**: 添加 & 到黑名单（注意 & 在 && 之前匹配）。根本方案: 放弃 sh -c。

---

### P0-03: check_url() URL 解析混淆导致 SSRF [VULN-008]

| 字段 | 值 |
|------|-----|
| **ID** | P0-03 / VULN-008 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/sandbox.rs |
| **文件** | `sandbox.rs:162` |
| **状态** | Resolved (b5d57c3) |

**描述**: check_url() 使用 starts_with("http://localhost") 做字符串前缀匹配。根据 RFC 3986，"http://localhost@evil.com/" 中 localhost 被视为用户名而非主机。攻击者可绕过 localhost-only 限制。

**根因**: 字符串前缀匹配代替 URL 解析。应使用 url 库正确提取 host 组件。

**建议修复**: 使用 url::Url::parse() 解析 host 组件，对 host 而非原始字符串做检查。

---

### P0-04: HTTPS 到内网 SSRF 完全未限制 [VULN-009]

| 字段 | 值 |
|------|-----|
| **ID** | P0-04 / VULN-009 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/sandbox.rs |
| **文件** | `sandbox.rs:162` |
| **状态** | Resolved (bdd6341) |

**描述**: check_url() 对 https:// URL 无任何 host 检查。web_search 工具可访问 https://127.0.0.1:6379/（本地 Redis）、https://169.254.169.254/（AWS 元数据）、https://localhost:8443/ 等。

**根因**: https:// 被默认认为安全，但目的 host 并非如此。无内网/保留地址检查。

**建议修复**: 对 https:// URL 同样检查 host，拒绝 127.0.0.1/::1/10.0.0.0/8/172.16.0.0/12/192.168.0.0/16/169.254.0.0/16。

---

### P0-05: bash 命令执行无超时控制 [VULN-010]

| 字段 | 值 |
|------|-----|
| **ID** | P0-05 / VULN-010 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/sandbox.rs + tools.rs |
| **文件** | `sandbox.rs:20` / `tools.rs:135` |
| **状态** | Resolved (bdd6341) |

**描述**: max_command_timeout: u32 = 30 已定义但 tools.rs 中的 bash 执行完全未使用。permissive 模式下 sleep 99999 即永久阻塞 agent 线程。

**根因**: 超时字段被定义但执行路径未消费。设计与实现脱节。

**建议修复**: 添加 .spawn() + .wait_timeout() 或 tokio::time::timeout 包装。

---

### P0-06: Agent 配置文件无完整性校验 [VULN-028]

| 字段 | 值 |
|------|-----|
| **ID** | P0-06 / VULN-028 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-harness/src/profile.rs |
| **文件** | `profile.rs:114-118` |
| **状态** | Resolved (bdd6341) |

**描述**: agent.toml 配置文件无数字签名或哈希校验。攻击者可添加 default=true 的 handoff 规则重定向到恶意 agent。watcher 自动 hot-reload 无完整性检查。

**根因**: 配置文件来源不可信但被盲目信任。无签名机制。

**建议修复**: 为 agent.toml 添加 SHA-256 哈希或 Ed25519 签名验证。

---

### P0-07: read_allowlist 是 dead code — 任意文件读取 [VULN-012]

| 字段 | 值 |
|------|-----|
| **ID** | P0-07 / VULN-012 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/sandbox.rs |
| **文件** | `sandbox.rs:73-88` |
| **状态** | Resolved (b5d57c3) |

**描述**: read_allowlist 字段被定义并文档化但 check_read() 方法从未引用它。即使设置了读取目录限制，也完全不被执行。攻击者可读取任何不被 sensitive_files 匹配的文件。

**根因**: 字段实现遗漏。read_allowlist 从未集成到 check_read() 的验证逻辑中。

**建议修复**: 在 check_read() 中添加 read_allowlist 检查逻辑。

---

## P1 — HIGH 安全漏洞（9 项）

### P1-01: check_command() 未阻止 > 和 < 重定向 [VULN-011]

| 字段 | 值 |
|------|-----|
| **ID** | P1-01 / VULN-011 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/sandbox.rs |
| **文件** | `sandbox.rs:119` |
| **状态** | Resolved (22863b8) |

**描述**: > 和 < 不在 metacharacter 黑名单中。"grep . < /etc/shadow" 读取系统文件，"cargo test > /etc/crontab" 覆盖关键文件。bash 工具仅调用 check_command()，不调用 check_read/check_write，完全绕过文件沙箱。

**根因**: shell 层和文件层之间有空隙。重定向在 shell 层操作文件，不经过路径沙箱。

**建议修复**: 添加到黑名单。根本方案: 放弃 sh -c。

---

### P1-02: 文件操作 TOCTOU 竞态条件 [VULN-013]

| 字段 | 值 |
|------|-----|
| **ID** | P1-02 / VULN-013 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/tools.rs |
| **文件** | `tools.rs:49-63, 90-99` |
| **状态** | Resolved (22863b8) |

**描述**: check_read/check_write 和实际 fs 操作之间无原子性。并发攻击可替换 symlink 指向后再执行操作。

**根因**: 检查-使用时间差。无 O_NOFOLLOW 或 canonicalize 后二次检查。

**建议修复**: 先 canonicalize 路径再在 canonical 路径上做二次检查。

---

### P1-03: web_search 请求无超时 [VULN-014]

| 字段 | 值 |
|------|-----|
| **ID** | P1-03 / VULN-014 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-agent/src/tools.rs |
| **文件** | `tools.rs:195` |
| **状态** | Resolved (22863b8) |

**描述**: reqwest::blocking::get(url) 无任何超时配置。攻击者可通过 Slowloris 技术永久阻塞 agent 线程。

**根因**: 使用便捷函数而非带超时配置的 Client。

**建议修复**: 使用 Client::builder().timeout(30s).build()。

---

### P1-04: sandbox write allowlist substring matching [VULN-002]

| 字段 | 值 |
|------|-----|
| **ID** | P1-04 / VULN-002 |
| **首次报告** | 2026-05-01 |
| **来源** | [VULN-002](../issues/VULN-002-sandbox-write-allowlist-substring-bypass.md) |
| **组件** | lattice-agent/src/sandbox.rs |
| **文件** | `sandbox.rs:97` |
| **状态** | Resolved (22863b8) |

**描述**: check_write() 使用 path.contains(prefix) 子串匹配。evil-lattice-core/ 匹配 lattice-core/ 前缀。结合 symlink 可写入任意位置。

**根因**: 子串匹配而非路径规范化 + 目录前缀匹配。

**建议修复**: 使用 canonicalize + starts_with 目录前缀匹配。

---

### P1-05: 进程环境变量跨请求污染 [VULN-004]

| 字段 | 值 |
|------|-----|
| **ID** | P1-05 / VULN-004 |
| **首次报告** | 2026-05-01 |
| **来源** | [VULN-004](../issues/VULN-004-process-env-cross-request-contamination.md) |
| **组件** | lattice-cli/src/commands/resolve.rs |
| **状态** | Resolved (22863b8) |

**描述**: 使用 std::env::set_var() 修改进程级环境变量。多请求并发时污染其他请求的凭证解析。

**根因**: 进程级可变状态用于请求级上下文。

**建议修复**: 使用 with_credentials() HashMap 而非 set_var。

---

### P1-06: SSRF via HTTP 重定向跟踪 [VULN-020]

| 字段 | 值 |
|------|-----|
| **ID** | P1-06 / VULN-020 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-core/src/provider.rs |
| **文件** | `provider.rs:8-14` |
| **状态** | Resolved (22863b8) |

**描述**: reqwest::Client 默认跟踪 HTTP 重定向。恶意端点可 302 → http://169.254.169.254/。validate_base_url() 仅检查初始 URL。Authorization header 随重定向发送。

**根因**: 默认重定向策略 + URL 验证仅覆盖初始请求。

**建议修复**: 设置 .redirect(Policy::none()) 或自定义验证每次重定向目标。

---

### P1-07: 无界限 fork 目标线程轰炸 [VULN-029]

| 字段 | 值 |
|------|-----|
| **ID** | P1-07 / VULN-029 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-harness/src/handoff_rule.rs + pipeline.rs |
| **文件** | `handoff_rule.rs:31-38` / `pipeline.rs:535-594` |
| **状态** | Resolved (22863b8) |

**描述**: HandoffTarget::parse() 接受任意数量的 fork 目标，无上限。可创建 500 个线程同时调用 LLM API。

**根因**: fork 分支数无上限。

**建议修复**: 在反序列化时限制 fork 分支数上限（如 10）。

---

### P1-08: system.file symlink 遍历 [VULN-032]

| 字段 | 值 |
|------|-----|
| **ID** | P1-08 / VULN-032 |
| **首次报告** | 2026-05-02 |
| **来源** | 蓝军自攻击安全审计 |
| **组件** | lattice-harness/src/profile.rs |
| **文件** | `profile.rs:127-144` |
| **状态** | Resolved (22863b8) |

**描述**: system_prompt() 拒绝 .. 但不解析 symlink。可创建 symlink 指向敏感文件，内容作为系统提示词注入 LLM provider。

**根因**: 路径检查未调用 canonicalize() 解析 symlink。

**建议修复**: 在读取前 canonicalize 路径并验证在允许的目录前缀内。

---

### P1-09: sandbox sensitive-file 过滤非规范 [VULN-003]

| 字段 | 值 |
|------|-----|
| **ID** | P1-09 / VULN-003 |
| **首次报告** | 2026-05-01 |
| **来源** | [VULN-003](../issues/VULN-003-sandbox-sensitive-file-filter-noncanonical.md) |
| **组件** | lattice-agent/src/sandbox.rs |
| **状态** | Resolved (22863b8) |

**描述**: 敏感文件过滤使用 path.contains(".env") 子串匹配，不解析路径。可能误挡合法文件，也可能被编码或 symlink 绕过。

**根因**: 路径未 canonicalize，匹配逻辑使用子串而非路径组件匹配。

**建议修复**: canonicalize 后做路径组件匹配而非子串匹配。

---

## P2 — MEDIUM 安全/设计问题（12 项）

### P2-01: 敏感文件列表严重不完整 [VULN-015]
**组件**: lattice-agent/src/sandbox.rs | **文件**: `sandbox.rs:36-43` | **状态**: Resolved (3b68a95)

**描述**: 仅 6 项。缺少 .git/config、~/.ssh/id_*、~/.aws/credentials、.npmrc 等。

### P2-02: DNS rebinding 绕过 HTTPS SSRF [VULN-016]
**组件**: lattice-agent/src/sandbox.rs | **文件**: `sandbox.rs:162` | **状态**: Resolved (3b68a95)

**描述**: DNS rebinding 服务（nip.io, xip.io）可解析域名到内网 IP，绕过 host 检查。

### P2-03: reqwest 默认重定向跟踪 SSRF [VULN-017]
**组件**: lattice-agent/src/tools.rs | **文件**: `tools.rs:195` | **状态**: Resolved (3b68a95)

**描述**: 302 重定向可绕过 URL scheme 检查，访问内网 HTTP 服务。

### P2-04: Bash ANSI-C 引用绕过 [VULN-018]
**组件**: lattice-agent/src/sandbox.rs | **文件**: `sandbox.rs:119` | **状态**: Resolved (3b68a95)

**描述**: $'\\n' 展开为换行符绕过黑名单。$'\\x3b' → ;。

### P2-05: 多向量组合绕过 [VULN-019]
**组件**: lattice-agent/src/sandbox.rs | **文件**: `sandbox.rs:132-133` | **状态**: Resolved (3b68a95)

**描述**: PATH 注入 + 子进程继承 + check_command 参数不验证。

### P2-06: URL 验证 userinfo 注入绕过 [VULN-021]
**组件**: lattice-core/src/router.rs | **文件**: `router.rs:592-624` | **状态**: Resolved (3b68a95)

**描述**: https://anything@169.254.169.254/ 中 anything 被视为用户名而非 host。

### P2-07: HTTP header 注入 via header: 前缀 [VULN-022]
**组件**: lattice-core/src/lib.rs + transport/gemini.rs | **状态**: Resolved (6d5b044)

**描述**: provider_specific 的 header: 机制可注入任意 HTTP 头，无白名单。

### P2-08: 凭证通过错误响应体泄露 [VULN-023]
**组件**: lattice-core/src/errors.rs + lib.rs | **状态**: Resolved (6d5b044)

**描述**: 非 2xx 响应体存入错误消息，可能含 API key 回显。

### P2-09: WebSocket 端点无鉴权 [VULN-031]
**组件**: lattice-harness/src/ws.rs | **文件**: `ws.rs:22-59` | **状态**: Resolved (3b68a95)

**描述**: /ws 端点无 Origin/Token/CORS。跨域可监听 pipeline 事件泄露。

### P2-10: LATTICE_AGENTS_DIR 环境变量毒化 [VULN-033]
**组件**: lattice-harness + lattice-cli | **状态**: Resolved (3b68a95)

**描述**: 环境变量可覆盖 agent 目录到攻击者控制的路径。

### P2-11: Python binding lock poisoning [VULN-005]
**组件**: lattice-python | **状态**: Resolved (3b68a95)

**描述**: Python GIL/lock 交互在异常路径上可能导致宿主进程 panic。

### P2-12: schema-retry JSON fallback [VULN-006]
**组件**: lattice-harness | **状态**: Resolved (3b68a95)

**描述**: schema 校验失败时的 JSON fallback 行为可能隐藏畸形 agent 输出。

---

## P3 — LOW 代码质量问题（8 项）

### P3-01: URL 验证接受非 HTTP 协议 [VULN-024]
**组件**: lattice-core/src/router.rs | **状态**: Resolved (3b68a95)

### P3-02: 缺少全局请求超时 [VULN-025]
**组件**: lattice-core/src/provider.rs | **状态**: Resolved (3b68a95)

### P3-03: chat_endpoint 覆盖可重定向 [VULN-026]
**组件**: lattice-core/src/lib.rs | **状态**: Resolved (3b68a95)

### P3-04: Gemini URL model ID 无长度限制 [VULN-027]
**组件**: lattice-core/src/transport/gemini.rs | **状态**: Resolved (3b68a95)

### P3-05: SQLite FTS5 查询构造脆弱 [VULN-030]
**组件**: lattice-harness/src/memory/sqlite.rs | **状态**: Resolved (3b68a95)

### P3-06: diagnostics() 凭证枚举 [VULN-034]
**组件**: lattice-cli/src/credentials.rs | **状态**: Resolved (3b68a95)

### P3-07: 内存条目无大小限制 [VULN-035]
**组件**: lattice-harness/src/pipeline.rs | **状态**: Resolved (3b68a95)

### P3-08: 深度嵌套 JSON 栈溢出风险 [VULN-036]
**组件**: lattice-harness/src/handoff_rule.rs | **状态**: Resolved (3b68a95)

---

# 深扫新增工程问题追踪

> 本分区记录 2026-05-02 全代码库深扫发现的**非 VULN 编号工程问题**。其中部分问题会影响真实 provider 可用性、profile-driven orchestration 正确性、插件语义闭环、Python/TUI/CLI 对外体验。安全类问题仍使用上方 VULN/P0-P3 编号追踪。

## 深扫新增统计

| 等级 | 数量 |
|------|------|
| DS-P0 | 4 |
| DS-P1 | 8 |
| DS-P2 | 6 |
| DS-P3 | 4 |
| **总计** | **22** |

---

## DS-P0 — 对外可用性 / 运行正确性阻断（4 项）

### DS-P0-01: Transport extra_headers 未合并到真实 HTTP 请求路径

| 字段 | 值 |
|------|-----|
| **ID** | DS-P0-01 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/lib.rs + transport/* |
| **状态** | Resolved (6d5b044) |

**影响**: Anthropic Messages API 真实调用可能因缺少 `anthropic-version` header 失败；OpenAI-compatible provider 的扩展 header 机制也容易失效。

**建议修复**: 在 `send_streaming_request()` 中统一合并 `transport.extra_headers()`、调用方显式 headers、`provider_specific` 的 `header:*`、以及 auth header；为 Anthropic 增加真实请求路径测试。

---

### DS-P0-02: resolve() 可能返回缺凭证或空 base_url 的伪可用模型

| 字段 | 值 |
|------|-----|
| **ID** | DS-P0-02 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/router.rs + catalog/data.json |
| **状态** | Resolved (22863b8) |

**描述**: 普通 catalog resolve、`provider_override`、permissive `provider/model` fallback 的 credential/base_url 语义不一致。部分路径即使 credential missing 或 provider default base_url 为空，也可能返回 `Ok(ResolvedModel)`。

**影响**: 用户看到 resolve 成功，但 `chat()` 阶段才因 401、invalid URL、empty base_url 等失败，破坏“自动选择可用 provider/credential”的核心承诺。

**建议修复**: 明确区分 `resolve()` 与 debug/doctor 用的 inspect API。默认 `resolve()` 只返回可调用模型；新增 `resolve_allow_missing()` 或 `inspect_model()` 才允许返回 `CredentialStatus::Missing`。

---

### DS-P0-03: Harness profile 的 system prompt / tools / executor 未完整装配到 Agent

| 字段 | 值 |
|------|-----|
| **ID** | DS-P0-03 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-harness/src/pipeline.rs + runner.rs + micro_agent.rs |
| **状态** | Resolved (22863b8) |

**描述**: 标准 pipeline 构建 `AgentRunner` 时主要执行 `Agent::new(resolved)` 和可选 memory 注入，未完整应用 profile 中的 `system.prompt`、`tools.enabled`、tool executor、sandbox policy 等配置。

**影响**: TOML profile 看起来定义了 agent 行为，但运行时 agent 可能没有系统提示词、没有工具定义或没有工具执行器。profile-driven orchestration 语义不成立。

**建议修复**: 在 runner/build path 中统一装配 `profile.system_prompt()`、工具定义、工具执行器和 sandbox；如果 profile 声明工具但 executor 缺失，应显式报错或禁用工具，不应静默运行。

---

### DS-P0-04: AgentRunner 吞掉 LoopEvent::Error 并可能把失败当成功输出

| 字段 | 值 |
|------|-----|
| **ID** | DS-P0-04 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-harness/src/runner.rs |
| **状态** | Resolved (22863b8) |

**描述**: `AgentRunner::run_once()` 主要收集 `LoopEvent::Token`，没有把 `LoopEvent::Error` 转成 `Err`。如果底层 model/provider/auth/network 失败，runner 可能把空内容 fallback 成 `{"content":""}` 并继续 pipeline。

**影响**: pipeline 的 `errors` 列表不可靠，handoff 可能基于空成功结果继续执行，schema retry 也可能掩盖真实 transport/auth/model 错误。

**建议修复**: 只要出现 `LoopEvent::Error` 即返回 error；空输出应单独建模；schema retry 只处理结构化输出错误，不应吞掉底层调用失败。

---

## DS-P1 — 本迭代应修复的工程问题（8 项）

### DS-P1-01: Catalog context_length 全为 0，模型能力目录不可用

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-01 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/catalog/data.json |
| **状态** | Resolved (1d1ed59) |

**描述**: catalog 中模型虽然暴露 `context_length` 字段，但当前数据基本为 0。

**影响**: token budget、context trimming、ContextWindowExceeded 预防、模型能力展示都无法可靠工作，削弱 model-centric engine 的核心价值。

**建议修复**: 为 catalog 模型填充真实 context window；增加 catalog schema/CI 校验，禁止 known model 的 `context_length = 0` 静默进入主分支。

---

### DS-P1-02: lattice-core::chat() 不使用 retry policy，文档和实现语义不一致

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-02 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/lib.rs + retry.rs + errors.rs |
| **状态** | Resolved (1d1ed59) |

**描述**: `RetryPolicy` 和 `ErrorClassifier::is_retryable()` 存在，但 `lattice_core::chat()` 真实请求路径不执行 retry；retry 主要在 `lattice-agent` 外层包装。

**影响**: 直接使用 core API 的调用方不会获得文档暗示的 retry 行为，core/agent 行为割裂。

**建议修复**: 二选一：将 retry 接入 core 首请求路径；或修改文档，明确 core 只做错误分类，retry 属于 agent/harness 层。

---

### DS-P1-03: Pipeline fork 缺少取消、冲突和资源控制语义

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-03 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-harness/src/pipeline.rs |
| **状态** | Resolved (1d1ed59) |

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
| **状态** | Resolved (1d1ed59) |

**描述**: `Plugin` 暴露 `system_prompt()`、`tools()`、`preferred_model()`、`output_schema()`，`PluginConfig` 暴露 timeout/context_check，但 runner 未完整应用这些语义。

**影响**: 插件 API 看起来成熟，但运行时只完成 prompt → agent → parse → behavior 的部分闭环，开发者会误以为 schema、tools、preferred model、timeout 已生效。

**建议修复**: runner 应调用 `agent.set_system_prompt()`，注入 plugin tools，应用 preferred model 或显式声明由外部 agent 决定，执行 output schema validation，落实 timeout/context check。

---

### DS-P1-05: Runtime 模型混用 static Runtime、block_on、tokio::spawn，嵌入场景易出问题

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-05 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-agent + lattice-harness + lattice-python |
| **状态** | Resolved (1d1ed59) |

**描述**: 项目中存在多种 runtime 模式：CLI async path、pipeline static runtime sync wrapper、micro-agent bus runtime、Python binding runtime、`tokio::spawn` 对当前 runtime 的隐式依赖等。

**影响**: 嵌入到已有 Tokio runtime、Python runtime 或服务端 async 应用时，可能出现 panic、nested runtime、阻塞 worker thread 或 lifecycle 混乱。

**建议修复**: 核心库 API async-first；sync wrapper 限制在 CLI/convenience 层并检测当前 runtime；blocking SQLite/IO 使用 `spawn_blocking`；micro-agent 提供显式 `spawn_async()`。

---

### DS-P1-06: Bus pub/sub capability 未强制，delivery policy 基本未实现

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-06 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-bus/src/lib.rs |
| **状态** | Resolved (1d1ed59) |

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
| **组件** | lattice-agent/src/memory + lattice-harness/src/memory |
| **状态** | Resolved (1d1ed59) |

**描述**: `Agent`、`AgentRunner`、`Pipeline`、`MicroAgent` 都有 memory 概念，但职责边界不清。`Memory::clone_box` 默认返回新的 in-memory backend，若真实后端未覆盖，clone 后可能丢失 SQLite/共享后端语义。

**影响**: `Agent::with_memory()` 看起来有效但 agent loop 内使用有限；shared/private/session memory 语义容易产生误解和数据丢失。

**建议修复**: 统一 memory ownership：要么由 harness 管理，要么 agent 通过明确 hook 使用；移除危险默认 `clone_box`，优先使用 `Arc<dyn Memory>`；为 SQLite memory 明确 clone/share 语义。

---

### DS-P1-08: CLI credential、tool、session/config 行为不一致或半成品暴露

| 字段 | 值 |
|------|-----|
| **ID** | DS-P1-08 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-cli/src/* |
| **状态** | Resolved (1d1ed59) |

**描述**: `resolve/models` 与 `run/print/debug/doctor` 的 credential 加载路径不完全一致；`print` 与 `run` 的 tool 集合和执行策略不同；`sessions`、`stats`、`config set`、`--continue-session`、`--no-save` 等功能部分仍是 placeholder。

**影响**: 用户会看到同一配置在不同命令下行为不同；半成品 flag/命令暴露会造成产品可信度下降。

**建议修复**: 统一 credential injection；统一 tool registry/sandbox；未实现命令 hidden/experimental 或补齐实现；更新 CLI help 和 README 示例。

---

## DS-P2 — 计划中修复的设计/产品问题（6 项）

### DS-P2-01: Gemini native transport 已实现但默认 catalog 策略不清

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-01 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/transport/gemini.rs + streaming.rs + catalog/data.json |
| **状态** | Resolved (1d1ed59) |

**描述**: Gemini native streaming transport/parser 存在，但 catalog 默认 Gemini provider 更像走 OpenAI-compatible endpoint；文档中对 Gemini streaming/non-streaming 的描述也不一致。

**影响**: 实现、数据、文档三者脱节，后续维护和用户预期都不稳定。

**建议修复**: 明确 Gemini 策略：要么 catalog 默认走 native `gemini_generate_content`，要么把 native transport 标为 experimental/custom-only，并同步文档。

---

### DS-P2-02: SSE parser EOF residual buffer 未 flush，可能丢最后事件

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-02 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/streaming.rs |
| **状态** | Resolved (1d1ed59) |

**描述**: `sse_from_bytes_stream()` 只在遇到空行分隔时解析事件，stream EOF 时未 flush residual buffer。虽然规范 SSE 应以空行结束，但真实 provider/proxy 可能不稳定。

**影响**: 最后一个未以空行终止的 SSE event 可能被丢弃。

**建议修复**: bytes stream 结束时如果 buffer 非空，尝试解析 residual event；增加无尾随空行测试。

---

### DS-P2-03: Python API 缺少 streaming、Agent/tool loop、credentials injection

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-03 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-python/src/* |
| **状态** | Resolved (1d1ed59) |

**描述**: Python 当前比 resolver-only 更进一步，已有 non-streaming `chat_complete()`，但未暴露 streaming iterator/callback、Agent loop、tool continuation、credentials map、provider override。

**影响**: Python 用户无法使用 Rust 主线中的 streaming/agent/tool 能力，embedding 场景也难以避免全局环境变量依赖。

**建议修复**: 暴露 streaming API；提供 credentials/provider override；增加 Agent 或 submit_tools continuation；修正文档中 resolver-only/StreamIterator 的矛盾描述。

---

### DS-P2-04: TUI 未接入主 CLI，且 UI 多轮与模型上下文可能不一致

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-04 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-tui + lattice-cli |
| **状态** | Resolved (1d1ed59) |

**描述**: `lattice-cli` 无子命令时只提示 TUI 未实现，真实 TUI 是独立 `lattice-tui` binary；TUI UI 保存 messages，但每次 submit 可能新建 Agent，导致界面像多轮，模型实际只看到单轮上下文。

**影响**: 用户体验和模型行为不一致；README/CLI 入口预期不可靠。

**建议修复**: 明确 TUI 入口；让 CLI 启动 TUI 或文档改为 `lattice-tui`；TUI 应复用长期 Agent 或把历史消息传入下一轮。

---

### DS-P2-05: README/STATUS/ROADMAP/CLAUDE 与当前代码状态漂移

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-05 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | README.md + STATUS.md + ROADMAP.md + CLAUDE.md |
| **状态** | Resolved (1d1ed59) |

**描述**: 文档仍提到 `lattice-memory`/`lattice-token-pool` 独立 crate，未解释 `lattice-bus`；CLI 示例和实际命令不匹配；Python 状态在 resolver-only、chat_complete、StreamIterator 之间互相矛盾；STATUS 中的 runtime/memory refactor 描述与当前代码不完全一致。

**影响**: 新贡献者和用户会被文档误导，后续问题定位成本增加。

**建议修复**: 以当前 workspace 为准重写项目结构、CLI 示例、Python/TUI/plugin 状态和 known limitations；将历史状态移入 changelog 或 resolved docs。

---

### DS-P2-06: ErrorClassifier 未消费 HTTP Retry-After header，限流等待不准

| 字段 | 值 |
|------|-----|
| **ID** | DS-P2-06 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-core/src/lib.rs + errors.rs |
| **状态** | Resolved (1d1ed59) |

**描述**: 非 2xx 响应路径只读取 body 并调用 `ErrorClassifier::classify(status, body, provider)`；`Retry-After` HTTP header 未传入分类器。

**影响**: `RateLimit.retry_after` 多数情况下为 None，agent retry 只能使用本地 jitter backoff，不能尊重 provider 指定等待时间。

**建议修复**: error classification 接收 response headers；优先解析标准 `Retry-After` header，再 fallback 到 body 字段。

---

## DS-P3 — 延后处理的代码质量/体验问题（4 项）

### DS-P3-01: Plugin retry 未带 correction hint，重复同一 prompt 成本高

| 字段 | 值 |
|------|-----|
| **ID** | DS-P3-01 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-plugin/src/erased_runner.rs |
| **状态** | Resolved (1d1ed59) |

**描述**: parse/schema/confidence 失败后的 retry 多数情况下重新发送同一 prompt，未包含上次 raw output、parse error、schema expectation 或 correction instruction。

**建议修复**: retry prompt 中加入结构化 correction hint，避免模型重复产生同样坏输出。

---

### DS-P3-02: TUI 长消息渲染、滚动、终端恢复仍是 early-stage

| 字段 | 值 |
|------|-----|
| **ID** | DS-P3-02 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | lattice-tui/src/ui.rs + event.rs |
| **状态** | Resolved (1d1ed59) |

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
| **状态** | Resolved (1d1ed59) |

**描述**: `OpenAICompatTransport` 有独立结构和文档，但 dispatcher 对 `ApiProtocol::OpenAiChat` 返回的是 `ChatCompletionsTransport::default()`，真实 URL/auth/header 多来自 `ResolvedModel`，使 compat transport 更像未接入/测试遗留。

**建议修复**: 明确 OpenAI-compatible provider 的实现策略：删除/合并未用 transport，或让 dispatcher/provider defaults 能真正选择 compat transport。

---

### DS-P3-04: Workspace 测试通过但 warnings 未收敛

| 字段 | 值 |
|------|-----|
| **ID** | DS-P3-04 |
| **首次报告** | 2026-05-02 |
| **来源** | 全代码库深扫 |
| **组件** | workspace tests/examples |
| **状态** | Resolved (1d1ed59) |

**描述**: `cargo test --workspace --quiet` 当前通过，但存在 unused imports、unused variable、deprecated API 使用等 warnings。

**建议修复**: 清理 warnings，并在 CI 中继续保持 `cargo clippy -- -D warnings` 和 `cargo fmt --check --all`。

---

## 交叉引用

- [安全审计问题索引](../issues/README.md) — 所有 36 个 VULN 的详细描述和复现步骤
- [VULN-001 换行符注入](../issues/VULN-001-sandbox-newline-bypass.md)
- [VULN-002 写白名单子串匹配](../issues/VULN-002-sandbox-write-allowlist-substring-bypass.md)
- [VULN-003 敏感文件过滤非规范](../issues/VULN-003-sandbox-sensitive-file-filter-noncanonical.md)
- [VULN-004 进程环境变量污染](../issues/VULN-004-process-env-cross-request-contamination.md)
- [VULN-005 Python 绑定锁中毒](../issues/VULN-005-python-binding-lock-poisoning-dos.md)
- [VULN-006 Schema-retry JSON fallback](../issues/VULN-006-agent-json-fallback-can-mask-invalid-structured-output.md)
