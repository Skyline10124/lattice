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

### M-12: `hash_json` 依赖 serde_json 序列化顺序 — `compatibility`
- **位置**: `crates/lattice-session/src/graph.rs:329-336`
- **问题**: commit ID 由序列化结果哈希得到，若 `preserve_order` feature 变化所有已有 ID 失效。
- **计划**: 引入 canonical JSON 格式。

### M-13: profile `canonicalize` 失败时回退非规范路径 — `security`
- **位置**: `crates/lattice-profile/src/load.rs:148-149`
- **问题**: 失败后 `starts_with` 比较 canonical vs non-canonical 路径，可能绕过限制。

### M-15: `map_session_error` 丢弃结构化错误信息 — `observability`
- **位置**: `crates/lattice-api/src/session.rs:131-136`
- **问题**: 所有非 NotFound 的 SessionError 扁平化为 `ApiError::Internal(string)`。

### M-18-01: release-gate.sh 缺三个 workspace crate — `quality`
- **位置**: `scripts/release-gate.sh:13-23`
- **问题**: 只测试 lattice-llm/agent/api/cli，缺 lattice-session/tools/profile。任一有 bug 会漏过 release gate。
- **计划**: post-alpha 改为 `cargo test --workspace`。

### M-18-02: `status_to_exit_code` 死代码 — `quality`
- **位置**: `crates/lattice-cli/src/commands/run.rs:612-619`
- **问题**: `#[allow(dead_code)]` 仅为三个遗留测试保留，新 `exit_code_from_state` 已完整覆盖 D-A4 矩阵。
- **计划**: 移除 `status_to_exit_code` 及其三个测试。

## LOW

### L-02: `parse_json_data` 用 `Err(LlmEvent::Started)` 做 sentinel — `api-design`
- **位置**: `crates/lattice-llm/src/providers/mod.rs:14-22`
- **问题**: 空数据/`[DONE]` 返回 `Err(LlmEvent::Started)` 表示"跳过"，语义混乱。

### L-18-01: `handle_profile_error` 未用 `_source` 参数 — `quality`
- **位置**: `crates/lattice-cli/src/commands/run.rs:489-493`
- **问题**: `_source: &ProfileSource` 从未使用，下划线前缀确认此问题。

### L-18-02: TurnEvent 序列化 `.expect()` 不可恢复 panic — `robustness`
- **位置**: `crates/lattice-cli/src/commands/run.rs:180`
- **问题**: `serde_json::to_string(event).expect(...)` — 类型系统保证所有字段可序列化，但 panic 无优雅降级。

### L-18-03: 无 stream 超时/deadline 机制 — `robustness`
- **位置**: `crates/lattice-cli/src/commands/run.rs:439`
- **问题**: `while let Some(event) = stream.next().await` 无超时。channel 容量 256 提供背压，但 hung provider 会阻塞 CLI。
- **计划**: post-alpha 加 deadline。

## OBSOLETE / CLOSED

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
