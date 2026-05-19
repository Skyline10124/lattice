# FINDINGS — Open Issues

未修复项，留给 v1.2。已修复项见 [FIX.md](./FIX.md)。

---

## HIGH

### H-06: `stream_turn` 不是真正的流式 — `streaming`
- **位置**: `crates/lattice-api/src/turn/execute.rs:133-173`
- **问题**: `stream_turn_with_deps` 内部执行完整个 turn 后用 `stream::iter` replay。调用方无法获得渐进 token 推送。
- **计划**: 需将 AgentLoop 改为 channel-based 事件推送架构。
- **关联**: P2-04

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

## LOW

### L-02: `parse_json_data` 用 `Err(LlmEvent::Started)` 做 sentinel — `api-design`
- **位置**: `crates/lattice-llm/src/providers/mod.rs:14-22`
- **问题**: 空数据/`[DONE]` 返回 `Err(LlmEvent::Started)` 表示"跳过"，语义混乱。

## PARTIAL（已缓解，v1.2 完全修复）

### H-03: `write_file` 跟随符号链接绕过 project-root 限制 — `security`
- **已做**: 写入前 `symlink_metadata` 检测 symlink 并拒绝。
- **残留**: 检测到 `tokio::fs::write` 之间存在 TOCTOU 窗口。完整修复需 Unix `O_NOFOLLOW`。

### H-10: `validate_agents_dir` canonicalize 失败时回退原始路径 — `security`
- **已做**: canonicalize 失败时手动按 `Component` 规范化，`..` 越过根直接报错。
- **残留**: 手动规范化不解析路径中已存在的 symlink。完整修复需锚点回溯算法。
