# Issue #187 实施计划：修正 store、config 与 lock 测试的空转断言

## 0. 计划元数据

- **目标 Issue**：GitHub #187《修正 store、config 与 lock 测试的空转断言》
- **代码基线**：`main@34dcdc16cfb2d4e614dda04f514e77b38a26ccea`
- **基线日期**：2026-09-03
- **阻塞状态**：无；#187 当前为 `open`、`ready-for-agent`，无评论，可立即实施。
- **唯一相邻票**：#188 负责其余 Session Git 图不变量迁回 `lattice-session`；#188 已明确写明 no-remote/refspec 不属于其范围。
- **改动边界**：只修两条测试的真实性与所有权，删除一条放错层级的 API integration test，并更新 `FIX.md`。除非新的正确测试暴露真实产品缺陷，否则不修改生产代码。
- **实施原则**：KISS、YAGNI、replacement-first。不得借本票重构 Git 配置 API、锁注册表、Session graph、durability barrier 或测试框架。
- **计划文件规则**：本文件是 #187 的唯一权威实施计划；不得另建近似的 `issue187-*-plan.md` 副本。

## 1. 最终目标

完成后必须满足以下不变量：

1. Session store 创建后，测试从**真实 bare repository 的真实 config**读取内容；config 缺失或路径错误必须直接失败。
2. 测试直接证明创建过程没有配置 remote，且 config 不包含会传播 `refs/lattice/**` 的 refspec。
3. no-remote/refspec 契约由 `lattice-session` owner 层唯一持有；`lattice-api/tests/git_store_no_remote_refspec.rs` 不再存在。
4. no-remote 测试不创建 Session、不提交 Turn、不制造 branch ref，也不扫描人工 remote-tracking 场景。
5. create lock 测试只声明它实际证明的两件事：
   - `StoreRepo::create*` 返回后 create lock 已释放；
   - steady-state repository write lock 使用另一条、位于 Git 目录内部的路径。
6. 锁测试不再声称“同时持有一种锁时另一种锁一定 non-blocking 成功”，因为生产契约要求 create lock 与 repository write lock 不重叠，当前调用图没有这一更强需求。
7. replacement 必须先单独通过，再删除旧测试；最终测试名称、注释和断言范围一致。

## 2. 当前代码事实

### 2.1 no-remote 测试读取了错误路径，并通过 `if exists` 静默跳过

当前 exact identity：

```text
crates/lattice-api/tests/git_store_no_remote_refspec.rs
└── git_store_no_remote_push_refspec_for_lattice_refs
```

测试创建的是 bare repository：

```rust
GitSessionGraphStore::create(dir.join("sessions.git"))
```

bare repository 的配置位于：

```text
<tmp>/sessions.git/config
```

当前测试却读取：

```rust
dir.join(".git").join("config")
```

并把核心断言放在：

```rust
if config_path.is_file() {
    // assert config does not mention refs/lattice
}
```

因此在正常执行中错误路径不存在，核心 config 断言零执行，测试仍然通过。这是本票必须消除的 false-green。

### 2.2 旧 no-remote 测试夹带了三个非目标场景

为了检查创建时的 repository config，旧测试额外执行了：

1. 创建 Session；
2. 通过 `test_commit_turn` 创建 Turn commit 和 Session branch ref；
3. 枚举 `refs/remotes/**`，检查没有 remote-tracking ref 指向该 commit。

这些步骤不参与“`GitSessionGraphStore::create` 是否写入 remote/refspec”的证明。它们扩大 fixture、引入 test-only Turn API，并把 Session graph、ref publication 和 remote-tracking 混入一个配置测试。

本票应有意删除这些非目标断言，而不是把它们迁到 replacement。Session branch/ref/commit 行为由 #188 及 `lattice-session` 既有 owner tests 持有。

### 2.3 no-remote 契约属于 `lattice-session`

`lattice-session/AGENTS.md` 已明确规定：

```text
Do not add remote push refspecs for refs/lattice/**.
```

该契约描述 `GitSessionGraphStore` 创建出的 repository，而不是 `SessionApi` facade。新的唯一 owner 应位于：

```text
crates/lattice-session/src/git_graph/tests/state_and_locks.rs
```

该模块已经拥有 Session store 的 strict open、identity、first-create、state 和 lock acceptance，不需要新建测试模块。

### 2.4 lock 测试的注释比执行序列证明得更多

当前 exact identity：

```text
crates/lattice-store/tests/integration.rs
└── create_lock_and_repo_lock_are_distinct_files
```

当前测试实际执行：

```text
StoreRepo::create_global 返回
→ 获取 repository write lock
→ 检查 write lock 路径
→ drop write lock
→ non-blocking 获取 create lock
```

它证明：

- create lock 文件保留在 repository 父目录；
- repository write lock 文件位于 Git 目录内部，名称以 `lattice-repo.lock` 结尾；
- create 返回后 create lock 已释放。

它没有在持有一种锁时尝试获取另一种锁，因此不能证明注释中的：

```text
holding one never blocks the other
```

同时，`lattice-store` 的正式边界已经规定 first-create lock 与 repository write lock **never overlap**。为了一个没有生产消费者的更强性质新增“双持锁”测试，会扩张契约而不是修正测试。

## 3. exact identity、唯一 replacement 与断言映射

| 旧 identity | 唯一 owner / replacement | 处理 |
|---|---|---|
| `lattice-api --test git_store_no_remote_refspec::git_store_no_remote_push_refspec_for_lattice_refs` | `lattice_session::git_graph::tests::state_and_locks::created_session_store_config_has_no_remote_or_lattice_refspec` | replacement 单独通过后删除整个 API test file |
| `lattice-store --test integration::create_lock_and_repo_lock_are_distinct_files` | 原地重命名为 `create_returns_with_create_lock_released_and_repository_lock_on_distinct_path` | 收窄注释，保留现有真实断言，增加直接路径不等断言 |

### 3.1 no-remote assertion mapping

| 旧断言 | 新处理 | 理由 |
|---|---|---|
| Session branch ref 存在并指向 commit | 删除 | Session graph 行为，不是 config 契约 |
| `repo.remote_names()` 为空 | 保留 | 直接证明 store 未配置 remote |
| raw config 不包含 `refs/lattice` | 加强 | 改为读取正确且必然存在的 config；无条件执行 |
| 没有 `refs/remotes/**` 指向 commit | 删除 | 没有 remote 时是派生事实；人工 commit/remote-tracking 场景无必要 |

replacement 不是旧测试逐行断言超集，但对本票拥有的**目标契约**是严格增强：

```text
旧：remote list 为空；raw config 断言可能零执行
新：remote list 为空；正确 config 必须成功读取；refs/lattice 检查必然执行
```

删除的断言必须在 PR 描述中明确标成“有意删除的非目标断言”，不能声称无损逐行迁移。

### 3.2 lock assertion mapping

| 当前事实 | 新处理 |
|---|---|
| create lock 文件存在于 parent | 保留 |
| write lock 位于 `repo.git_dir()` | 保留 |
| write lock 文件名为 `lattice-repo.lock` | 保留 |
| create 返回后可 non-blocking 再获取 create lock | 保留 |
| 两条 lock path 不同 | 新增直接 `assert_ne!` |
| 同时持有两种锁互不阻塞 | 不声明、不测试 |

## 4. 目标实现

### 4.1 在 Session owner 层新增正确的 config 测试

目标 identity：

```text
git_graph::tests::state_and_locks::
created_session_store_config_has_no_remote_or_lattice_refspec
```

建议实现形状：

```rust
#[test]
fn created_session_store_config_has_no_remote_or_lattice_refspec() {
    let dir = temp_dir("no_remote_refspec");
    let path = dir.path().join("sessions.git");
    let store = GitSessionGraphStore::create(&path).expect("create");

    let config_path = store.dir().join("config");
    let config = std::fs::read_to_string(&config_path)
        .expect("created bare Session store must have a readable config");

    let repo = gix::open_opts(store.dir(), gix::open::Options::isolated())
        .expect("open created Session store");
    let remotes: Vec<_> = repo.remote_names().into_iter().collect();

    assert!(
        remotes.is_empty(),
        "Session store creation must not configure remotes: {remotes:?}"
    );
    assert!(
        !config.contains("refs/lattice"),
        "Session store config must not contain a refs/lattice refspec: {config}"
    );
}
```

实施约束：

- 使用 `store.dir().join("config")`，不要再次手工猜 `.git/config`。
- `read_to_string(...).expect(...)` 必须无条件执行；不得使用 `is_file()`、`exists()` 或 `if let Ok(...)` 跳过。
- 不调用 `create_session`。
- 不调用 `test_commit_turn`。
- 不创建或枚举 Session branch refs。
- 不遍历 `refs/remotes/**`。
- 不做整个 config 的 golden snapshot；只检查本票拥有的语义。
- 不新增依赖；`lattice-session` 已直接依赖 `gix`。
- 不在 #187 中删除 `lattice-api` 的 `gix` dev-dependency；其他测试仍有使用，依赖清理由 #188 在完成全量 inventory 后决定。

### 4.2 删除上层副本

replacement 单独通过后删除：

```text
crates/lattice-api/tests/git_store_no_remote_refspec.rs
```

不创建新的 API replacement。`SessionApi` 没有为 remote/refspec 增加任何 facade 语义，因此 API 层不应保留 forwarding smoke。

#188 已明确排除 no-remote/refspec，本票无需改写 #188 issue 正文。若实施时发现其他计划文件把该契约分配给 #188，只删除那条重复所有权描述，不扩 #187 的代码范围。

### 4.3 原地校准 create/repository lock 测试

目标 identity：

```text
create_returns_with_create_lock_released_and_repository_lock_on_distinct_path
```

建议实现形状：

```rust
#[test]
fn create_returns_with_create_lock_released_and_repository_lock_on_distinct_path() {
    let state = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let repo = StoreRepo::create_global(&layout, "sessions.git", meta(), |_| Ok(())).unwrap();

    // The first-create lock file remains in the parent, but create returns
    // only after releasing it. Steady-state writes use a distinct lock inside
    // the published repository.
    let create_lock = layout.lattice_root().join(".sessions.git.create.lock");
    assert!(create_lock.exists());

    let write = repo.write_lock().unwrap();
    assert!(write.path().starts_with(repo.git_dir()));
    assert!(write.path().ends_with("lattice-repo.lock"));
    assert_ne!(write.path(), create_lock.as_path());
    drop(write);

    let create = lattice_store::lock::acquire_flock_strict(
        &create_lock,
        lattice_store::Blocking::NonBlocking,
    )
    .expect("create lock must be released after create returns");
    drop(create);
}
```

不得增加：

- 第二线程或第二进程；
- sleep、timeout 或调度假设；
- 在持有 repository write lock 时获取 create lock；
- 在持有 create lock 时获取 repository write lock；
- lock registry 或 FIFO fairness 的重复覆盖。

这些都不是 #187 的目标。锁实现、same-process registry 和 FIFO waiters 已由 `lattice-store` owner tests 持有。

## 5. 预计修改文件

```text
A  docs/plans/2026-09-03-issue187-store-config-lock-tests.md
M  crates/lattice-session/src/git_graph/tests/state_and_locks.rs
D  crates/lattice-api/tests/git_store_no_remote_refspec.rs
M  crates/lattice-store/tests/integration.rs
M  FIX.md
```

默认不修改：

```text
crates/lattice-store/src/lock.rs
crates/lattice-store/src/repo.rs
crates/lattice-session/src/git_graph/mod.rs
crates/lattice-api/Cargo.toml
crates/lattice-session/AGENTS.md
crates/lattice-store/AGENTS.md
docs/TESTING.md
FINDINGS.md
```

只有正确路径的新测试暴露真实生产行为缺陷时，才暂停实施并重新分流为产品 bug；不得在同一补丁里顺手修生产实现。

## 6. 实施顺序

### 步骤 1：记录迁移 inventory

在 PR 描述或实施记录中先列出第 3 节的两条 exact identity、唯一 replacement、保留断言和有意删除项。不要恢复历史 test-triage ledger。

### 步骤 2：先增加 Session owner replacement

只修改 `state_and_locks.rs`，运行：

```bash
cargo test -p lattice-session \
  git_graph::tests::state_and_locks::created_session_store_config_has_no_remote_or_lattice_refspec \
  -- --exact
```

必须观察到：

```text
running 1 test
test ...created_session_store_config_has_no_remote_or_lattice_refspec ... ok
```

任何 `running 0 tests` 都是失败，不得当作验证通过。

### 步骤 3：删除 API 旧副本

确认 replacement 通过后删除整个：

```text
crates/lattice-api/tests/git_store_no_remote_refspec.rs
```

删除后不再调用已经消失的 integration-test target；使用 crate-level 测试确认 integration test auto-discovery 和剩余 API tests 正常：

```bash
cargo test -p lattice-api
```

并通过文件搜索确认旧 identity 消失：

```bash
rg -n \
  'git_store_no_remote_push_refspec_for_lattice_refs|git_store_no_remote_refspec' \
  crates/lattice-api crates/lattice-session
```

允许仅命中新 replacement 的语义描述；不得残留旧 test file 或旧 identity。

### 步骤 4：校准 lock test

原地重命名测试，收窄注释，增加 lock path 直接不等断言，然后运行：

```bash
cargo test -p lattice-store --test integration \
  create_returns_with_create_lock_released_and_repository_lock_on_distinct_path \
  -- --exact
```

同样必须确认只运行一条目标测试。

### 步骤 5：受影响 crate 验证

```bash
cargo test -p lattice-session -p lattice-store -p lattice-api

cargo clippy \
  -p lattice-session \
  -p lattice-store \
  -p lattice-api \
  --all-targets -- -D warnings

cargo fmt --all -- --check
```

本票不需要：

- `lattice-system-harness`；
- KVM / MSB；
- release E2E；
- performance qualification；
- SIGKILL matrix；
- remote ext4 smoke。

### 步骤 6：记录落地结果

所有测试通过后 append 一条 `FIX.md` 记录，至少包含：

- GitHub #187；
- false-green 根因：bare config path 写错且被 `if exists` 跳过；
- replacement identity；
- API 副本删除；
- lock 测试的新名称与收窄契约；
- 实际 changed files；
- 实际执行的 exact、crate、clippy、fmt 命令。

只有出现真实残余风险时才修改 `FINDINGS.md`。如果本票按计划完成且没有新风险，`FINDINGS.md` 不应产生占位条目。

## 7. Commit 建议

实施 PR 可保持两个小 commit：

### Commit 1

```text
test(session): 直接验证创建 store 的 remote/refspec 配置
```

内容：

- 新增 Session owner replacement；
- 独立 exact test 通过；
- 删除 API integration 副本。

### Commit 2

```text
test(store): 校准 create 与 repository lock 断言
```

内容：

- 重命名和收窄 lock test；
- 增加 path 不等断言；
- 更新 `FIX.md`；
- 记录完整验证结果。

不要求为了 commit 纯度拆分更多提交。

## 8. 明确非目标

- 不迁移其他 `git_store_*.rs`；它们属于 #188。
- 不修改 remote policy 或增加 remote/refspec 管理 API。
- 不为“未来可能同时持有两把锁”增加新契约。
- 不重构 `acquire_flock` / `acquire_flock_strict`。
- 不修改 repository durability barrier。
- 不清理所有 API `gix` 使用或 dev-dependency。
- 不创建通用 repository-config assertion helper。
- 不新建 `refs.rs`、`config.rs` 或 lock test framework。
- 不把这张小票扩展为 Session storage architecture 迁移。
- 不因后续 Session Tree MVP 计划而弱化当前测试修复；#187 先作为独立 correctness cleanup 落地。

## 9. Acceptance criteria 对照

| #187 验收项 | 本计划落点 |
|---|---|
| 读取真实 Session store 的正确 config | §4.1 使用 `store.dir()/config` 且无条件读取 |
| 不用 `if exists`，不构造 remote-tracking | §4.1 明确禁止；删除旧 Session/Turn/ref fixture |
| no-remote 所有权唯一 | §2.3、§4.2 迁入 `lattice-session`，删除 API 文件 |
| lock 名称、注释和断言一致 | §4.3 精确重命名与收窄注释 |
| 若双持锁仍是契约必须真实验证 | 本计划明确判定它不是当前契约，因此不再声明 |
| replacement-first 与 assertion-superset | §3、§6 步骤 1–3 |
| 更新 FIX/FINDINGS | §6 步骤 6 |

## 10. 完成定义

只有全部满足才可关闭 #187：

- [ ] 新 owner test 从 `sessions.git/config` 无条件读取。
- [ ] 新 owner test 证明无 configured remote、无 `refs/lattice` refspec。
- [ ] 新 owner exact test 真实运行 1 条并通过。
- [ ] 旧 API test file 已删除。
- [ ] 旧 test identity 全仓库不再存在。
- [ ] lock test 新名称准确描述释放与路径分离。
- [ ] lock test 没有双持锁或并发扩张。
- [ ] `lattice-session`、`lattice-store`、`lattice-api` 测试通过。
- [ ] 三个 crate 的 clippy 与 workspace fmt check 通过。
- [ ] `FIX.md` 记录实际验证。
- [ ] 没有无依据修改 `FINDINGS.md`、AGENTS 或生产代码。
