# INFO — Informational Notes

Non-blocking review notes that are useful context but are not open issues. Open
or deferred issues belong in [FINDINGS.md](./FINDINGS.md); landed fixes belong
in [FIX.md](./FIX.md).

---

## Phase 30 commit review notes — 2026-06-04

### I-30-01: Partial artifact apply reports `overlay_disposition = "committed"`
- **位置**: `crates/lancet-native/src/executor/supervisor/artifact.rs`
- **说明**: `apply_prepared_artifact_set` 遇到 partial failure 时，commit report 使用 `commit_outcome = "partial"`，同时 `overlay_disposition = "committed"`。这表示部分 artifact 已经落到真实 workspace，语义上可接受。
- **结论**: 不作为 bug 处理。若未来需要更细语义，可考虑扩展 disposition，例如 `PartiallyCommitted`，但这会改变枚举/metadata 形状，v1.4-alpha 暂不需要。

### I-30-02: Artifact handler 集成函数参数较多
- **位置**: `crates/lancet-native/src/handlers/command.rs`
- **说明**: `handle_artifact_overlay_run` 和 `run_artifact_overlay_supervised` 参数数量偏多，并已使用窄范围 `#[allow(clippy::too_many_arguments)]`。当前函数边界仍清晰：它们承载生产 command handler 到 artifact overlay pipeline 的私有编排。
- **结论**: 不作为当前架构问题处理。Phase 30 刚完成且验证覆盖充分，暂不为了形状重构触碰主路径；若后续继续扩展 artifact orchestration，可再引入 context struct 收拢参数。
