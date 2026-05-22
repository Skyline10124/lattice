# FINDINGS — Open Issues

未修复项，留给 v1.2。已修复项见 [FIX.md](./FIX.md)。

---

## HIGH

(None)

## MEDIUM

### M-04: DNS rebinding TOCTOU（lattice-llm） — `security`
- **位置**: `crates/lattice-llm/src/security.rs:100-117`
- **问题**: 验证时 DNS 解析一次检查非内网 IP，reqwest 发请求时再次解析。攻击者可在两次解析间切换到内网 IP。
- **计划**: 需 reqwest `resolve()` 钉住 IP。

### M-05: DNS rebinding TOCTOU（lattice-tools）+ 不完整黑名单 — `security`
- **位置**: `crates/lattice-tools/src/executor/web.rs:12-15, 60-88`
- **问题**: 同 M-04。rebinding 域名黑名单只有 `nip.io`/`xip.io`，缺少多个已知域名。

### M-11: `head_transcript` 只走 first-parent — `correctness`
- **位置**: `crates/lattice-session/src/sqlite_graph.rs:219`
- **问题**: 合并后的分支消息永远不会出现在 transcript 重建结果中。
- **计划**: 需拓扑排序遍历所有可达 commit。

## LOW

(None)

## OBSOLETE / CLOSED

### L-18-03: 无 stream 超时/deadline 机制 — `robustness`
- **原位置**: `crates/lattice-cli/src/commands/run.rs:439`
- **原问题**: `while let Some(event) = stream.next().await` 无超时，hung provider 会阻塞 CLI。
- **当前状态**: CLI 加 5 分钟 idle timeout，超时写 stderr + exit 1。API/Agent durable deadline 留未来。

### H-06: `stream_turn` 不是真正的流式 — `streaming`
- **原位置**: `crates/lattice-api/src/turn/execute.rs`
- **原问题**: production `stream_turn` 曾经执行完整个 turn 后 replay buffered events，调用方无法获得渐进 token 推送。
- **当前状态**: Phase 16 已将 production `stream_turn` 改为 spawn + bounded channel 的 live stream；Phase 17 补充了 failure/consistency 覆盖。`stream_turn_with_deps` 仍是 buffered replay，但现在是测试 seam，不是 production public live path。
- **后续**: CLI 仍然调用 `run_turn`，改为消费 public `TurnEvent` 属于 Phase 18，不再作为 H-06 open finding 跟踪。

## PARTIAL（已缓解，v1.2 完全修复）

### H-03: `write_file` 跟随符号链接绕过 project-root 限制 — `security`
- **已做**: 写入前 `symlink_metadata` 检测 symlink 并拒绝。
- **残留**: 检测到 `tokio::fs::write` 之间存在 TOCTOU 窗口。完整修复需 Unix `O_NOFOLLOW`。

### H-10: `validate_agents_dir` canonicalize 失败时回退原始路径 — `security`
- **已做**: canonicalize 失败时手动按 `Component` 规范化，`..` 越过根直接报错。
- **残留**: 手动规范化不解析路径中已存在的 symlink。完整修复需锚点回溯算法。
