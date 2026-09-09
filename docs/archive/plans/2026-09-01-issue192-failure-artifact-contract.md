# Issue #192 实施计划：统一 failure report，安全持久化且不覆盖原始失败

## 0. 计划元数据

- **目标 Issue**：GitHub #192《让 failure artifact 安全、唯一且保留原始失败》
- **代码基线**：`main@b60f1672dbbbff266763bd7ac1a7fb1f4aa6f3e6`
- **基线日期**：2026-09-01
- **阻塞状态**：#192 仅阻塞于 #185；#185 已完成，`CommandOutcome`、metadata-only `CommandOutcomeSummary`、输出完整性和 process-group cleanup 事实均已进入 `main`。
- **改动边界**：只修改 `lattice-system-harness`、测试文档以及 `FIX.md` / `FINDINGS.md`；不修改产品 crate、CLI、Turn、Tool、provider 或 Local Workspace 产品语义。
- **实施原则**：KISS、YAGNI、DRY。只统一 failure report 与 artifact 持久化，不建设通用日志系统、通用安全文件系统、锁服务、事务 writer、健康检查或新的命令执行模型。
- **计划文件规则**：本文件是 #192 唯一权威计划。若 worktree 中已有未跟踪的 `docs/plans/2026-09-01-issue192-failure-artifact-contract-plan.md`，将其**重命名/替换**为 `docs/plans/2026-09-01-issue192-failure-artifact-contract.md`，不得保留两份近似计划。

## 1. 最终目标

完成后，harness failure path 必须满足以下不变量：

1. **原始 assertion 永不丢失**：artifact claim、stale cleanup、目录创建、文件写入、scanner 或后续 evidence 操作失败时，primary assertion 仍出现在最终错误中。
2. **失败事实可组合**：同一次失败可以同时报告 primary assertion、全部已观察到的 secret leak 位置、全部独立 evidence errors，以及命令结果诊断。
3. **写盘和错误展示不泄漏 secret**：raw stdout/stderr 只在测试进程内用于解析和断言；错误文本和持久化 bundle 使用现有 literal redaction。
4. **fast 与 Direct 两条 artifact 路径共享同一个组合规则**：迁移期保留旧入口，但旧入口只能适配新实现，不能继续拥有第二套错误优先级或 writer 流程。
5. **稳定 artifact 路径不会被同名并发 writer 混写**：同名第二个 writer 立即 fail closed；claim 必须先于 stale cleanup。
6. **scanner 不因后续 I/O 错误丢掉先前已发现的 leak**：新 report 路径同时保留 leak locations 与 scan errors。

另外包含两个局部健壮性修复：

- scenario artifact 名称在 join 到 failure base 前必须是一个安全、非保留的 path component；这是内部防误用，不是产品攻击面。
- workspace failure snapshot 遇到 symlink 时只记录链接自身并停止，不跟随 target；只做一条局部 no-follow 修复，不扩展 symlink 产品语义。

## 2. 当前代码事实

### 2.1 当前只有一套命令结果模型

权威执行结果是：

```rust
CommandOutcome {
    stdout,
    stderr,
    stdout_complete,
    stderr_complete,
    termination,
    interventions,
    cleanup,
}
```

`CommandOutcomeSummary<'a>` 只是借用 `&CommandOutcome` 的 metadata-only 展示器，不拥有重复状态，也不是第二套 outcome。它已经展示：

- termination；
- harness interventions；
- 完整 `ProcessGroupCleanup`；
- surviving PIDs；
- stdout/stderr byte count；
- stdout/stderr completeness。

#192 必须复用 `output.summary()`，不得新增 `ArtifactCommandOutcome`、`PersistedTermination`、`CleanupFailure` 或任何第二/第三套命令与终止模型。

### 2.2 真正并存的是两条 failure-artifact 路径

#### Fast path

```text
assert_with_failure_artifacts
  -> capture_from_output
  -> FailureArtifacts
  -> write_failure_artifacts
  -> write_world_context
  -> scan_artifacts_for_secrets
```

当前缺口：

- `write_failure_artifacts`、`write_world_context` 或 scanner 的任一 `?` 会覆盖原 assertion；
- scanner 命中会返回 `SecretLeak`，同样替换 assertion；
- `FailureArtifacts` 尚未把 `CommandOutcomeSummary` 写入 `command.txt`；
- `AssertError::SecretLeak(String)` 可能直接持有实际 secret；
- 多文件 writer 在首个错误处返回，不能保留后续独立错误。

#### Direct / multi-command path

```text
ScenarioEvidence
  -> record_command / record_captures
  -> finish
  -> tests/support::finish_scenario
```

当前缺口：

- `finish_scenario` 先 panic evidence error，再处理原 assertion，因此 evidence 失败时 assertion 消失；
- `ScenarioEvidence::finish` 和内部 writers 普遍在第一条错误处返回；
- scanner 也无法同时返回先前发现的 leak 与后续 scan error。

### 2.3 已有 redaction 与 scanner 基础，不增加新依赖

当前已有：

- `redact(&str, secrets)`；
- byte-level `redact_bytes`；
- stdout/stderr/command summary 预写入 redaction；
- provider capture 各字段和 raw JSON redaction；
- workspace listing/diff redaction；
- world env secret-name mask；
- 写盘后递归 byte-level scanner；
- scanner 命中后原地替换；
- scanner I/O fail-closed 局部测试。

本票不引入 `aho-corasick`、regex、`secrecy` 或 public `Redactor` 子系统。secret 数量和 artifact 规模很小，现有 literal replacement 足够。实现者可以抽取 crate-private 小 helper，但不得新增依赖或公开通用抽象。

### 2.4 scenario-name 风险只对测试代码作者可达

当前 scenario name 来自测试字面量或版本控制 fixture；没有 CLI、LLM、Tool、provider 或产品输出流入该字段。因此这不是产品路径穿越漏洞。

真实问题是内部 helper：

```rust
failure_dir(scenario_name) = base.join(scenario_name)
```

后续代码可能执行 `remove_dir_all`。未来测试代码若误传 `".."`、`"a/b"`、绝对路径、空名称或内部保留名，可能清理错误位置或破坏 claim namespace。修复只放在唯一 artifact target 入口，不扩散到 `ScenarioWorld` 或产品层。

### 2.5 `.claims` 必须是保留名称

同名 writer claim 使用：

```text
<failure-base>/.claims/<scenario>.lock
```

若 scenario 名称 `.claims` 被当作合法 bundle 名，stale cleanup 会删除整个 claim namespace，使另一个 writer 能重建 lock file 并锁住新 inode，破坏互斥。因此 `.claims` 必须由同一个 artifact-name validator 直接拒绝。

### 2.6 当前 scanner 会丢失“部分成功”事实

当前 API：

```rust
Result<Vec<PathBuf>, ArtifactScanError>
```

一旦后续 traversal/read/rewrite 失败，之前已经发现并修补的 leak locations 不会返回给调用者。#192 的新 report 路径必须收集：

```text
leak_locations + scan_errors
```

旧公开 scanner API 保持兼容；存在错误时仍返回第一条错误，但内部新 finalizer 必须消费完整 collector。

### 2.7 workspace symlink 只涉及 failure snapshot

`WorkspaceSnapshot::capture()` 只是失败证据采集器，用路径与大小生成 before/after/diff；它不是产品 Local Workspace。当前遍历会跟随 symlink。修复只需把 symlink 当叶节点记录，不引入 `EntryKind`、marker 或额外矩阵。

## 3. 本票拥有的范围

1. 一个最小、唯一的组合失败报告模型。
2. fast bundle 写入现有 `CommandOutcomeSummary`。
3. fast 与 Direct path 都不得让 evidence error 覆盖 primary assertion。
4. secret leak finding 只保存安全位置，不保存 matched secret。
5. 最终错误展示、world context、command diagnostic 和 scanner path 均使用现有 redaction。
6. scanner 同时保留全部已观察 leaks 与后续 scan errors。
7. 各独立文件写入继续尝试，并按 pipeline 顺序保留全部错误。
8. scenario name 的单组件、非保留名防误用检查。
9. 同名 scenario writer 的 Linux nonblocking `flock` claim。
10. `WorkspaceSnapshot` 一条 no-follow symlink 修复。
11. 新入口与旧入口并存；旧入口成为 adapter，最终 hard cut 留给 #205。

## 4. 明确不属于 #192

- 不重写 `direct_secrets_*` 产品见证；真实 public-sink 刺激属于 #194。
- 不修改 fast CLI exact exit 或严格 JSON 契约；属于 #197。
- 不收缩 mutation、provider failure、observation、operator、crash、rewind 测试；属于 #193–#204。
- 不迁移所有调用点，不删除所有旧 helper；hard cut 属于 #205。
- 不修改 `CommandOutcome`、`CommandOutcomeSummary`、driver、process-group cleanup 或 timeout。
- 不给 fake LLM 增加 health、failure subscription 或统一 verdict 集成。
- 不引入通用 safe-filesystem、lock manager、事务 writer、后台服务、retry 或 backoff。
- 不引入新的 redaction 依赖或编码变体 secret 检测器。
- 不在 external 层新增 artifact/symlink/scanner 笛卡尔矩阵。
- 不把内部 artifact-name 检查描述为产品安全边界。
- 不要求 `FailureReport` 使用 `Box`；这里只有直接数据结构。只有出现实际 enum-size 证据时才考虑 boxing。

## 5. 目标设计

### 5.1 一个最小 `FailureReport`

建议在 `src/artifacts.rs` 定义唯一组合结果，避免 `assert.rs` 与 `artifacts.rs` 互相拥有一套模型：

```rust
pub struct FailureReport {
    primary_assertion: Option<String>,
    secret_leaks: Vec<String>,
    command_diagnostics: Vec<String>,
    evidence_errors: Vec<String>,
}
```

具体字段可保持私有，但必须满足：

- `primary_assertion` 保存原 assertion 的完整、已脱敏诊断文本；
- `secret_leaks` 只保存 bundle-relative 或逻辑 sink location；不保存 secret、matched bytes 或 raw payload；
- `command_diagnostics` 只保存共享 command renderer 的结果；不复制 raw stdout/stderr；
- `evidence_errors` 保存带 stage 的已脱敏错误文本；
- leak locations 排序、去重；
- command diagnostics 与 evidence errors 保持 pipeline 顺序；
- `Display` 与 `Debug` 使用同一安全输出；
- 固定展示顺序：secret leaks → primary assertion → command diagnostics → evidence errors。

leak 可以优先展示，但 primary 必须紧随其后且永不消失。

### 5.2 `AssertError` 直接包装 report，不强制 Box

保留现有 assertion variants，并把 secret variant 改为只存安全位置：

```rust
SecretLeak { location: String }
```

`assert_no_secret(output, secret)` 命中时只记录例如 `command stdout/stderr`，不得把 `secret` 参数放入 error。

旧接口的 adapter variant 使用直接值：

```rust
FailureReport(FailureReport)
```

不强制 `Box<FailureReport>`。`FailureReport` 不含递归类型；只有出现实际 enum-size 证据时再评估 boxing。

### 5.3 唯一 command diagnostic renderer

只抽取一个 crate-private helper：

```rust
fn render_command_text(
    summary: &str,
    outcome: &CommandOutcome,
    secrets: &[&str],
) -> String
```

输出形状固定为：

```text
<redacted command summary>
outcome: <redacted CommandOutcomeSummary>
```

以下三处必须消费同一个 helper：

1. fast `FailureArtifacts.command.txt`；
2. Direct `N-command.txt`；
3. `FailureReport.command_diagnostics`。

不得在三处分别 `format!`，也不得新增 `ArtifactCommandOutcome`。

### 5.4 `FailureArtifacts` 只保存持久化需要的数据

建议收敛为：

```rust
FailureArtifacts {
    dir,
    stdout,
    stderr,
    command_text,
}
```

- `stdout` / `stderr` 是经过 redaction 的持久化副本；
- `command_text` 来自唯一 renderer；
- `world.txt` 直接接收原 command summary，并在 writer 内完成最终 redaction，不复用 `command_text` 作为 world 的单行 command 字段。

### 5.5 redaction 规则

继续使用现有 literal replacement：

- raw stdout/stderr 保留在 `CommandOutcome` 中供断言与解析；
- 进入错误文本或 artifact 文件前复制并 redaction；
- `write_world_context` 保留 env secret-name mask，并在完整 `ctx` 组装完后再执行一次总 `redact`，覆盖 scenario name、project/home 路径等动态字段；
- command summary 与 outcome summary 先由唯一 renderer 组装，再统一 redaction；
- provider capture 保留现有预写入 redaction，不重建 scanner；
- scanner 内部 raw `PathBuf` 仅用于读取和原地修补；进入 report 或兼容错误前转换成 bundle-relative、已脱敏位置；
- `PostWriteSentinel` 等可显示结构不得持有未经脱敏的 `Vec<PathBuf>`；
- invalid artifact-name error 不回显原始输入；
- cleanup error message 若含 sentinel，必须经 command renderer redaction 后才能进入 bundle/report。

### 5.6 artifact target validator 的固定签名

将当前：

```rust
pub fn failure_dir(scenario_name: &str) -> PathBuf
```

改为：

```rust
pub fn failure_dir(name: &str) -> Result<PathBuf, EvidenceError>
```

为 `EvidenceError` 增加不回显输入的 variant：

```rust
InvalidArtifactName
```

validator 只接受一个**精确的** `Component::Normal`，并额外拒绝：

- 空名称；
- `.`、`..`；
- 绝对路径；
- 多组件与含 `/` 的变体；
- NUL；
- 保留名称 `.claims`。

使用一个共享 validator，供 `failure_dir` 与公开的
`ScenarioEvidence::new(scenario_name, ...)` 校验同一个安全单组件名称。公开构造器不得接受调用方提供的
`PathBuf`；需要 `TempDir` 的模块单元测试只使用 crate-private `new_at_dir` seam。不得在多处复制规则，也不建立 `SafePath` 类型。

当前仓库内固定字面量调用点可显式：

```rust
failure_dir(name).expect("valid scenario name")
```

新 finalizer 必须把 `InvalidArtifactName` 追加到 `FailureReport.evidence_errors`，而不是 panic。

### 5.7 同名 nonblocking claim

在 failure base 下使用持久 lock file：

```text
<base>/.claims/<scenario>.lock
```

由 crate-private `ArtifactClaim` 持有打开的 `File`，使用 Linux：

```text
flock(LOCK_EX | LOCK_NB)
```

规则：

1. 先 validate；
2. 创建 `<base>/.claims`；
3. 打开持久 lock file 并 nonblocking claim；
4. **取得 claim 后**才允许清理 stale bundle；
5. 同名第二个 writer 立即返回 evidence error；
6. 不等待、不 retry、不删除 lock file；
7. `File` drop 释放锁；
8. `ScenarioEvidence` 生命周期内持有 claim；
9. fast path 在 stale cleanup、写盘和 scan 期间持有 claim。

`.claims` 是保留 scenario 名称，防止 stale cleanup 删除锁命名空间。crate 已有 `libc`，不得修改 `Cargo.toml`。

claim test 必须证明：

- stale bundle cleanup 不删除 `<base>/.claims` 或当前 lock file；
- 第一个 guard 存活时第二次 claim 立即失败；
- drop 第一个 guard 后第二次 claim 成功。

### 5.8 scanner 改为私有完整 collector + 公开兼容 adapter

新增 crate-private collector，名称可调整：

```rust
struct ScanCollection {
    leak_paths: Vec<PathBuf>,
    errors: Vec<ArtifactScanError>,
}

fn scan_artifacts_collecting(...) -> ScanCollection
```

语义：

- directory entries 以稳定 path 顺序处理；
- 发现 secret 后先记录 leak path，再尝试原地 redaction；
- rewrite 失败时同时保留 leak 与 rewrite error；
- 某个 entry、subtree、read 或 rewrite 失败时记录错误并继续其他可达 siblings；
- 根目录无法读取时记录一条错误并结束该 root；
- symlink/special file 继续保持 scanner fail closed：记录错误，但不阻止扫描其他 entries；
- collector 内部可以持有 raw path；转换到 report 时必须变为 bundle-relative、已脱敏 string。

现有公开 API 保持：

```rust
scan_artifacts_for_secrets(...) -> Result<Vec<PathBuf>, ArtifactScanError>
```

它适配完整 collector：

- 有错误时返回稳定排序后的第一条**已安全化**错误；
- 无错误时返回全部 leak paths；
- 不复制第二套 scanner 实现。

新 fast/Direct finalizer 必须直接消费完整 collector，不能调用只返回首错的兼容 adapter。

### 5.9 多 evidence error 的最小收集规则

只增加一个 crate-private 小 helper，名称可调整：

```rust
fn attempt<T>(
    stage: &'static str,
    result: std::io::Result<T>,
    secrets: &[&str],
    errors: &mut Vec<String>,
) -> Option<T>
```

不建立 writer trait、transaction 或通用 error collector。

规则：

1. claim/validation 失败：不碰 bundle target，report 保留 primary 与 command diagnostic；
2. stale cleanup 失败：记录 cleanup error，停止该 bundle 的后续写盘与扫描，避免和旧 bundle 混合；
3. bundle root 创建失败：记录错误，跳过依赖 root 的后续步骤；
4. root 已存在时，各独立文件写入都继续尝试；
5. 一个文件写失败不阻止其他独立文件写入；
6. root 仍存在时，即使已有写入错误，scanner 仍执行；
7. scanner 的全部 leaks 与全部 scan errors 都追加到同一个 report；
8. `sentinel_scan.txt` 写失败只追加 evidence error；
9. partial bundle 保留供诊断；下一次同名 run 在 claim 下清理；
10. evidence errors 按 pipeline 顺序进入 report。

需要 collecting variant 的多文件 writer：

- fast stdout/stderr/command writer；
- Direct 每个 recorded command 的 command/stdout/stderr；
- workspace before/after/diff 三个文件。

零调用点的公开 raw writer 必须删除；`FailureArtifacts`、`capture_from_output` 与 collecting writer
仅作为 crate-private 统一 pipeline 存在。#205 只保留仍有调用者的兼容入口。

### 5.10 fast path 新入口

新增 report-returning 入口，名称可调整：

```rust
pub fn finalize_with_failure_artifacts(
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    output: &CommandOutcome,
    secrets: &[&str],
    result: Result<(), AssertError>,
) -> Result<(), FailureReport>;
```

行为：

- 在任何 filesystem 操作前，把 primary assertion 与 shared command diagnostic 转为已脱敏 report 内容；
- 调用 `failure_dir`；invalid name 进入 evidence errors；
- acquire claim；busy/I/O 进入 evidence errors；
- claim 后清理 stale bundle；失败则停止写盘；
- assertion 成功时只清理 stale bundle；清理失败返回 evidence-only report；
- owner test 必须预建 stale marker，执行 fast 成功 finalizer，并证明 scenario bundle 消失而
  `.claims` 命名空间仍存在；
- assertion 失败时创建 root，并逐个尝试 stdout/stderr/command/world 写入；
- 即使部分写入失败，只要 root 可读，仍运行完整 scanner collector；
- scanner leaks、scan errors、写入 errors 与 primary 一次性返回。

旧：

```rust
assert_with_failure_artifacts(...) -> Result<(), AssertError>
```

只适配新入口，并使用直接 `AssertError::FailureReport(FailureReport)`；不得保留旧写盘控制流。

### 5.11 Direct path 接入同一 report

`ScenarioEvidence` 新增 report finalizer。为避免模块循环，建议接收已格式化 primary：

```rust
pub fn finish_report(
    self,
    primary_assertion: Option<String>,
) -> Result<(), FailureReport>;
```

`tests/support::finish_scenario` 负责把：

```rust
Result<(), AssertError>
```

转换为 `error.to_string()`，再交给 `ScenarioEvidence`；`ScenarioEvidence` 使用自己持有的 sentinels 进行 redaction。

要求：

- `ScenarioEvidence::new` 只接收 `scenario_name`，通过唯一 checked failure-base builder 取得
  target；随后 acquire claim、claim 后清理 stale bundle，并在整个 recorder 生命周期持有 guard；
- 在任何 artifact I/O 前，把所有 recorded command diagnostics 用共享 renderer 加入 report；
- Direct recorded command、provider capture、workspace diff 和 scanner 的独立错误全部收集；
- `tests/support::finish_scenario` 最终只 panic 一次，文本同时包含 primary、leaks、commands 与 evidence errors；
- 当前 direct tests 不需要逐个改业务断言。

保留：

```rust
ScenarioEvidence::finish(bool) -> Result<(), EvidenceError>
```

作为迁移 adapter，并复用同一 persistence implementation；可将组合失败安全地折叠为一个 legacy `EvidenceError` 文本，但不得保留第二份 writer。

### 5.12 workspace symlink 只做一条 no-follow 修复

修改 `WorkspaceSnapshot::capture` 的私有 `walk`：

```text
DirEntry::file_type()
  real directory -> 记录并递归；真实 .git 目录继续跳过
  symlink        -> symlink_metadata；按现有 path+size 结构记录；continue
  regular file   -> 现有逻辑
```

不增加 marker、不读取 target、不增加 snapshot schema。artifact scanner 对 symlink 仍 fail closed；两者语义不要混合。

## 6. 最小 TDD 顺序

### Slice A：组合 report、SecretLeak 与唯一 command renderer

先写 RED tests：

1. `failure_report_preserves_primary_leaks_commands_and_multiple_evidence_errors`
2. `failure_report_display_is_redacted_and_stably_ordered`
3. `assert_no_secret_reports_location_without_storing_secret`
4. `capture_from_output_redacts_stdout_stderr_and_shared_command_text`

第 4 条使用 synthetic `CommandOutcome`：stdout、stderr、command summary 和 cleanup error message 都含同一 sentinel，证明唯一 renderer 与 output 持久化副本均完成 redaction。

实现：

- `FailureReport`；
- safe location-only `SecretLeak`；
- 直接 `AssertError::FailureReport(FailureReport)` adapter；
- `render_command_text`；
- fast `FailureArtifacts.command_text`。

### Slice B：safe target 与 claim-before-cleanup

先写 RED tests：

5. `artifact_name_rejects_non_component_and_reserved_inputs`
6. `artifact_claim_precedes_cleanup_and_releases_after_drop`

第 5 条 table 至少包括：

```text
""  "."  ".."  "a/b"  "a/../b"  "/tmp/x"  ".claims"
```

第 6 条必须证明 stale cleanup 不删除 `.claims` namespace，第二个 guard 在第一个存活时立即失败，drop 后可成功。

实现：

- `failure_dir -> Result<PathBuf, EvidenceError>`；
- `InvalidArtifactName`；
- 共享 validator；
- crate-private `ArtifactClaim`；
- claim-before-cleanup。

### Slice C：scanner partial results 与多 error writer

先写/强化 RED tests：

7. `scan_collection_preserves_leak_and_later_scan_error`
8. `fast_failure_report_preserves_primary_two_write_errors_and_scan_results`

第 7 条在同一 root 中放置：

- 一个含 sentinel 的普通文件；
- 一个 symlink 或其他确定性 invalid artifact entry。

断言 collector 同时返回 leak location 与 scan error，并确认普通文件已原地 redaction。不得依赖 `read_dir` 恰好先访问普通文件。

第 8 条通过 private claimed persistence seam 创建两个独立 target type conflicts，并放置一个可扫描 leak；断言：

- primary assertion 保留；
- 两条 write errors 按 pipeline 顺序出现；
- scanner 仍运行；
- leak location 与 scan error（若测试同时注入）都保留；
- command diagnostic 仍存在。

实现：

- `ScanCollection`；
- 完整 scanner traversal；
- 旧 scanner adapter 返回第一条安全错误；
- 私有 `attempt`；
- fast multi-file collecting writer。

### Slice D：Direct path 与 workspace no-follow

先写/强化 RED tests：

9. `scenario_evidence_preserves_primary_two_write_errors_and_scan_results`
10. `scenario_evidence_records_command_diagnostics_before_persistence`
11. `legacy_scenario_evidence_finish_remains_usable`
12. `workspace_snapshot_records_symlink_without_following_target`

第 9 条在 `ScenarioEvidence` 已 claim 的 target 中制造两个独立 command-file type conflicts，并保留一个可扫描 sentinel 文件；断言 primary、两条 errors、leak 和 command diagnostic 同时存在。

第 12 条只验证：snapshot 包含 symlink 自身，不包含链接目标目录中的子文件。不得增加 broken-link、loop、`.git`、retarget 或 external-binary 矩阵。

实现：

- `ScenarioEvidence::finish_report`；
- `tests/support::finish_scenario` 单次 panic；
- Direct command/workspace collecting writers；
- legacy adapter；
- snapshot no-follow。

### Slice E：最终动态字段 redaction、文档与 residual closeout

先写/强化 RED test：

13. `world_context_final_buffer_redacts_dynamic_fields`

构造 scenario/world prefix 或 command summary 含 sentinel，证明完整 `ctx` 最终总 redaction；不要只验证 env value。

更新：

- `crates/lattice-system-harness/AGENTS.md`：组合 report、claim-before-cleanup、`.claims` 保留名、snapshot no-follow；
- `docs/TESTING.md`：fast `command.txt` 含 outcome metadata，artifact errors 不覆盖 primary，scanner 可同时报告 leaks 与 errors；
- `FIX.md`：append #192 修复与验证；
- `FINDINGS.md`：仅在 fast outcome metadata 与组合失败 tests 通过后移除 `M-GH185-01`；
- 本计划落为单一 canonical 文件。

不修改 #194/#197/#205 issue body 或状态。

## 7. 确定性 failure injection 规则

允许：

- 在 private unit-test seam 中预建同名目录，使 `write(path, ...)` 稳定返回类型冲突；
- 使用 symlink 作为 scanner 的确定性 invalid entry；
- 使用 `UnixStream`/普通临时文件等本地确定性 fixture。

禁止：

- `chmod 0555`；
- 依赖 root/non-root 差异；
- 固定 sleep；
- 调度窗口；
- 修改进程全局 `LATTICE_HARNESS_FAILURE_DIR` 造成并发污染；
- 新增通用 fault-injection trait 或 filesystem transaction。

## 8. 预期修改文件

主要修改：

```text
crates/lattice-system-harness/src/artifacts.rs
crates/lattice-system-harness/src/assert.rs
crates/lattice-system-harness/src/lib.rs
crates/lattice-system-harness/tests/support/mod.rs
crates/lattice-system-harness/AGENTS.md
docs/TESTING.md
docs/plans/2026-09-01-issue192-failure-artifact-contract.md
FINDINGS.md
FIX.md
```

机械编译调整可能涉及：

```text
crates/lattice-system-harness/tests/direct_secrets.rs
```

仅用于适配 `failure_dir(...) -> Result<...>`；不得修改其业务断言或提前实施 #194。

原则上不得修改：

```text
crates/lattice-system-harness/Cargo.toml
crates/lattice-system-harness/src/driver.rs
crates/lattice-system-harness/src/fake_llm.rs
任何 product crate
任何 direct_* 业务场景（除上述签名机械调整）
```

`src/cli.rs` 只有在 adapter 文档或编译签名必须调整时才修改。

## 9. 建议提交序列

1. `docs(harness): finalize issue 192 failure artifact plan`
2. `test(harness): pin combined failure and safe command diagnostics`
3. `fix(harness): claim safe artifact targets before cleanup`
4. `fix(harness): preserve scan findings and multiple evidence errors`
5. `fix(harness): unify scenario evidence failure reporting`
6. `docs(harness): close issue 192 artifact residual`

实现提交应各自可编译。不要把 #194 产品 secret witness、#197 fast CLI 语义或 #205 hard cut 混入本 PR。

## 10. 验证命令

从当前 worktree 根目录运行：

```bash
cargo test -p lattice-system-harness --lib assert::tests -- --nocapture
cargo test -p lattice-system-harness --lib artifacts::tests -- --nocapture
cargo test -p lattice-system-harness --all-features --no-run
cargo fmt --all --check
cargo clippy -p lattice-system-harness --all-targets -- -D warnings
cargo xtask harness-boundary
cargo build -p lattice-cli --bin lattice
LATTICE_BIN="$PWD/target/debug/lattice" \
  cargo test -p lattice-system-harness

# 规范最终 gate：清除可能指向其他 checkout/worktree 的陈旧值，
# 让 xtask 注入当前 worktree 的绝对 binary 路径。
env -u LATTICE_BIN cargo xtask ci
```

不得用裸：

```bash
cargo test --workspace
```

作为最终验证，除非同一命令显式设置当前 worktree 的绝对 `LATTICE_BIN`。

验证纪律：

- focused tests 先绿，再跑 full harness；
- 不增加 `--test-threads=1`；同名 writer 冲突由 claim fail closed；
- full gate 若命中 #192 范围外既有测试问题，不在本票提高 timeout、加 retry 或修改范围外测试；先证明基线与归属；
- `cargo xtask harness-boundary` 必须继续证明 harness 不依赖产品 crate。

## 11. Acceptance mapping

| #192 条件 | 实现与测试证据 |
|---|---|
| primary、全部 leaks、evidence/cleanup errors 同时保留 | `FailureReport`；fast/Direct 多 write error + scan partial-result tests；process cleanup 复用 `CommandOutcomeSummary` |
| artifact 名称只允许安全单组件 | `failure_dir -> Result`；共享 validator；拒绝 `.claims` 与非组件输入 |
| workspace symlink 为叶节点且不递归 | 一条 outside-dir symlink snapshot test |
| 同名 scenario 立即 fail closed | persistent `.claims/<scenario>.lock` + `LOCK_NB`；claim-before-cleanup test |
| 所有持久化出口先脱敏 | shared command renderer；stdout/stderr/world/provider/workspace 复用现有 literal redaction |
| leak error 不携带真实 secret | location-only `SecretLeak`；report 只保存 redacted relative locations |
| scanner 同时保留 leaks 与 errors | private `ScanCollection`；公开 scanner 仅作首错兼容 adapter |
| 多 write/scan errors 不覆盖 assertion | private `attempt`；fast/Direct 各一条两-error 持久化测试 |
| harness 专用最小实现 | 无新依赖、无通用 FS/lock/redaction/transaction 服务、boundary PASS |
| 新旧接口并存 | report-returning APIs + legacy adapters；hard cut 留给 #205 |

## 12. 完成定义

#192 只有在以下全部成立时才完成：

- primary assertion 在 claim、cleanup、write、scan error 和 leak hit 下都仍可见；
- scanner 在“先发现 leak、后遇到错误”时同时返回两个事实；
- 多个独立 writer errors 按 pipeline 顺序保留，首错不终止其他独立写入；
- root 可读时，即使已有写入错误，scanner 仍运行；
- `FailureReport` 的 Display/Debug 不含任何注册 sentinel 原值；
- `AssertError::SecretLeak` 不拥有或展示 secret；
- fast `command.txt` 包含 termination、interventions、cleanup、survivors、byte counts 和 completeness；
- fast、Direct 与 report command diagnostic 使用同一个 renderer；
- report 中的 command diagnostic 即使 bundle 写盘失败也仍可见；
- invalid scenario name 在 join/delete 前返回 typed error，且不回显原始输入；
- `.claims` 被拒绝为 scenario 名称；
- stale cleanup 不删除 claim namespace；
- 同名第二个 writer 在第一个 guard 存活时立即失败，drop 后可成功；
- workspace snapshot 不跟随 symlink target；
- 旧 fast/Direct helper 仍编译运行，当前 direct scenarios 无需大规模迁移；
- `M-GH185-01` 在证据成立后从 `FINDINGS.md` 移除，并在 `FIX.md` append 记录；
- 最终 gate 使用 `env -u LATTICE_BIN cargo xtask ci`；
- 没有新增依赖、没有修改产品语义、没有提前实施 #194/#197/#205。

## 13. Agent 执行提示

- 开始前重新确认 `git rev-parse HEAD`；若 `main` 已前进，先重读 `artifacts.rs`、`assert.rs`、`tests/support/mod.rs`，不要套用旧行号。
- 先把本计划作为唯一 canonical 文件落入仓库；不要复制出第二份近似计划。
- RED tests 必须在旧实现上证明真实缺口；不要用当前 vacuous `direct_secrets_failure_bundle_redacts_sentinels` 作为 red evidence。
- 不要为 redaction 引入第三方库；当前工作是统一出口和错误组合。
- 不要让 report 保存 raw secret、raw stdout/stderr 或未经处理的 PathBuf。
- 不要把 artifact-name 检查扩散到 `ScenarioWorld` 或产品层。
- `.claims` 必须是 validator 的保留名，claim 必须先于 stale cleanup。
- scanner 新 finalizer 必须消费完整 collector，不能回退到首错兼容 API。
- symlink 修复只允许一条局部 no-follow test，不建立矩阵。
- 所有 I/O failure 注入必须确定性，不依赖权限、root 身份、固定 sleep 或调度窗口。
- 提交、push、建 PR、切 Ready、合并和修改 issue 状态由上层流程决定。
