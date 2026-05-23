# FINDINGS — Open Issues

未修复项，留给后续 alpha 主线。已修复项见 [FIX.md](./FIX.md)。

---

## HIGH

(None)

---

## MEDIUM

### M-11: `head_transcript` 只走 first-parent — `correctness`
- **位置**: `crates/lattice-session/src/sqlite_graph/commit.rs`（`head_transcript_impl` ancestry 遍历）
- **问题**: 合并后的分支消息永远不会出现在 transcript 重建结果中。
- **排期**: 若 merge / 多 parent commit 已是用户可见能力，优先于 M-21；若 merge 仍非常用路径，可放在 v1.3-alpha 中段。
- **计划**: 拓扑排序遍历所有可达 commit；测试须覆盖 diamond merge，避免重复消息或顺序不稳定。

### M-21: `AgentLoop` 每轮全量 `convert_to_llm_message` — `performance`
- **位置**: `crates/lattice-agent/src/agent_loop/mod.rs`（`run_turn_inner` 主循环）
- **问题**: 多轮 tool loop（最高约 200 轮）中，每轮对当前全部 `messages` 做 `iter().map(convert_to_llm_message).collect()`。随对话变长，总 work 约为 **O(轮次 × 消息数)**，长会话下 CPU/分配开销显著。
- **排期**: 性能债，**非** v1.3-alpha 第一刀；controlled editing / 更密集 tool loop 变重要后再做。
- **计划**: 增量维护 `Vec<lattice_llm::Message>`，每轮只 append 新 assistant/tool 消息；须厘清 system / head transcript / current turn delta 边界，避免重复喂给模型。

## LOW

### L-20: `SqliteSessionGraphStore` 单连接 `Mutex` 应用层串行化 — `performance` / `architecture`
- **位置**: `crates/lattice-session/src/sqlite_graph/mod.rs`
- **问题**: 虽启用 WAL，但全进程共享一个 `Connection` + `Mutex`，读写均独占；多会话/多任务并行时 DB 成为串行瓶颈。
- **当前状态**: 本地 CLI 场景可接受；非 bug。
- **排期**: 除非很快做并发 TUI、多 session daemon 或 HTTP server，否则属 v0-beta 前架构评估项，非 v1.3-alpha 开场项。
- **计划**: 连接池或每任务独立 `Connection`（仍每连接 WAL）。

### L-21: 工具执行 `await_with_deadline` 未与 `cancel_flag` 竞争 — `robustness`
- **位置**: `crates/lattice-agent/src/agent_loop/mod.rs`（`execute` 调用处）
- **问题**: 用户取消后，须等当前工具跑完或触及 turn `deadline` 才能在 checkpoint #5 退出；长时 `bash` / `web_fetch` 取消滞后。
- **当前状态**: Plan 15-02 **D-07** 明确不 abort in-flight 工具。
- **升级条件**: 若 v1.3-alpha 引入 verification runner / long-running command workflow，**升为 Medium**。
- **计划**: `select!` cancel + 工具 future；对可中断工具传 cancel 或子进程 kill（按工具类型分级）。

## OBSOLETE / CLOSED

### L-18-03: 无 stream 超时/deadline 机制 — `robustness`
- **原位置**: `crates/lattice-cli/src/commands/run.rs:439`
- **原问题**: `while let Some(event) = stream.next().await` 无超时，hung provider 会阻塞 CLI。
- **当前状态**: CLI 加 5 分钟 idle timeout，超时写 stderr + exit 1。API/Agent durable deadline 留未来。

## PARTIAL

### M-04: `validate_base_url` DNS 检查与 HTTP 请求之间存在 TOCTOU — `security`
- **已做**: 请求前 `resolve_public_socket_addrs`，`ClientBuilder::resolve_to_addrs` 钉住解析结果（`lattice-llm/src/client.rs`）。
- **残留**: `validate_base_url` 阶段的 DNS 仍为 best-effort（离线 CI 可能不解析）；权威防护在 request-time pinning，配置时校验不能单独当作闭环。

### M-05: `web_fetch` DNS rebinding / 黑名单 — `security`
- **已做**: 与 M-04 共享 request-time 钉 IP；已知 rebinding 后缀硬拒绝（`DNS_REBINDING_DOMAIN_SUFFIXES`，`security.rs` + `web.rs`）。
- **残留**: 黑名单不可穷尽（新 wildcard→IP 服务、非列表域名）；需持续维护或改为启发式（主机名编码 IP 模式）+ 仍依赖钉 IP 作为主防线。

### H-03: `write_file` 跟随符号链接绕过 project-root 限制 — `security`
- **已做**: 写入前 `symlink_metadata` 检测 symlink 并拒绝。
- **残留**: 检测到 `tokio::fs::write` 之间存在 TOCTOU 窗口。完整修复需 Unix `O_NOFOLLOW`。

### H-10: `validate_agents_dir` canonicalize 失败时回退原始路径 — `security`
- **已做**: canonicalize 失败时手动按 `Component` 规范化，`..` 越过根直接报错。
- **残留**: 手动规范化不解析路径中已存在的 symlink。完整修复需锚点回溯算法。
