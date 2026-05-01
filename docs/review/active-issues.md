# LATTICE 活跃问题追踪

**最后更新**: 2026-05-02
**审核来源**: 蓝军自攻击全代码库安全审计（lattice-core, lattice-agent, lattice-harness, lattice-cli）
**状态**: 36 ISSUES — 7 CRITICAL, 9 HIGH, 12 MEDIUM, 8 LOW

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

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
| **状态** | Open |

**描述**: 敏感文件过滤使用 path.contains(".env") 子串匹配，不解析路径。可能误挡合法文件，也可能被编码或 symlink 绕过。

**根因**: 路径未 canonicalize，匹配逻辑使用子串而非路径组件匹配。

**建议修复**: canonicalize 后做路径组件匹配而非子串匹配。

---

## P2 — MEDIUM 安全/设计问题（12 项）

### P2-01: 敏感文件列表严重不完整 [VULN-015]
**组件**: lattice-agent/src/sandbox.rs | **文件**: `sandbox.rs:36-43` | **状态**: Open

**描述**: 仅 6 项。缺少 .git/config、~/.ssh/id_*、~/.aws/credentials、.npmrc 等。

### P2-02: DNS rebinding 绕过 HTTPS SSRF [VULN-016]
**组件**: lattice-agent/src/sandbox.rs | **文件**: `sandbox.rs:162` | **状态**: Open

**描述**: DNS rebinding 服务（nip.io, xip.io）可解析域名到内网 IP，绕过 host 检查。

### P2-03: reqwest 默认重定向跟踪 SSRF [VULN-017]
**组件**: lattice-agent/src/tools.rs | **文件**: `tools.rs:195` | **状态**: Open

**描述**: 302 重定向可绕过 URL scheme 检查，访问内网 HTTP 服务。

### P2-04: Bash ANSI-C 引用绕过 [VULN-018]
**组件**: lattice-agent/src/sandbox.rs | **文件**: `sandbox.rs:119` | **状态**: Open

**描述**: $'\\n' 展开为换行符绕过黑名单。$'\\x3b' → ;。

### P2-05: 多向量组合绕过 [VULN-019]
**组件**: lattice-agent/src/sandbox.rs | **文件**: `sandbox.rs:132-133` | **状态**: Open

**描述**: PATH 注入 + 子进程继承 + check_command 参数不验证。

### P2-06: URL 验证 userinfo 注入绕过 [VULN-021]
**组件**: lattice-core/src/router.rs | **文件**: `router.rs:592-624` | **状态**: Open

**描述**: https://anything@169.254.169.254/ 中 anything 被视为用户名而非 host。

### P2-07: HTTP header 注入 via header: 前缀 [VULN-022]
**组件**: lattice-core/src/lib.rs + transport/gemini.rs | **状态**: Open

**描述**: provider_specific 的 header: 机制可注入任意 HTTP 头，无白名单。

### P2-08: 凭证通过错误响应体泄露 [VULN-023]
**组件**: lattice-core/src/errors.rs + lib.rs | **状态**: Open

**描述**: 非 2xx 响应体存入错误消息，可能含 API key 回显。

### P2-09: WebSocket 端点无鉴权 [VULN-031]
**组件**: lattice-harness/src/ws.rs | **文件**: `ws.rs:22-59` | **状态**: Open

**描述**: /ws 端点无 Origin/Token/CORS。跨域可监听 pipeline 事件泄露。

### P2-10: LATTICE_AGENTS_DIR 环境变量毒化 [VULN-033]
**组件**: lattice-harness + lattice-cli | **状态**: Open

**描述**: 环境变量可覆盖 agent 目录到攻击者控制的路径。

### P2-11: Python binding lock poisoning [VULN-005]
**组件**: lattice-python | **状态**: Open

**描述**: Python GIL/lock 交互在异常路径上可能导致宿主进程 panic。

### P2-12: schema-retry JSON fallback [VULN-006]
**组件**: lattice-harness | **状态**: Open

**描述**: schema 校验失败时的 JSON fallback 行为可能隐藏畸形 agent 输出。

---

## P3 — LOW 代码质量问题（8 项）

### P3-01: URL 验证接受非 HTTP 协议 [VULN-024]
**组件**: lattice-core/src/router.rs | **状态**: Open

### P3-02: 缺少全局请求超时 [VULN-025]
**组件**: lattice-core/src/provider.rs | **状态**: Open

### P3-03: chat_endpoint 覆盖可重定向 [VULN-026]
**组件**: lattice-core/src/lib.rs | **状态**: Open

### P3-04: Gemini URL model ID 无长度限制 [VULN-027]
**组件**: lattice-core/src/transport/gemini.rs | **状态**: Open

### P3-05: SQLite FTS5 查询构造脆弱 [VULN-030]
**组件**: lattice-harness/src/memory/sqlite.rs | **状态**: Open

### P3-06: diagnostics() 凭证枚举 [VULN-034]
**组件**: lattice-cli/src/credentials.rs | **状态**: Open

### P3-07: 内存条目无大小限制 [VULN-035]
**组件**: lattice-harness/src/pipeline.rs | **状态**: Open

### P3-08: 深度嵌套 JSON 栈溢出风险 [VULN-036]
**组件**: lattice-harness/src/handoff_rule.rs | **状态**: Open

---

## 交叉引用

- [安全审计问题索引](../issues/README.md) — 所有 36 个 VULN 的详细描述和复现步骤
- [VULN-001 换行符注入](../issues/VULN-001-sandbox-newline-bypass.md)
- [VULN-002 写白名单子串匹配](../issues/VULN-002-sandbox-write-allowlist-substring-bypass.md)
- [VULN-003 敏感文件过滤非规范](../issues/VULN-003-sandbox-sensitive-file-filter-noncanonical.md)
- [VULN-004 进程环境变量污染](../issues/VULN-004-process-env-cross-request-contamination.md)
- [VULN-005 Python 绑定锁中毒](../issues/VULN-005-python-binding-lock-poisoning-dos.md)
- [VULN-006 Schema-retry JSON fallback](../issues/VULN-006-agent-json-fallback-can-mask-invalid-structured-output.md)
