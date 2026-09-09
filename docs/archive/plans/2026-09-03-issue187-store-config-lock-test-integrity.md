# Issue #187 实施计划：修正存储配置与锁测试的空转断言

> **修订标识：R2（2026-09-03）**：已落实本地代理的 3 个实质修正与 2 个记录修正；本标识用于区分此前错误交付的旧附件快照。

## 0. 计划元数据

- **目标 Issue**：GitHub #187《修正存储配置与锁测试的空转断言》
- **代码基线**：`main@34dcdc16cfb2d4e614dda04f514e77b38a26ccea`
- **基线日期**：2026-09-03
- **阻塞状态**：无；#187 当前为 `ready-for-agent`，可立即实施。
- **改动边界**：只修正无远端/配置测试的所有者与假绿，以及创建锁/仓库写锁测试的陈述范围；不修改产品持久化格式、Session 引用拓扑、锁实现或运行时行为。
- **实施原则**：先替换后删除、KISS、YAGNI。不得为一条测试新增通用 Git 配置解析器、双持锁契约、线程时序矩阵或产品抽象。
- **计划文件路径**：`docs/plans/2026-09-03-issue187-store-config-lock-test-integrity.md`
- **与后续架构探索的关系**：即使后续评估替换整个 Git 控制平面，本票仍应先完成；它修复当前基线的测试诚信，使存储最小可行产品的现状对比不建立在假绿上。
- **复核状态**：已吸收 2026-09-03 本地代理的 `NEEDS_MINOR_REVISION` 结论：修正锁测试的先替换后删除顺序、补齐被删分支头断言的精确所有者、收窄远端/引用规范契约，并补齐 `FIX.md` 严重级别与定向验证命名。

## 1. 最终目标

完成后必须同时满足：

1. 创建真实裸 Session 存储库后，无条件读取其中真实存在的 `sessions.git/config`。
2. 构造器未配置任何远端；因此也不存在由远端配置段设置的 `refs/lattice/**` 推送/拉取引用规范。
3. 测试不对整个配置施加“任何位置均不得出现 `refs/lattice` 文本”的新禁令；正式契约只约束远端/引用规范行为。
4. 无远端/配置契约由 `lattice-session` 所有者层唯一持有，`lattice-api` 不再拥有该 Git 存储库内部测试。
5. 删除旧 API 文件前，旧文件中的 Session 分支头断言已经有一个精确、已通过且更强的所有者替代测试。
6. 创建锁/仓库写锁测试只陈述并断言它实际证明的事实：
   - `StoreRepo::create*` 返回时创建锁已释放；
   - 仓库写锁使用仓库内另一条锁文件路径。
7. 不新增“同时持有两把锁必须互不阻塞”的契约。当前设计要求首次创建锁与仓库写锁不重叠，本票不扩张它。
8. 新锁测试标识必须在旧标识尚未删除时单独通过；最终提交只保留新测试。
9. `FIX.md` 显式记录 Issue 编号、严重级别、问题、修复、验证和变更文件；只有真实残余风险才进入 `FINDINGS.md`。

## 2. 当前代码事实

### 2.1 无远端测试的关键配置断言没有执行

当前测试标识：

```text
crates/lattice-api/tests/git_store_no_remote_refspec.rs
└── git_store_no_remote_push_refspec_for_lattice_refs
```

测试创建的是裸仓库：

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
    // 配置断言
}
```

错误路径不存在时，测试仍然成功；配置断言实际零执行。

### 2.2 当前测试混入了不属于配置契约的 Session 图构造

旧测试还执行了：

- 创建 Session；
- 通过测试专用的直接提交辅助函数提交一个 Turn；
- 断言 `refs/lattice/sessions/<id>/heads/main` 指向该提交；
- 枚举 `refs/remotes/**`，确认没有远端跟踪引用指向该提交。

这些步骤不是证明“构造器没有配置远端”的必要条件，并引入 Turn、清单、Session 引用、`test-utils` 与 `gix` 对象遍历的额外耦合。

### 2.3 无远端/配置契约属于 `lattice-session`

`GitSessionGraphStore`、Session 存储库路径、存储标识，以及“不为 `refs/lattice/**` 配置远端推送引用规范”的规则都由 `lattice-session` 持有。自然所有者是：

```text
crates/lattice-session/src/git_graph/tests/state_and_locks.rs
```

该模块已经集中覆盖 Session 存储库的 `create`/`open`、标识、严格打开和锁定行为，不需要再建测试模块。

GitHub #188 已明确无远端/引用规范不属于其范围，因此 #187 是该契约唯一负责落地的 Issue。

### 2.4 旧分支头断言已有更强所有者

旧 API 测试中的：

```text
Session 分支头引用指向刚提交的 Turn
```

已有明确的所有者替代测试：

```text
git_graph::tests::pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically
```

该测试走生产最终发布路径，并同时验证分支头、永久 Turn 固定引用、Session 状态与 `active-turn` 删除的一次原子发布。它严格强于旧测试只检查测试专用直接提交的分支头引用。

### 2.5 锁测试的注释超过了断言

当前测试标识：

```text
crates/lattice-store/tests/integration.rs
└── create_lock_and_repo_lock_are_distinct_files
```

当前执行顺序：

```text
创建存储库
→ 获取仓库写锁
→ 检查仓库写锁路径
→ 释放仓库写锁
→ 以非阻塞方式获取创建锁
```

它实际证明：

- 创建锁文件位于仓库父目录；
- 仓库写锁位于 Git 目录中，文件名为 `lattice-repo.lock`；
- `create` 返回后创建锁已释放。

它没有在持有一种锁时获取另一种锁，因此没有证明注释中的：

```text
持有一把锁绝不会阻塞另一把锁
```

正确修复是收窄名称与注释，而不是补一条没有生产消费者的双持锁契约。

## 3. 迁移前精确测试标识与断言清单

| 当前测试标识 | 当前断言 | 处理 | 唯一替代测试 / 所有者 | 断言超集说明 |
| --- | --- | --- | --- | --- |
| `git_store_no_remote_push_refspec_for_lattice_refs` | Session 分支头引用指向已提交的 Turn | 从旧 API 文件删除，但不丢失覆盖 | `git_graph::tests::pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically` / `lattice-session` | 生产最终发布同时断言分支头、Turn 固定引用、状态与 `active-turn` 删除的原子发布，严格强于旧测试专用直接提交的分支头断言 |
| 同上 | `repo.remote_names()` 为空 | 保留并迁移 | `git_graph::tests::state_and_locks::created_session_store_config_is_readable_and_has_no_remote` / `lattice-session` | 同一语义断言迁到构造器所有者；不再依赖 Session/Turn 测试夹具 |
| 同上 | 正确的裸仓库配置必须存在且可读 | 修正路径后保留并增强 | 同上 | 旧测试可能零执行；替代测试无条件读取 `store.dir()/config`，因此严格增强 |
| 同上 | 整个配置不含任意 `refs/lattice` 文本 | 不作为正式契约保留 | 无；改由无远端语义断言持有远端/引用规范契约 | 旧断言此前没有执行；直接启用会暗中扩张契约，不能把全文件文本禁令伪装成引用规范检查 |
| 同上 | 无 `refs/remotes/**` 指向该测试专用直接提交 | 按非契约构造删除 | 无替代测试 | 构造器未配置远端已由所有者测试证明；额外创建 Turn 并按 OID 扫描远端跟踪引用不增加信号 |
| `create_lock_and_repo_lock_are_distinct_files` | 两条锁文件路径位置不同 | 保留 | 新 `create_returns_after_releasing_create_lock_and_repo_lock_is_distinct` / `lattice-store` | 新测试增加直接 `assert_ne!`，并保留两条位置断言 |
| 同上 | `create` 返回后可以非阻塞获取创建锁 | 保留 | 同上 | 同一行为断言与获取顺序；名称准确表达创建完成后锁已释放 |
| 同上注释 | 同时持有一种锁不阻塞另一种 | 删除过度陈述 | 无替代测试 | 旧测试从未证明，生产契约也不要求；不建立新并发行为 |

“无替代测试”只用于明确退役的非契约断言，必须在 PR 中如实记录，不能写成“已有其他测试大概覆盖”。

## 4. 设计决定

### 4.1 Session 配置替代测试标识

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
2. `read_to_string(...).expect(...)` 无条件执行；配置缺失必须失败。
3. 通过 Git 配置语义层的 `remote_names().is_empty()` 证明未配置远端。

不得保留：

```rust
config.contains("refs/lattice")
```

这种全文件文本扫描禁止了正式契约之外的内容。未配置远端已经排除远端配置项拥有的推送/拉取引用规范。若实施者仍希望添加独立的推送引用规范断言，只能使用 `gix` 配置解析接口检查 `remote.*.push` / `remote.*.fetch` 的解析值，不得扫描整个配置，也不得新增通用解析器。

不做完整的配置黄金快照；`gix` 将来增加无害的 `[core]` 配置时不应导致测试失败。

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

第一条持有配置/无远端契约；第二条持有旧文件中的分支头引用断言。远端跟踪引用 OID 扫描作为非契约构造退役，不寻找伪替代测试。

### 4.3 锁替代测试标识与范围

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
首次创建锁文件保留在父目录中，但 StoreRepo::create* 返回前已经释放。
稳态仓库写操作使用仓库内另一条锁文件路径。
```

不得声明：

```text
持有一把锁绝不会阻塞另一把锁
可以同时持有两把锁
```

临时并存只用于验证先替换后删除；最终提交删除旧测试标识，不保留重复测试。

### 4.4 默认不修改生产实现

本票预计只修改测试和记录。出现以下任一情况才升级为产品缺陷：

- 正确路径的裸仓库配置不存在或不可读；
- 配置语义读取发现构造器确实创建了远端；
- `StoreRepo::create*` 返回时创建锁仍被持有；
- 仓库写锁与创建锁实际解析为同一路径。

遇到真实产品缺陷时不得弱化替代测试或恢复条件跳过；停止删除旧测试，先修产品问题。只有这种情况才记录真实的失败转为通过证据。替代测试默认预期直接通过，不得虚构失败阶段。

## 5. 实施步骤

### 5.1 先记录迁移证据表

在实施提交或 PR 描述中先写入第 3 节的精确测试标识、唯一替代测试、断言超集和明确退役项。不要只列文件名。

### 5.2 新增 Session 配置所有者替代测试

修改：

```text
crates/lattice-session/src/git_graph/tests/state_and_locks.rs
```

要求：

- 使用该模块已有 `temp_dir`；
- 只创建存储库，不创建 Session，不提交 Turn；
- 无条件读取正确的配置文件；
- 不遍历 Session 引用、提交对象或 `refs/remotes/**`；
- 不抽取只被这一条测试使用的辅助函数。

### 5.3 在删除 API 文件前运行两条 Session 所有者证据

```bash
cargo test -p lattice-session \
  git_graph::tests::state_and_locks::created_session_store_config_is_readable_and_has_no_remote \
  -- --exact

cargo test -p lattice-session \
  git_graph::tests::pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically \
  -- --exact
```

两条命令都必须确认输出显示 `1 passed`，不能只看退出码；错误过滤器零命中也可能返回成功。实施记录分别保存两条精确测试标识的结果。

### 5.4 删除 API 副本

只有第 5.3 节两条所有者证据都通过后，才删除：

```text
crates/lattice-api/tests/git_store_no_remote_refspec.rs
```

不保留弃用包装，不在 API 层增加新的门面副本，也不顺手迁移其他 `git_store_*.rs`。其他文件属于 #188 或 Session Tree 存储对比评估的决策范围。

不要仅因删除此文件就修改 `lattice-api` 的 `gix` 依赖；依赖清理必须等全引用归零后由相应责任 Issue 处理。

### 5.5 在旧锁测试标识尚存时新增替代测试

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

允许在未提交工作树中短暂复制最小准备代码，以真实证明先替换后删除；不得先重命名旧函数，也不得为了临时并存建设长期辅助函数或产品抽象。

### 5.6 在旧测试标识仍存在时单独运行锁替代测试

```bash
cargo test -p lattice-store --test integration \
  create_returns_after_releasing_create_lock_and_repo_lock_is_distinct \
  -- --exact
```

必须确认输出显示 `1 passed`，并在实施记录注明此时旧测试标识尚未删除。

### 5.7 删除旧锁测试标识

定向替代测试通过后，删除旧测试：

```text
create_lock_and_repo_lock_are_distinct_files
```

最终只保留新测试。新测试必须：

- 删除过度陈述注释；
- 增加 `assert_ne!(write.path(), create_lock.as_path())`；
- 保持释放仓库写锁后才重新获取创建锁；
- 不新增线程、进程、休眠、超时或双持锁路径。

### 5.8 更新记录

实施和验证完成后追加到 `FIX.md`。建议标题：

```text
GitHub #187：让 Session 配置与创建锁测试执行其声明的断言 -- 2026-09-03
```

建议严重级别：

```text
MEDIUM（测试假绿 / 契约过度陈述）
```

最终可根据实施中发现的真实产品影响调整，但条目必须显式包含：

- Issue 编号：#187；
- 严重级别；
- 问题：错误的裸仓库配置路径、`if is_file` 静默跳过、锁注释过度陈述；
- 修复：所有者迁移、无条件读取、无远端语义断言、删除人工远端跟踪引用、准确的锁替代测试；
- 验证：全部定向/完整命令与真实结果；
- 变更文件。

`FINDINGS.md` 仅在发现真实生产缺陷且未在同一 PR 解决时追加；没有残余就不修改。

## 6. 预计变更文件

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

只有真实产品契约或 crate 导航发生变化时才更新 `AGENTS.md`/文档；本票默认不会。

## 7. 建议提交划分

### 提交 1

```text
测试：让无远端配置断言在所有者层必然执行
```

包含最终状态：

- 新配置所有者替代测试；
- 两条 Session 所有者定向证据已在删除前通过；
- 删除 API 旧文件；
- 计划文件中的精确断言清单。

### 提交 2

```text
测试：收窄创建锁与仓库写锁的测试陈述
```

包含最终状态：

- 新锁测试标识已在旧标识尚存时定向通过；
- 删除旧标识，最终只保留准确测试；
- `FIX.md`；
- 最终验证结果。

如果实现很小，也可合为一个提交；先替换后删除是实施顺序和证据要求，不要求最终历史保留一个临时重复测试提交。

## 8. 验证矩阵

### 8.1 定向的先替换后删除验证

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

前两条必须在删除 API 文件前通过；第三条必须在旧锁测试标识尚存时通过。替代测试/所有者证据默认预期直接通过，不在记录中虚构失败阶段。

### 8.2 删除后的定向回归

```bash
cargo test -p lattice-session \
  git_graph::tests::state_and_locks::created_session_store_config_is_readable_and_has_no_remote \
  -- --exact

cargo test -p lattice-store --test integration \
  create_returns_after_releasing_create_lock_and_repo_lock_is_distinct \
  -- --exact
```

并确认仓库中不存在旧测试标识：

```bash
rg -n \
  'git_store_no_remote_push_refspec_for_lattice_refs|create_lock_and_repo_lock_are_distinct_files' \
  crates
```

预期零命中。

### 8.3 受影响的 crate

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
- 定向测试与受影响 crate 的结果；
- 阻塞目标的精确测试标识和既有所有者；
- 不得把未运行描述为通过。

## 9. 验收标准到证据的映射

| Issue #187 验收项 | 落地证据 |
| --- | --- |
| 读取真实正确配置 | 替代测试使用 `store.dir().join("config")` 且无条件调用 `read_to_string` |
| 不使用 `if exists` | 替代测试不存在条件跳过；配置缺失直接失败 |
| 不暗中扩张配置契约 | 通过 `remote_names().is_empty()` 验证未配置远端；不对整个配置做任意 `refs/lattice` 文本禁令 |
| 不人为构造远端跟踪引用 | 替代测试不创建 Session/Turn，不枚举 `refs/remotes/**` |
| 被删分支头断言有精确所有者 | `pending_journal::finalize_publishes_head_pin_state_and_deletes_active_turn_atomically` 在删除 API 文件前单独显示 `1 passed` |
| 无远端契约所有权唯一 | API 旧文件删除；配置所有者测试位于 `lattice-session` |
| 锁测试名称、注释、断言一致 | 新名称只声明创建返回后锁已释放与路径不同 |
| 不伪造双持锁契约 | 释放仓库写锁后才重新获取创建锁；注释不再声称同时持有 |
| 先替换后删除 | 配置/分支头证据在删除 API 文件前通过；新锁测试标识在旧标识尚存时通过 |
| 维护记录 | `FIX.md` 含 Issue 编号、严重级别、问题、修复、验证、变更文件；无残余则不改 `FINDINGS.md` |

## 10. 完成定义

以下全部成立才可关闭 #187：

- 新的 Session 配置所有者测试单独显示 `1 passed`；
- 现有 PendingTurn 分支头/引用断言超集单独显示 `1 passed`，且证据发生在删除 API 文件之前；
- 新锁测试在旧测试标识尚存时单独显示 `1 passed`；
- `crates/lattice-api/tests/git_store_no_remote_refspec.rs` 已删除；
- 仓库中不再存在两个旧测试标识；
- 配置断言没有条件跳过，也没有全文件 `refs/lattice` 文本禁令；
- 锁测试没有超出代码路径的陈述；
- 受影响 crate 的测试、格式检查、Clippy 通过；
- `FIX.md` 已追加，且 Issue 编号、严重级别、问题、修复、验证、变更文件与实际结果一致。
