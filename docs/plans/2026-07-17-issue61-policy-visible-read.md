# #61 实现计划：统一 Policy-Visible Workspace 读可见性

- 票：<https://github.com/Skyline10124/lattice/issues/61>（原 Codeberg #39）
- 前置依赖：#60（[计划](2026-07-17-issue60-remove-execution-backend.md)）。
  全局约束与验证矩阵见 [总览](2026-07-17-direct-local-overview.md)。
- 对应 ADR 0003 Migration Sequence step 4 的 `[lancet.workspace]` 部分
  （`[workspace]` schema 删除归 #62）。

## 1. profile schema（`lattice-profile`）

```rust
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]                    // 未知字段 fail closed（已有）
pub struct LancetWorkspacePolicySection {
    #[serde(default)] pub denied_write_patterns: Vec<String>,
    #[serde(default)] pub included_ignored_read_patterns: Vec<String>,
    #[serde(default)] pub denied_read_patterns: Vec<String>,
}
```

校验复用 `normalize_write_path_pattern` 一族规则：project-relative、无 NUL、
无 `..`、无绝对路径、非空——不满足即加载失败（fail closed）。

## 2. authority 语义（`lancet-authority`）

```rust
// policy/visibility.rs（新）
/// 运行时常量，profile 不可触碰（CONTEXT.md: System Path Exclusion）
pub const SYSTEM_PATH_EXCLUSIONS: &[&str] = &[".git", ".lattice"];
// root escape 由 WorkspacePath/resolve_secure 层保证，不进 pattern 列表

pub enum ReadVisibility { Visible, Hidden(HiddenReason) }
pub enum HiddenReason { SystemExclusion, ReadDenied, GitIgnored }

pub struct ReadVisibilityPolicy {
    pub included_ignored: Vec<Pattern>,
    pub denied_read: Vec<Pattern>,
}
impl ReadVisibilityPolicy {
    /// 纯决策序（gitignore 事实由调用方注入，authority 不做 I/O）：
    /// System Exclusion > denied_read > included_ignored > gitignore 默认
    pub fn decide(&self, rel: &str, git: GitFileStatus) -> ReadVisibility {
        if under_system_exclusion(rel)            { return Hidden(SystemExclusion); }
        if self.denied_read.matches(rel)          { return Hidden(ReadDenied); }
        match git {
            Tracked | NotIgnored                  => Visible,
            Ignored if self.included_ignored.matches(rel) => Visible,
            Ignored                               => Hidden(GitIgnored),
        }
    }
}
```

`WorkspacePolicy` 增加两个 read 字段并入 `validate()`；write deny 语义与
`authorized_region_for` 不动（验收第 8 条：不引入 allowlist envelope）。

## 3. gix visibility 引擎（`lancet-native/workspace/visibility.rs`）

> ⚠️ gix 0.85 具体 API 以**锁定后的 crate 源码**为准（Phase 33 幻觉 API 教训）。
> 下面是概念骨架，落地前先做 30 分钟 spike 核对方法名。禁 shell、任意命令与
> Git subprocess（票文硬约束）。

```rust
/// 每个 Tool call 构建一次 —— "visibility 基于调用时的 Local state"：
/// index 与 exclude 栈不跨 Tool call 缓存，.gitignore/追踪状态变化即时生效。
pub(crate) struct VisibilityContext {
    inner: Option<GitVisibility>,   // None ⇒ 非 git 项目：无 tracked 集、无 ignore 规则，
}                                    //         全部按 non-ignored 处理；System Exclusion 照常

struct GitVisibility {
    repo: gix::Repository,          // gix::open_opts(root, isolated())——隔离全局配置
    index: gix::worktree::IndexPersistedOrInMemory,
    excludes: gix::AttributeStack,  // repo.excludes(&index, None, ..) 的产物
}

impl VisibilityContext {
    fn load(root: &Path) -> Self { /* gix::open 失败(非 repo) → inner: None */ }

    fn git_status(&mut self, rel: &BStr, is_dir: bool) -> GitFileStatus {
        let Some(g) = &mut self.inner else { return GitFileStatus::NotIgnored };
        // tracked：index 命中即可见（tracked ∪ non-ignored 是并集，
        // tracked-but-ignored 文件默认可见）
        if !is_dir && g.index.entry_by_path(rel).is_some() { return GitFileStatus::Tracked; }
        let platform = g.excludes.at_entry(rel, Some(is_dir_mode(is_dir)))?;
        if platform.is_excluded() { GitFileStatus::Ignored } else { GitFileStatus::NotIgnored }
    }
}
```

接线（单一决策点，四操作共用）：

- `observe.workspace read`：解析路径后 `decide()`；`Hidden(_)` → 与
  "文件不存在"**同一错误形状**（不泄漏 reason 到 Agent；reason 只进 audit）。
- `list`/`find`：遍历时对每个 entry `decide()`；被隐藏目录**默认剪枝不下降**，
  但若某 `included_ignored_read_patterns` 模式可能命中该目录内部（模式前缀
  可达性检查），仍需下降后逐项判定。
- `grep`：候选文件集先过 `decide()` 再搜索——隐藏文件不产生 match、不计入
  truncation 统计、不出现在错误文本。

## 4. audit 与测试

- decision evidence 进 durable audit：命中的规则类别（system_exclusion /
  read_denied / included_ignored / ignored_default）+ 截断事实；**不记录**
  被拒绝文件内容。
- focused tests（票文清单逐条）：
  - profile round-trip（新字段序列化/反序列化/未知字段 fail closed）
  - 四操作对同一 path 决策一致
  - ignored include 生效；read denial 优先于 include
  - System Exclusion 压倒一切（profile 试图 include `.git/config` 无效）
  - symlink/root escape
  - secret sentinel（`.env` 进 `.gitignore` → read/list/grep/find 四路全不可见）
  - 非 git 项目 fallback
  - `.gitignore` 变更即时生效（Turn 内两次调用之间改 ignore 规则）
