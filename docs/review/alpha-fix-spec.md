# LATTICE Alpha 全量修复规格书

**目标**: 修复 active-issues.md 全部 58 个 issue，发布 v0.1.0-alpha
**截止**: 今天（2026-05-03）
**版本**: v0.1.0-alpha
**基线**: 已 commit 7 项（P0-01/P0-03/P0-07/P2-07/P2-08/DS-P0-01/token 估算）+ 6 项 unstaged 改动

---

## Issue 总量与分布

| 等级 | 总量 | 已修 | 剩余 |
|------|------|------|------|
| P0 | 7 | 3 | 4 |
| P1 | 9 | 0 | 9 |
| P2 | 12 | 2 | 10 |
| P3 | 8 | 0 | 8 |
| DS-P0 | 4 | 1 | 3 |
| DS-P1 | 8 | 0 | 8 |
| DS-P2 | 6 | 0 | 6 |
| DS-P3 | 4 | 0 | 4 |
| **总计** | **58** | **6** | **52** |

> P2-07（header 注入）、P2-08（credential 泄露）、DS-P0-01（extra_headers）在 6d5b044 中修完。

---

## 执行策略

按优先级跨组件执行，而非按组件堆叠。每 Phase 独立验证 + commit，同步更新 active-issues.md。

---

## Phase 0: Commit unstaged（5 项，预计 5 min）

当前 unstaged 改动已测试，直接落地。

| 文件 | 改动 |
|------|------|
| lattice-plugin/src/builtin/*.rs (9 files) | parse_json_from_response → PluginError 传播，含 response preview |
| lattice-cli/Cargo.toml + run.rs + main.rs | CLI 集成 PluginRegistry + ToolRegistry |
| lattice-harness/src/dag_runner.rs | PluginDagRunner 注入 DefaultToolExecutor |
| lattice-core/src/transport/anthropic.rs | extra_headers 测试清理 + tool_use 解析精简 |

**产出**: Commit `fix: plugin parse err propagation + CLI registry + DAG executor + transport cleanup`

---

## Phase 1: P0（4 项，预计 1-2 h）

最高危——用户输入直达危险操作。

| ID | 组件 | 文件 | 修复 |
|----|------|------|------|
| P0-02 | sandbox | sandbox.rs | & 加入 metacharacter 黑名单（注意 && 前匹配） |
| P0-04 | sandbox | sandbox.rs | check_url 对 HTTPS 加内网/保留地址检查（127.0.0.1/::1/10.0.0.0/8/172.16.0.0/12/192.168.0.0/16/169.254.0.0/16） |
| P0-05 | sandbox + tools | sandbox.rs:20 + tools.rs:135 | bash 执行添加 tokio::time::timeout 包装 |
| P0-06 | harness | profile.rs | agent.toml SHA-256 哈希或 Ed25519 签名验证 |

**验证**: `cargo test -p lattice-agent -p lattice-harness` 全绿
**产出**: Commit `fix: P0 sandbox SSRF + bash timeout + config integrity`

---

## Phase 2: P1 + DS-P0（12 项，预计 3-4 h）

安全缺陷 + 功能性 bug，同优先级。

### P1-01 ~ P1-04: sandbox/tools（4 项）
| ID | 文件 | 修复 |
|----|------|------|
| P1-01 | sandbox.rs | > < 加入 metacharacter 黑名单 |
| P1-02 | tools.rs | canonicalize + O_NOFOLLOW 消除 TOCTOU |
| P1-03 | tools.rs | web_search 加 30s timeout（Client::builder().timeout()） |
| P1-04 | sandbox.rs | write allowlist 子串匹配 → canonical + starts_with 目录前缀 |

### P1-05 ~ P1-06: core/transport（2 项）
| ID | 文件 | 修复 |
|----|------|------|
| P1-05 | resolve.rs | set_var → with_credentials HashMap |
| P1-06 | provider.rs | .redirect(Policy::none()) |

### P1-07 ~ P1-08: harness（2 项）
| ID | 文件 | 修复 |
|----|------|------|
| P1-07 | handoff_rule.rs | fork 分支上限 10 |
| P1-08 | profile.rs | canonicalize 后再验证路径 |

### P1-09: sandbox（1 项）
| ID | 文件 | 修复 |
|----|------|------|
| P1-09 | sandbox.rs | sensitive-file canonicalize + 路径组件匹配 |

### DS-P0（3 项）
| ID | 组件 | 修复 |
|----|------|------|
| DS-P0-02 | core/router.rs | resolve() 只返回可调用模型，缺 credential 返回 error |
| DS-P0-03 | harness | profile system_prompt/tools/executor/sandbox 装配到 Agent |
| DS-P0-04 | harness/runner.rs | LoopEvent::Error 即返回 Err |

**验证**: `cargo test --workspace` 全绿 + active-issues.md 状态更新
**产出**: Commit `fix: P1 security defects + DS-P0 functional bugs`

---

## Phase 3: P2-P3（18 项，预计 2-3 h）

中低优安全/质量漏洞。

### P2-01 ~ P2-05: sandbox（5 项）
| ID | 修复 |
|----|------|
| P2-01 | 扩展敏感文件列表 |
| P2-02 | check_url 解析后二次 DNS 检查（基本防御：禁止已知 rebinding 域名） |
| P2-03 | web_search redirect 同样 Policy::none() |
| P2-04 | ANSI-C 引用转义检测 |
| P2-05 | PATH 注入 + 子进程参数验证 |

### P2-06 ~ P2-12（剩余 5 项）
| ID | 组件 | 修复 |
|----|------|------|
| P2-06 | core/router.rs | url::Url 解析 host，拒绝含 userinfo 的内网 URL |
| P2-09 | harness/ws.rs | WS 加 Origin/Token 鉴权 |
| P2-10 | harness + cli | LATTICE_AGENTS_DIR 允许列表 + 路径验证 |
| P2-11 | python | lock poisoning 修复（异常安全释放） |
| P2-12 | harness | schema retry 错误明确化 |

### P3-01 ~ P3-08（8 项）
| ID | 组件 | 修复 |
|----|------|------|
| P3-01 | core/router.rs | 拒绝非 http/https 协议 |
| P3-02 | core/provider.rs | 添加全局请求超时 |
| P3-03 | core/lib.rs | chat_endpoint 覆盖限制 |
| P3-04 | core/gemini.rs | model_id 长度检查 |
| P3-05 | harness/sqlite.rs | FTS5 参数化查询 |
| P3-06 | cli/credentials.rs | diagnostics() 不枚举具体 key |
| P3-07 | harness/pipeline.rs | 内存条目大小/数量限制 |
| P3-08 | harness/handoff_rule.rs | 深度嵌套 JSON 递归深度限制 |

**验证**: `cargo test --workspace` + active-issues.md 更新
**产出**: Commit `fix: P2-P3 security and quality issues`

---

## Phase 4: DS-P1/P2（14 项，预计 2-3 h）

工程优化。

| ID | 组件 | 修复 |
|----|------|------|
| DS-P1-01 | core/catalog | 填充模型 context_length |
| DS-P1-02 | core | retry 接入 core 或文档明确 |
| DS-P1-03 | harness/pipeline | fork 取消/超时/冲突策略 |
| DS-P1-04 | plugin/erased_runner | PluginRunner 完整执行 Plugin trait |
| DS-P1-05 | agent + harness + python | runtime 模型统一 |
| DS-P1-06 | bus | pub/sub 权限检查 + bounded queue |
| DS-P1-07 | agent + harness/memory | Memory API 统一 |
| DS-P1-08 | cli | credential/tool/session 行为统一 |
| DS-P2-01 | core | Gemini 策略明确 |
| DS-P2-02 | core/streaming | SSE residual buffer flush |
| DS-P2-03 | python | streaming API 暴露 |
| DS-P2-04 | tui + cli | TUI 入口 + 上下文传递 |
| DS-P2-05 | docs | README/STATUS/ROADMAP 同步 |
| DS-P2-06 | core | ErrorClassifier 消费 Retry-After header |

**验证**: `cargo test --workspace` + active-issues.md 更新
**产出**: Commit `fix: DS-P1/P2 engineering improvements`

---

## Phase 5: DS-P3 + 文档迁移（5 项 + 迁移，预计 1 h）

| ID | 组件 | 修复 |
|----|------|------|
| DS-P3-01 | plugin | retry correction hint |
| DS-P3-02 | tui | 渲染/滚动/终端恢复 |
| DS-P3-03 | core | compat transport 策略 |
| DS-P3-04 | workspace | 清理 warnings |
| — | docs | active-issues.md → resolved-issues.md 迁移 |

**产出**: Commit `chore: DS-P3 cleanup + issue tracker migration`

---

## 总工作量估算

| Phase | 项数 | 时间 | 产出 |
|-------|------|------|------|
| Phase 0 | 5 | 5 min | commit unstaged |
| Phase 1 | 4 | 1-2 h | commit P0 |
| Phase 2 | 12 | 3-4 h | commit P1+DS-P0 |
| Phase 3 | 18 | 2-3 h | commit P2-P3 |
| Phase 4 | 14 | 2-3 h | commit DS-P1/P2 |
| Phase 5 | 5 | 1 h | commit DS-P3 + migration |
| **总计** | **58** | **9-13 h** | **6 commits** |

> 每 Phase 完成后立即更新 active-issues.md 状态，避免最终一次性 diff overflow。
> 每 Phase 独立 `cargo test --workspace` 验证，有问题原地修复不跨 Phase。
