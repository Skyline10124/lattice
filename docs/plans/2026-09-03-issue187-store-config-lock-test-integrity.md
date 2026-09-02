# Issue #187 实施计划：修正 store config 与 lock 测试的空转断言

> **修订标识：R2（2026-09-03）**：已落实本地 agent 的 3 个实质修正与 2 个记录修正；本标识用于区分此前错误交付的旧附件快照。

## 0. 计划元数据

- **目标 Issue**：GitHub #187《修正 store、config 与 lock 测试的空转断言》
- **代码基线**：`main@34dcdc16cfb2d4e614dda04f514e77b38a26ccea`
- **基线日期**：2026-09-03
- **阻塞状态**：无；#187 当前为 `ready-for-agent`，可立即实施。
- **改动边界**：只修正 no-remote/config 测试的 owner 与 false-green，以及 create/repository lock 测试的陈述范围；不修改产品持久化格式、Session ref 拓扑、锁实现或运行时行为。
- **实施原则**：replacement first、KISS、YAGNI。不得为一条测试新增通用 Git-config parser、双持锁契约、线程时序矩阵或产品抽象。
- **计划文件路径**：`docs/plans/2026-09-03-issue187-store-config-lock-test-integrity.md`
- **与后续架构探索的关系**：即使后续评估替换全 Git control plane，本票仍应先完成；它修复当前基线的测试诚信，使存储 MVP 的现状对比不建立在 false-green 上。
- **复核状态**：已吸收 2026-09-03 本地 agent 的 NEEDS_MINOR_REVISION 结论：修正 lock replacement-first 顺序、补齐被删 head 断言的 exact owner、收窄 remote/refspec 契约，并补齐 FIX.md severity 与 focused 验证命名。

## 1. 最终目标

完成后必须同时满足：

1. 创建真实 bare Session store 后，无条件读取其真实存在的 `sessions.git/config`。
2. 构造器未配置任何 remote；因此也不存在由 remote section 配置的 `refs/lattice/**` push/fetch refspec。
3. 测试不对整个 config 施加“任何位置均不得出现 `refs/lattice` 文本”的新禁令；正式契约只约束 remote/refspec 行为。
4. no-remote/config 契约由 `lattice-session` owner 层唯一持有，`lattice-api` 不再拥有该 Git-store 内部测试。
5. 删除旧 API 文件前，旧文件中的 Session head 断言已经有一个 exact、已通过且更强的 owner replacement。
6. create/repository lock 测试只陈述并断言它实际证明的事实：
   - `StoreRepo::create*` 返回时 create lock 已释放；
   - repository write lock 使用仓库内另一条锁文件路径。
7. 不新增“同时持有两把锁必须互不阻塞”的契约。当前设计要求 first-create lock 与 repository write lock 不重叠，本票不扩张它。
8. 新 lock identity 必须在旧 identity 尚未删除时单独通过；最终提交只保留新测试。
9. `FIX.md` 显式记录 issue id、severity、problem、fix、verification 和 changed files；只有真实残余风险才进入 `FINDINGS.md`。

## 2. 当前代码事实

### 2.1 no-remote 测试的关键 config 断言没有执行

当前测试 identity：

```text
crates/lattice-api/tests/git_store_no_remote_refspec.rs
└── git_store_no_remote_push_refspec_for_lattice_refs
```

测试创建的是 bare repository：

```rust
GitSessionGraphStore::create(dir.join("sessions.git"))
```

真实配置文件位于：

```text
<temp>/sessions.git/config
```

当前测试却读取：

```text
<temp>/.git/config
```

并使用：

```rust
if config_path.is_file() {
    // config 断言
}
```

错误路径不存在时，测试仍然成功；config 断言实际零执行。

### 2.2 当前测试混入了不属于 config 契约的 Session 图构造

旧测试还执行了：

- 创建 Session；
- 通过 test-only direct commit helper 提交一个 Turn；
- 断言 `refs/lattice/sessions/<id>/heads/main` 指向该 commit；
- 枚举 `refs/remotes/**`，确认没有 remote-tracking ref 指向该 commit。

这些步骤不是证明“构造器没有配置 remote”的必要条件，并引入 Turn、manifest、Session refs、test-utils 与 gix object traversal 的额外耦合。

### 2.3 no-remote/config 契约属于 `lattice-session`

`GitSessionGraphStore`、Session store 路径、store identity，以及“不为 `refs/lattice/**` 配置 remote push refspec”的规则都由 `lattice-session` 持有。自然 owner 是：

```text
crates/lattice-session/src/git_graph/tests/state_and_locks.rs
```

该模块已经集中覆盖 Session store 的 create/open、identity、strict-open 和 locking，不需要再建测试模块。

GitHub #188 已明确 no-remote/refspec 不属于其范围，因此 #187 是该契约的唯一 frontier owner。

### 2.4 旧 head 断言已有更强 owner

旧 API 测试中的：

```text
Session head ref 指向刚提交的 Turn
```

已有明确 owner replacement：

```text
git_graph::tests::pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically
```

该测试走生产 finalization，并同时验证 head、永久 Turn pin、Session state 与 active-turn 删除的一次原子 publication。它严格强于旧测试只检查 direct test commit 的 head ref。

### 2.5 lock 测试的注释超过了断言

当前测试 identity：

```text
crates/lattice-store/tests/integration.rs
└── create_lock_and_repo_lock_are_distinct_files
```

当前执行顺序：

```text
create store
→ 获取 repository write lock
→ 检查 repository lock 路径
→ drop repository write lock
→ non-blocking 获取 create lock
```

它实际证明：

- create lock 文件位于仓库父目录；
- repository write lock 位于 Git 目录中，文件名为 `lattice-repo.lock`；
- create 返回后 create lock 已释放。

它没有在持有一种锁时获取另一种锁，因此没有证明注释中的：

```text
holding one never blocks the other
```

正确修复是收窄名称与注释，而不是补一条没有生产消费者的双持锁契约。

## 3. 迁移前 exact identity 与 assertion inventory

| 当前 identity | 当前断言 | 处理 | 唯一 replacement / owner | assertion-superset 说明 |
| --- | --- | --- | --- | --- |
| `git_store_no_remote_push_refspec_for_lattice_refs` | Session head ref 指向 committed Turn | 从旧 API 文件删除，但不丢失覆盖 | `git_graph::tests::pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically` / `lattice-session` | 生产 finalization 同时断言 head、Turn pin、state 与 `active-turn` 删除原子发布，严格强于旧 direct-commit head 断言 |
| 同上 | `repo.remote_names()` 为空 | 保留并迁移 | `git_graph::tests::state_and_locks::created_session_store_config_is_readable_and_has_no_remote` / `lattice-session` | 同一 semantic assertion，迁到构造器 owner；不再依赖 Session/Turn fixture |
| 同上 | 正确 bare config 必须存在且可读 | 修正路径后保留并增强 | 同上 | 旧测试可能零执行；replacement 无条件读取 `store.dir()/config`，因此严格增强 |
| 同上 | 整个 config 不含任意 `refs/lattice` 文本 | 不作为正式契约保留 | 无；改由 semantic no-remote 断言持有 remote/refspec 契约 | 旧断言此前没有执行；直接启用会暗中扩张契约，不能把全文件文本禁令伪装成 refspec 检查 |
| 同上 | 无 `refs/remotes/**` 指向该 direct test commit | 按非契约构造删除 | 无 replacement | 构造器无 remote 已由 owner test 证明；额外创建 Turn 并按 OID 扫描 remote-tracking refs 不增加信号 |
| `create_lock_and_repo_lock_are_distinct_files` | 两条 lock path 位置不同 | 保留 | 新 `create_returns_after_releasing_create_lock_and_repo_lock_is_distinct` / `lattice-store` | 新测试增加直接 `assert_ne!`，并保留两条位置断言 |
| 同上 | create 返回后 create lock 可 non-blocking 获取 | 保留 | 同上 | 同一行为断言与获取顺序；名称准确表达 release-after-create |
| 同上注释 | 同时持有一种锁不阻塞另一种 | 删除 overclaim | 无 replacement | 旧测试从未证明，生产契约也不要求；不建立新并发行为 |

“无 replacement”只用于明确退役的非契约断言，必须在 PR 中如实记录，不能写成“已有其他测试大概覆盖”。

## 4. 设计决定

### 4.1 Session config replacement identity

新增：

```text
crates/lattice-session/src/git_graph/tests/state_and_locks.rs
└── created_session_store_config_is_readable_and_has_no_remote
```

最小测试形状：

```rust
let dir = temp_dir("no_remote_config");
let store = GitSessionGraphStore::create(dir.path().join("sessions.git"))
    .expect("create Session store");

let config_path = store.dir().join("config");
let _config = std::fs::read_to_string(&config_path)
    .expect("created bare Session store must have a readable config");

let repo = gix::open_opts(store.dir(), gix::open::Options::isolated())
    .expect("open created Session store");
let remotes: Vec<_> = repo.remote_names().into_iter().collect();

assert!(remotes.is_empty(), "Session store must not configure remotes");
```

必须保留：

1. 路径来自 `store.dir().join("config")`，不手写 `.git/config`。
2. `read_to_string(...).expect(...)` 无条件执行；config 缺失必须失败。
3. 通过 Git 配置语义层的 `remote_names().is_empty()` 证明没有 remote。

不得保留：

```rust
config.contains("refs/lattice")
```

这种全文件文本扫描禁止了正式契约之外的内容。无 remote 已经排除了 remote-owned push/fetch refspec。若实施者仍希望添加独立的 push-refspec 断言，只能使用 gix 配置解析接口检查 `remote.*.push` / `remote.*.fetch` 的解析值，不得扫描整个 config，也不得新增通用 parser。

不做完整 config golden snapshot；gix 将来增加无害 `[core]` 配置时不应导致测试失败。

### 4.2 删除旧 API 文件的前置证据

删除以下文件前：

```text
crates/lattice-api/tests/git_store_no_remote_refspec.rs
```

必须先分别确认：

```text
git_graph::tests::state_and_locks::created_session_store_config_is_readable_and_has_no_remote
git_graph::tests::pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically
```

都显示 `1 passed`。

第一条持有 config/no-remote 契约；第二条持有旧文件中的 head-ref 断言。remote-tracking OID 扫描作为非契约构造退役，不寻找伪 replacement。

### 4.3 lock replacement identity 与范围

在旧测试仍存在时新增：

```text
create_returns_after_releasing_create_lock_and_repo_lock_is_distinct
```

推荐断言：

```rust
let create_lock = layout.lattice_root().join(".sessions.git.create.lock");
assert!(create_lock.exists());

let write = repo.write_lock().expect("repository write lock");
assert!(write.path().starts_with(repo.git_dir()));
assert!(write.path().ends_with("lattice-repo.lock"));
assert_ne!(write.path(), create_lock.as_path());
drop(write);

let create = lattice_store::lock::acquire_flock_strict(
    &create_lock,
    lattice_store::Blocking::NonBlocking,
)
.expect("create lock must be free after create returns");
drop(create);
```

推荐注释：

```text
The first-create lock file remains in the parent directory but is released
before create returns. Steady-state repository writes use a distinct lock
inside the repository.
```

不得声明：

```text
holding one never blocks the other
both locks may be held concurrently
```

临时并存只用于验证 replacement-first；最终提交删除旧 identity，不保留重复测试。

### 4.4 默认不修改生产实现

本票预计只修改测试和记录。出现以下任一情况才升级为产品 bug：

- 正确路径的 bare config 不存在或不可读；
- semantic 配置读取发现构造器确实创建了 remote；
- `StoreRepo::create*` 返回时 create lock 仍被持有；
- repository write lock 与 create lock 实际解析为同一路径。

遇到真实产品缺陷时不得弱化 replacement 或恢复条件跳过；停止删除旧测试，先修产品问题。只有这种情况才记录真实 RED→GREEN 证据。默认 replacement 预期直接通过，不得虚构 RED 阶段。

## 5. 实施步骤

### 5.1 先记录迁移证据表

在实施 commit 或 PR 描述中先写入第 3 节的 exact identity、唯一 replacement、assertion-superset 和明确退役项。不要只列文件名。

### 5.2 新增 Session config owner replacement

修改：

```text
crates/lattice-session/src/git_graph/tests/state_and_locks.rs
```

要求：

- 使用该模块已有 `temp_dir`；
- 只创建 store，不创建 Session，不提交 Turn；
- 无条件读取正确 config；
- 不遍历 Session refs、commit objects 或 `refs/remotes/**`；
- 不抽取只被这一条测试使用的 helper。

### 5.3 在删除 API 文件前运行两条 Session owner evidence

```bash
cargo test -p lattice-session \
  git_graph::tests::state_and_locks::created_session_store_config_is_readable_and_has_no_remote \
  -- --exact

cargo test -p lattice-session \
  git_graph::tests::pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically \
  -- --exact
```

两条命令都必须确认输出显示 `1 passed`，不能只看 exit code；错误 filter 零命中也可能返回成功。实施记录分别保存两条 exact identity 的结果。

### 5.4 删除 API 副本

只有第 5.3 节两条 owner evidence 都通过后，才删除：

```text
crates/lattice-api/tests/git_store_no_remote_refspec.rs
```

不保留 deprecated wrapper，不在 API 层增加新的 facade 副本，也不顺手迁移其他 `git_store_*.rs`。其他文件属于 #188 或 Session Tree 存储 bake-off 的决策范围。

不要仅因删除此文件就修改 `lattice-api` 的 `gix` 依赖；依赖清理必须等全引用归零后由相应 owner issue 处理。

### 5.5 在旧 lock identity 尚存时新增 replacement

修改：

```text
crates/lattice-store/tests/integration.rs
```

在旧：

```text
create_lock_and_repo_lock_are_distinct_files
```

仍存在时，新增：

```text
create_returns_after_releasing_create_lock_and_repo_lock_is_distinct
```

允许在未提交 worktree 中短暂复制最小 setup，以便真实证明 replacement-first；不得先重命名旧函数，也不得为了临时并存建设长期 helper 或产品抽象。

### 5.6 在旧 identity 仍存在时单独运行 lock replacement

```bash
cargo test -p lattice-store --test integration \
  create_returns_after_releasing_create_lock_and_repo_lock_is_distinct \
  -- --exact
```

必须确认输出显示 `1 passed`，并在实施记录注明此时旧 identity 尚未删除。

### 5.7 删除旧 lock identity

focused replacement 通过后，删除旧：

```text
create_lock_and_repo_lock_are_distinct_files
```

最终只保留新测试。新测试必须：

- 删除 overclaim 注释；
- 增加 `assert_ne!(write.path(), create_lock.as_path())`；
- 保持 repository lock drop 后才 reacquire create lock；
- 不新增线程、进程、sleep、timeout 或双持锁路径。

### 5.8 更新记录

实施和验证完成后 append `FIX.md`。建议标题：

```text
GitHub #187: make Session config and create-lock tests execute their claimed assertions -- 2026-09-03
```

建议 severity：

```text
MEDIUM（test false-green / contract overclaim）
```

最终可根据实施中发现的真实产品影响调整，但条目必须显式包含：

- Issue id：#187；
- Severity；
- Problem：错误 bare config 路径、`if is_file` 静默跳过、lock 注释过度陈述；
- Fix：owner 迁移、无条件读取、semantic no-remote assertion、删除人工 remote-tracking、准确 lock replacement；
- Verification：全部 focused/full 命令与真实结果；
- Changed files。

`FINDINGS.md` 仅在发现真实生产缺陷且未在同一 PR 解决时追加；没有残余就不修改。

## 6. 预计 changed files

```text
A  docs/plans/2026-09-03-issue187-store-config-lock-test-integrity.md
M  crates/lattice-session/src/git_graph/tests/state_and_locks.rs
D  crates/lattice-api/tests/git_store_no_remote_refspec.rs
M  crates/lattice-store/tests/integration.rs
M  FIX.md
```

通常不修改：

```text
crates/lattice-store/src/repo.rs
crates/lattice-store/src/lock.rs
crates/lattice-session/src/git_graph/mod.rs
crates/lattice-session/AGENTS.md
crates/lattice-store/AGENTS.md
crates/lattice-api/Cargo.toml
Cargo.toml
docs/TESTING.md
FINDINGS.md
```

只有真实产品契约或 crate 导航发生变化时才更新 AGENTS/docs；本票默认不会。

## 7. 建议 commit 划分

### Commit 1

```text
test(session): 让 no-remote config 断言在 owner 层必然执行
```

包含最终状态：

- 新 config owner replacement；
- 两条 Session owner focused evidence 已在删除前通过；
- 删除 API 旧文件；
- 计划文件中的 exact inventory。

### Commit 2

```text
test(store): 收窄 create 与 repository lock 测试陈述
```

包含最终状态：

- 新 lock identity 已在旧 identity 尚存时 focused 通过；
- 删除旧 identity，最终只保留准确测试；
- `FIX.md`；
- 最终验证结果。

如果实现很小，也可合为一个 commit；replacement-first 是实施顺序和证据要求，不要求最终历史保留一个临时重复测试 commit。

## 8. 验证矩阵

### 8.1 Focused replacement-first

```bash
cargo test -p lattice-session \
  git_graph::tests::state_and_locks::created_session_store_config_is_readable_and_has_no_remote \
  -- --exact

cargo test -p lattice-session \
  git_graph::tests::pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically \
  -- --exact

cargo test -p lattice-store --test integration \
  create_returns_after_releasing_create_lock_and_repo_lock_is_distinct \
  -- --exact
```

前两条必须在删除 API 文件前通过；第三条必须在旧 lock identity 尚存时通过。默认预期是直接 GREEN 的 replacement/owner evidence，不在记录中虚构失败阶段。

### 8.2 删除后的 focused sanity

```bash
cargo test -p lattice-session \
  git_graph::tests::state_and_locks::created_session_store_config_is_readable_and_has_no_remote \
  -- --exact

cargo test -p lattice-store --test integration \
  create_returns_after_releasing_create_lock_and_repo_lock_is_distinct \
  -- --exact
```

并确认仓库中不存在旧 identities：

```bash
rg -n \
  'git_store_no_remote_push_refspec_for_lattice_refs|create_lock_and_repo_lock_are_distinct_files' \
  crates
```

预期零命中。

### 8.3 受影响 crate

```bash
cargo test -p lattice-session -p lattice-store -p lattice-api
```

### 8.4 静态检查

```bash
cargo fmt --all -- --check

cargo clippy \
  -p lattice-session \
  -p lattice-store \
  -p lattice-api \
  --all-targets -- -D warnings
```

### 8.5 仓库门禁

```bash
env -u LATTICE_BIN cargo xtask ci
```

若完整门禁受与本票无关的已知环境问题阻塞，必须记录：

- 实际运行到的命令；
- focused 与受影响 crate 的结果；
- 阻塞目标的 exact identity 和既有 owner；
- 不得把未运行描述为 PASS。

## 9. 验收标准到证据的映射

| Issue #187 验收项 | 落地证据 |
| --- | --- |
| 读取真实正确 config | replacement 使用 `store.dir().join("config")` 且无条件 `read_to_string` |
| 不使用 `if exists` | replacement 不存在条件跳过；config 缺失直接失败 |
| 不暗中扩张 config 契约 | 通过 `remote_names().is_empty()` 验证 no-remote；不对整个 config 做任意 `refs/lattice` 文本禁令 |
| 不人为构造 remote-tracking | replacement 不创建 Session/Turn，不枚举 `refs/remotes/**` |
| 被删 head 断言有 exact owner | `pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically` 在删除 API 文件前单独 `1 passed` |
| no-remote 所有权唯一 | API 旧文件删除；config owner test 位于 `lattice-session` |
| lock 名称、注释、断言一致 | 新名称只声明 release-after-create 与 distinct path |
| 不伪造双持锁契约 | repository lock drop 后才 reacquire create lock；注释不再声称同时持有 |
| replacement first | config/head evidence 在删 API 文件前通过；新 lock identity 在旧 identity 尚存时通过 |
| 维护记录 | `FIX.md` 含 issue id、severity、problem、fix、verification、changed files；无残余则不改 `FINDINGS.md` |

## 10. 完成定义

以下全部成立才可关闭 #187：

- 新的 Session config owner test 单独显示 `1 passed`；
- 现有 PendingTurn head/ref assertion-superset 单独显示 `1 passed`，且证据发生在删除 API 文件之前；
- 新的 lock test 在旧 identity 尚存时单独显示 `1 passed`；
- `crates/lattice-api/tests/git_store_no_remote_refspec.rs` 已删除；
- 仓库中不再存在两个旧 test identities；
- config 断言没有条件跳过，也没有全文件 `refs/lattice` 文本禁令；
- lock 测试没有超出代码路径的陈述；
- 受影响 crate tests、fmt、clippy 通过；
- `FIX.md` 已 append，且 issue id、severity、problem、fix、verification、changed files 与实际结果一致。
