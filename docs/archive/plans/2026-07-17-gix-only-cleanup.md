# gix-only 收口计划：session graph 与测试基建迁 gix，移除 git2 与 git subprocess

- 票：**待开**（建议开票挂 #38；用户 2026-07-17 拍板的附加范围，超出 #62 票面）。
- 前置依赖：#62（[计划](2026-07-17-issue62-remove-upper-msb-residue.md)）——
  `lattice-workspace`（git2 最大用户之一）删除后再迁，避免白做。
- 与 #63/#64（rewind，本就定案 gix）同引擎，先行统一。
- 全局约束与验证矩阵见 [总览](2026-07-17-direct-local-overview.md)。

范围：`git2` 与 `Command::new("git")` 在仓库内清零（production、测试、
harness）。#62 之后的实测残留面：`lattice-session`（git_graph 4 文件 +
migrate 4 文件）、`lattice-api`（session.rs + 13 个 `git_store_*.rs` 测试 +
若干 fixture）、`lattice-cli/run_tests.rs`、`lattice-system-harness/world.rs`。

## 1. `lattice-session::git_graph` git2 → gix

先做 30 分钟 spike：gix 0.85 下 init_bare / write blob / tree editor / commit /
ref transaction / revwalk 各一遍，锁定 API 后再动主迁移。概念对照：

| git2 现状 | gix 0.85 目标 |
|---|---|
| `Repository::open/init_bare` | `gix::open_opts(dir, isolated())` / `gix::init_bare(dir)` |
| `repo.blob(bytes)` | `repo.write_blob(bytes)` |
| TreeBuilder | `repo.edit_tree(base)`（`gix_object::tree::Editor`）→ `.write()` |
| `repo.commit(Some(ref), sig, ..)` | `repo.commit(ref_name, message, tree_id, parents)`（签名走 committer 配置注入，isolated 模式下显式提供） |
| `repo.reference(name, oid, force, log)` | `repo.reference(name, id, PreviousValue::Any, log_message)` / ref transaction |
| revwalk | `repo.rev_walk([tip]).all()` |
| `diff_tree_to_tree` | `gix::object::tree::diff`（`tree.changes().for_each_to_obtain_tree(..)`） |

- `RepoHandle`/懒加载结构保留，仅内部类型换 `gix::Repository`（注意 gix
  `Repository` 非 `Sync`——沿用现有 `Mutex<Option<..>>` 包装即可，必要时换
  `gix::ThreadSafeRepository` + `to_thread_local()`）。
- **`migrate/` 模块处置（决策项，倾向删）**：SQLite→Git 一次性迁移已完成使命
  （SQLite store 已退役），alpha 无兼容义务 → 整目录删除，连带
  `check_migration_guard` 及 `lattice-tui`/`lattice-cli` 调用点。若要保守，
  迁 gix 的成本远高于价值。
- `lattice-api/src/session.rs` 与 `tests/git_store_*.rs` 的 git2 引用随 store
  迁移同步改写。
- **回归门**：git_graph 全部现有测试保留原断言先行通过，再逐步替换内部实现；
  gix 写出的对象格式同为 git，旧断言天然可读回。

## 2. 测试/harness fixture 统一 gix helper

新增共享测试 helper（放 `lattice-session` 的 `test-utils` feature，各 crate
dev-dependency 指向）：

```rust
/// 替换所有 Command::new("git") fixture：init + 首提交，返回工作树根
pub fn init_fixture_repo(dir: &Path, files: &[(&str, &str)]) -> gix::Repository {
    let repo = gix::init(dir).unwrap();                 // 非 bare，带工作树
    for (rel, content) in files { std::fs::write(dir.join(rel), content).unwrap(); }
    // blob → tree editor → commit refs/heads/main（API 以 spike 锁定版为准）
    ...
}
```

替换点（实测清单）：`lattice-api/{src/turn/cep.rs, src/turn/test_support.rs,
tests/*}`、`lattice-cli/run_tests.rs`、
`lattice-session/git_graph/tests/no_remote.rs`、
`lattice-system-harness/world.rs::git()`。

## 3. 收口断言

- `rg -n "git2" crates xtask --type rust` 与
  `rg -n 'Command::new\("git"\)' crates xtask --type rust` 双零。
- 根 `Cargo.toml` 删 `git2 = "0.20"`；`cargo tree -i git2` 无结果。
- （可选）在 `lattice-system-harness/layer_guard` 或 xtask 加 grep gate 防回潮。
