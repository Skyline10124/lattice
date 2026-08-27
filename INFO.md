# INFO — Informational Notes

Non-blocking review notes that are useful context but are not open issues. Open
or deferred issues belong in [FINDINGS.md](./FINDINGS.md); landed fixes belong
in [FIX.md](./FIX.md).

---

## Obsolete Notes

### I-30-01: Partial artifact apply reports `overlay_disposition = "committed"`
- **位置**: `crates/lancet-native/src/executor/supervisor/artifact.rs`
- **说明**: `apply_prepared_artifact_set` 遇到 partial failure 时，commit report 使用 `commit_outcome = "partial"`，同时 `overlay_disposition = "committed"`。
- **状态**: 2026-06-27 moot — `a29072e2` 移除 supervisor/overlay，引用文件已不存在。

### I-30-02: Artifact handler 集成函数参数较多
- **位置**: `crates/lancet-native/src/handlers/command.rs`
- **说明**: `handle_artifact_overlay_run` 和 `run_artifact_overlay_supervised` 参数数量偏多。
- **状态**: 2026-06-27 moot — `a29072e2` 移除 supervisor/overlay，#38/#40 移除 command handler；引用文件已不存在。
