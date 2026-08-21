# Issue #118 — `lattice-tui` full-screen shell design

Date: 2026-08-22
Issue: <https://github.com/Skyline10124/lattice/issues/118>
Follow-on: #149 (card wiring), #116 (renderer), #117 (`ToolOutcome` seam)
Superseded roadmap entry: #101 C5 `render/` portion (see §13)

This document is a **signature-level implementation handoff**. It fixes the file
set, the public surface, every type and function signature, the state-machine
transition tables, and the key table. It deliberately does not contain function
bodies. Anything not written here is out of scope for v1 — 第一版保持克制是硬
约束，不是建议。

---

## 1. Grill decisions this design encodes

| # | Decision |
| --- | --- |
| D1 | `ToolOutcome` 类型落 `lattice-conversation`（#117），不在本票范围内，但本设计不得预先创建任何 Tool 结果结构。 |
| D2 | #116 的渲染器落 `crates/lattice-tui/src/render/`（模块，非新 crate），取代 #101 C5 的 `render/` 部分。本票**不建** `render/`。 |
| D3 | 内联 rustyline REPL 在本票内**删除**，不并存。`lattice-cli` 只有一个调用点。 |
| D4 | v1 克制清单（§3）是硬约束。 |
| D5 | 验证以纯状态机测试为主，渲染快照极少；PTY smoke 必须同步迁移（§12）。 |
| D6 | 上限分开计：本票的壳 ≤5 个文件 / ≤1200 行（**不含 `#[cfg(test)]` 模块**）；#116 的 `render/` 另计 ≤800 行。 |

---

## 2. Contract documents to amend (first commit)

第一个 commit 只改文档，不含 Rust 代码：

1. 根 `AGENTS.md`
   - Crate Roles 表中 `lattice-tui` 一行：删除 "Inline CLI chat REPL over
     `lattice-api` sessions and `stream_turn` via `rustyline` (no alternate
     screen, no ratatui/crossterm)"，改为全屏 TUI 表述，并写明 transcript 卡片
     渲染器（#116）作为本 crate 的 `render/` 模块落地，`lattice-cli` 通过既有的
     `lattice-cli -> lattice-tui` 边消费它。
   - Layer 5 描述中的 "presentation dependencies" 保持不变。
   - Ownership Rules 中 "Keep CLI output, non-interactive rendering, and argument
     parsing in `lattice-cli`" 一条：显式记录例外——共享的 transcript 渲染下沉到
     `lattice-tui`，因为 `lattice-tui` 不能依赖 `lattice-cli`，共享只能向下沉。
     这条同时消解 #116 验收标准与 #101 C5 标注的同一处边界漂移。
2. `crates/lattice-tui/AGENTS.md`
   - 删除 alternate screen / ratatui 禁令。
   - 新增 §键位表（§8 逐字抄入）。
   - 新增 §不复刻清单：主题系统 / 主题选择器、shell 模式（`!` 前缀）、会话分享、
     插件槽位与 JS 运行时、realtime / 语音、多 agent side thread。
   - 新增 §v1 克制清单（§3 逐字抄入）。

---

## 3. v1 scope

### 3.1 Three regions, nothing else

```text
┌────────────────────────────────────────────┐
│ transcript                      (flex)     │
│                                            │
├────────────────────────────────────────────┤
│ input                     (1..=6 rows)     │
├────────────────────────────────────────────┤
│ status                          (1 row)    │
└────────────────────────────────────────────┘
```

`transcript` 取剩余全部高度；`input` 随内容 1–6 行、超过 6 行内部滚动；`status`
恒为 1 行。**不画边框**（框线视觉语言归 #149 决定）。

### 3.2 Explicitly NOT in v1

- 会话选择器 UI（列表 / 搜索 / 预览）。`TuiOptions.session_id` 已经承担会话选择：
  给了就 open，不给就 create。v1 只操作当前会话。
- 鼠标，含滚轮。仅键盘。
- 语法高亮、markdown 渲染。transcript 是纯文本。
- reasoning 折叠 / usage-token 面板 / 搜索 / 命令面板 / 主题。
- 多于三个的界面区域；弹层、模态、toast。
- Tool call 卡片。v1 只有通用状态行（§9）。
- 会话分支 / 时间线视图。

---

## 4. File layout and line budget

```text
crates/lattice-tui/src/
  lib.rs        ~70    public surface, module tree
  session.rs   ~130    session open/create, profile load
  app.rs       ~360    three-arm event loop, App state, TurnEvent → Block
  input.rs     ~260    InputBuffer, History, key dispatch
  view.rs      ~250    layout, transcript wrapping, Viewport, status line
                ≈1070  (cap 1200, excluding #[cfg(test)] modules)
```

Deleted: `crates/lattice-tui/src/runtime.rs` (437 lines).
`crates/lattice-tui/src/app.rs` is rewritten wholesale (its current `AppConfig` /
`AppEvent` are removed; `lattice-cli` does not use them — verified: the only
cross-crate use is `lattice-cli/src/main.rs:136`).

### 4.1 Cargo.toml

```toml
[dependencies]
lattice-api = { workspace = true }
anyhow      = { workspace = true }
futures     = { workspace = true }
tokio       = { workspace = true }
ratatui     = { workspace = true }
crossterm   = { workspace = true, features = ["event-stream"] }
```

Removed: `colored`, `rustyline`.

**零新增 workspace 依赖。** `ratatui = "0.30"` 与 `crossterm = "0.29"` 已在
`[workspace.dependencies]` 中 pin。`ratatui` 默认 features 经 `ratatui-crossterm
0.1.1` 的 default `crossterm_0_29` 解析到同一份 crossterm 0.29，不会出现双版本。
`EventStream` 只能从独立 `crossterm` 依赖的 `event-stream` feature 获得
（`ratatui-crossterm` 无透传），因此该 feature 必须显式开启；这是 feature 增量，
不是新依赖。若发现 `colored` / `rustyline` 在移除后于 workspace 内彻底无消费者，
不在本票删除其 workspace pin（那是独立的清理）。

---

## 5. Public surface (unchanged)

`lattice-cli/src/main.rs:136` 一行不改。

```rust
// lib.rs
#[derive(Clone, Debug)]
pub struct TuiOptions {
    pub session_id: Option<String>,
    pub agent: String,
    pub project_branch: Option<String>,
    pub session_dir: PathBuf,
    pub agents_dir: PathBuf,
    pub project_root: PathBuf,
}

pub async fn run(options: TuiOptions) -> anyhow::Result<std::process::ExitCode>;
```

`AppConfig` 与 `AppEvent` 从公开面删除。

---

## 6. Signatures

### 6.1 `session.rs`

```rust
pub(crate) struct SessionContext {
    pub session_id: String,
    pub active_agent: String,
    pub project_branch: String,
}

/// Strict-open an existing session or first-create a new one, mirroring the
/// current `runtime::run` prologue verbatim.
pub(crate) fn open_or_create(options: &TuiOptions) -> anyhow::Result<SessionContext>;

/// Resolve and validate the named agent profile. Fails when
/// `profile.agent.name != active_agent`, same as today.
pub(crate) fn load_profile(
    options: &TuiOptions,
    active_agent: &str,
) -> anyhow::Result<lattice_api::LoadedAgentProfile>;

/// `lattice_api::stream_turn` takes `SessionApi` **by value**
/// (`turn/execute.rs:253`), so a fresh handle is opened per turn exactly as the
/// REPL does today. Do not cache one in `SessionContext`.
pub(crate) fn open_session_api() -> anyhow::Result<lattice_api::SessionApi>;

/// Moved verbatim from `runtime.rs`, together with its three existing tests.
fn required_project_branch(project_branch: Option<&str>) -> anyhow::Result<&str>;
```

### 6.2 `app.rs`

```rust
pub(crate) enum Block {
    User(String),
    /// Accumulates `TokenDelta`. A new delta appends to the trailing
    /// `Assistant` block when one exists, otherwise pushes a new block.
    Assistant(String),
    Tool { id: String, name: String, state: ToolState },
    Status(StatusKind),
}

pub(crate) enum ToolState {
    Running,
    Ok { output_len: usize },
    Failed { output_len: usize, error: Option<String> },
}

pub(crate) enum StatusKind {
    Committed { commit_id: String },
    CommitFailed { reason: String },
    TurnFailed { reason: String },
    Cancelled { reason: Option<String> },
    LlmRetry { attempt: u32, max_attempts: u32, delay_ms: u64, message: String },
    LlmFailed { attempt: u32, max_attempts: u32, message: String },
    /// `/help` output and startup banner.
    Notice(Vec<String>),
}

pub(crate) enum TurnPhase {
    Idle,
    Streaming,
    Committing,
    /// Esc / Ctrl+C pressed; `cancel_flag` set, stream still draining.
    Cancelling,
}

pub(crate) struct App {
    ctx: SessionContext,
    blocks: Vec<Block>,
    input: InputBuffer,
    history: History,
    viewport: Viewport,
    phase: TurnPhase,
    cancel_flag: Option<Arc<AtomicBool>>,
    /// Set when `supports_keyboard_enhancement()` reported true at startup.
    kitty_keyboard: bool,
    /// Instant of the last Ctrl+C, for the double-press exit rule.
    last_ctrl_c: Option<Instant>,
    should_quit: bool,
}

impl App {
    pub(crate) fn new(ctx: SessionContext, kitty_keyboard: bool) -> Self;

    /// Pure: fold one `TurnEvent` into `blocks` / `phase`. Exhaustive `match`
    /// with **no `_ =>` arm** (§9).
    pub(crate) fn apply_turn_event(&mut self, event: lattice_api::TurnEvent);

    /// Pure: fold one resolved key action. Returns `true` when a turn should be
    /// submitted with the returned prompt.
    pub(crate) fn apply_action(&mut self, action: KeyAction) -> Option<String>;

    /// Pure: `/help`, `/clear`, `/quit`, `/exit`, `/q`, `/?`. Returns
    /// `false` when the input is not a builtin and must be sent as a prompt.
    pub(crate) fn apply_builtin(&mut self, input: &str) -> bool;

    /// Number of transcript rows at width `w`, used by `Viewport`.
    pub(crate) fn content_rows(&self, w: u16) -> usize;
}

/// Terminal lifecycle + the three-arm loop. The only impure function here.
pub(crate) async fn run_app(options: TuiOptions, ctx: SessionContext) -> anyhow::Result<()>;
```

**事件循环形态（三臂 `tokio::select!`）**

```rust
// arm 1: crossterm::event::EventStream          -> Event
// arm 2: Option<TurnEventStream>                -> TurnEvent   (only while a turn runs)
// arm 3: tokio::time::interval(FRAME)           -> redraw tick
```

`FRAME = Duration::from_millis(33)`（约 30 fps 上限）。重绘只在 tick 臂发生，
其余两臂只改状态并置 `dirty`；tick 时 `dirty == false` 则跳过绘制。

**Turn 流 idle timeout（吸收 #101 A9 缺陷 1）**：arm 2 必须包在
`tokio::time::timeout(Duration::from_secs(STREAM_IDLE_TIMEOUT_SECS), stream.next())`
里。今天 `lattice-tui` 完全没有这个超时——流永远 pending 就永远挂着；
`lattice-cli/src/commands/run.rs:480` 有，TUI 没有。超时后 push
`Status(TurnFailed { reason })` 并把 `phase` 收回 `Idle`。

常量落 **`lattice-api`**：`pub const STREAM_IDLE_TIMEOUT_SECS: u64 = 300;`，紧邻
`stream_turn`。这是两个前端唯一都能向下够到的位置（`lattice-cli -> lattice-tui ->
lattice-api`）。`run.rs:35` 的 `pub(crate) const STREAM_TIMEOUT_SECS: u64 = 300`
改为**一行** re-export：

```rust
pub(crate) use lattice_api::STREAM_IDLE_TIMEOUT_SECS as STREAM_TIMEOUT_SECS;
```

`run_tests.rs:10` 那个把它钉死为 300 的测试**原样通过**，无需改动。这是 `run.rs`
在本票内唯一允许的改动；留两份 `300` 正是 A9 抱怨的漂移本身。

> 与 #118 原验收标准的偏差（有意）：原文写「并发消费三路：终端输入、UI 内部消息、
> `TurnEvent` 流」。本设计**不引入 UI 内部消息 channel**——没有第二个生产者。
> 第三臂是帧率 tick。同理不 spawn `tokio::signal::ctrl_c()` watcher：raw mode 下
> Ctrl+C 以 `KeyEvent` 送达，信号任务是多余的。

### 6.3 `input.rs`

```rust
pub(crate) struct InputBuffer {
    lines: Vec<String>,
    /// (line index, char index within that line)
    cursor: (usize, usize),
}

impl InputBuffer {
    pub(crate) fn new() -> Self;
    pub(crate) fn is_empty(&self) -> bool;
    pub(crate) fn text(&self) -> String;          // lines joined with '\n'
    pub(crate) fn set_text(&mut self, text: &str);
    pub(crate) fn clear(&mut self);
    pub(crate) fn insert_char(&mut self, c: char);
    /// Bracketed-paste payload. Inserts verbatim, newlines included, and
    /// **never** submits.
    pub(crate) fn insert_paste(&mut self, text: &str);
    pub(crate) fn newline(&mut self);
    pub(crate) fn backspace(&mut self);
    pub(crate) fn delete(&mut self);
    pub(crate) fn move_left(&mut self);
    pub(crate) fn move_right(&mut self);
    pub(crate) fn move_up(&mut self) -> bool;     // false when already on line 0
    pub(crate) fn move_down(&mut self) -> bool;   // false when already on last line
    pub(crate) fn home(&mut self);
    pub(crate) fn end(&mut self);
    pub(crate) fn rows(&self) -> usize;
}

pub(crate) struct History {
    entries: Vec<String>,
    /// `None` = not currently browsing history.
    cursor: Option<usize>,
    path: PathBuf,
}

impl History {
    /// Loads `session_dir/repl_history.txt` — same path and plain-line format
    /// the rustyline REPL used, so existing history survives the migration.
    pub(crate) fn load(session_dir: &Path) -> Self;
    pub(crate) fn push(&mut self, entry: &str);   // best-effort append + save
    pub(crate) fn prev(&mut self) -> Option<&str>;
    pub(crate) fn next(&mut self) -> Option<&str>;
    pub(crate) fn reset(&mut self);
}

pub(crate) enum KeyAction {
    Insert(char),
    Paste(String),
    Newline,
    Submit,
    Backspace,
    Delete,
    Left, Right, Home, End,
    /// Cursor up, or history-previous when already on line 0 (§8).
    Up,
    Down,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToBottom,
    /// Esc, and the first Ctrl+C.
    CancelOrClear,
    /// Second Ctrl+C within `CTRL_C_WINDOW`, or Ctrl+D on an empty buffer.
    Quit,
    Ignored,
}

pub(crate) const CTRL_C_WINDOW: Duration = Duration::from_secs(2);

/// Pure key → action resolution. `kitty` selects whether `Shift+Enter` is
/// distinguishable from `Enter` (§8).
pub(crate) fn resolve_key(
    key: crossterm::event::KeyEvent,
    kitty: bool,
    buffer_empty: bool,
) -> KeyAction;
```

### 6.4 `view.rs`

```rust
pub(crate) struct Viewport {
    /// First visible transcript row.
    scroll: usize,
    /// While true, `scroll` is pinned to the bottom on every content change.
    follow: bool,
}

impl Viewport {
    pub(crate) fn new() -> Self;                     // scroll 0, follow true
    /// Content grew to `content_rows`; viewport height is `height`.
    pub(crate) fn on_content_changed(&mut self, content_rows: usize, height: usize);
    pub(crate) fn page_up(&mut self, height: usize);
    pub(crate) fn page_down(&mut self, content_rows: usize, height: usize);
    pub(crate) fn to_bottom(&mut self, content_rows: usize, height: usize);
    pub(crate) fn scroll(&self) -> usize;
    pub(crate) fn following(&self) -> bool;
}

/// Style slots. Exactly five roles, terminal semantic colors only — no RGB.
pub(crate) enum Role { User, Assistant, ToolOk, ToolErr, DimMeta }

pub(crate) fn role_style(role: Role) -> ratatui::style::Style;

/// Wrap one block into styled lines at width `w`.
pub(crate) fn block_lines(block: &Block, w: u16) -> Vec<ratatui::text::Line<'static>>;

/// Status symbols, with an ASCII fallback when `unicode == false`.
pub(crate) fn symbol(state: SymbolKind, unicode: bool) -> &'static str;
pub(crate) enum SymbolKind { Running, Ok, Failed, Cancelled }

pub(crate) fn draw(frame: &mut ratatui::Frame, app: &App);
```

`unicode` 判据：`LANG` / `LC_ALL` 含 `UTF-8`（大小写不敏感）为 true，否则 false。
单一判据，不做终端能力探测。

---

## 7. State machines

### 7.1 `Viewport` follow (§the single most-complained-about behavior)

| 当前 `follow` | 事件 | 新 `follow` | `scroll` |
| --- | --- | --- | --- |
| true | 内容增长 | true | 钉到底部 |
| true | PageUp | **false** | `scroll -= height.saturating_sub(1)` |
| true | PageDown | true | 不变（已在底部） |
| false | 内容增长 | **false** | **不变**（视口不被拽走） |
| false | PageUp | false | `scroll -= height.saturating_sub(1)`，下限 0 |
| false | PageDown | 到底则 **true**，否则 false | `scroll += height.saturating_sub(1)`，上限 `content_rows - height` |
| any | `End` / `ScrollToBottom` | true | 钉到底部 |
| any | 内容收缩（`/clear`） | true | 0 |

「钉到底部」= `scroll = content_rows.saturating_sub(height)`。

### 7.2 Turn phase

| 当前 | 事件 | 新 |
| --- | --- | --- |
| Idle | Submit（非 builtin、非空） | Streaming |
| Streaming | `TurnCommitting` | Committing |
| Streaming / Committing | Esc / 首次 Ctrl+C | Cancelling（置 `cancel_flag`，**不中断流**） |
| Streaming / Committing / Cancelling | `TurnCommitted` / `TurnFailed` / `TurnCancelled` / `CommitFailed` | 记录 Status block，保持当前 phase |
| 任意非 Idle | 流结束（`None`） | Idle，`cancel_flag = None` |

被取消的 turn 照常 commit——`cancel_flag` 是协作取消，phase 不提前回 Idle。

### 7.3 Exit

| 状态 | 输入 | 结果 |
| --- | --- | --- |
| any | `/quit` `/exit` `/q` | 退出 |
| 输入缓冲为空 | Ctrl+D | 退出 |
| 输入缓冲非空 | Ctrl+D | 忽略 |
| `last_ctrl_c` 为空或超过 2s | Ctrl+C | 等同 Esc；状态栏提示「再按一次 Ctrl+C 退出」；记录 `last_ctrl_c` |
| `last_ctrl_c` 在 2s 内 | Ctrl+C | 退出 |

Esc 本身**永不退出**：turn 运行中取消 turn，空闲时清空输入缓冲。

---

## 8. Key table

逐字抄入 `crates/lattice-tui/AGENTS.md`。

| 键 | 条件 | 行为 |
| --- | --- | --- |
| `Enter` | — | 提交 |
| `Alt+Enter` | — | 换行 |
| `Shift+Enter` | kitty keyboard protocol 可用 | 换行 |
| 可打印字符 | — | 插入 |
| 粘贴（bracketed paste） | — | 原样插入，**含换行**，绝不提交 |
| `Backspace` / `Delete` | — | 删除 |
| `←` `→` `Home` `End` | — | 光标移动（行内） |
| `↑` | 光标不在第 0 行 | 光标上移 |
| `↑` | 光标在第 0 行 | 历史上一条 |
| `↓` | 光标不在最后一行 | 光标下移 |
| `↓` | 光标在最后一行 | 历史下一条 |
| `PageUp` / `PageDown` | — | transcript 翻页（§7.1） |
| `Ctrl+End` | — | 回到底部并恢复跟随 |
| `Esc` | turn 运行中 | 协作取消（turn 照常 commit） |
| `Esc` | 空闲 | 清空输入缓冲 |
| `Ctrl+C` | — | 同 Esc；2 秒内再按一次退出 |
| `Ctrl+D` | 缓冲为空 | 退出 |

`supports_keyboard_enhancement()` 在启动时探测一次。不可用时状态栏显示
`Alt+Enter 换行`；可用时显示 `Shift+Enter 换行`。**不静默失效。**

---

## 9. `TurnEvent` → transcript mapping

穷举 `match`，**无 `_ =>` 兜底**——#115 / #139-B 新增的变体必须在编译期打红。
现有 `runtime.rs` 已经是穷举的，本设计保持该性质。

| `TurnEvent` | 动作 |
| --- | --- |
| `TokenDelta { content }` | 追加到尾部 `Assistant` block，无则新建 |
| `ToolCallStarted { id, name }` | push `Block::Tool { id, name, state: Running }` |
| `ToolCallFinished { id, name, ok, output_len, error_summary }` | 按 `id` 找到该 Tool block，置 `Ok` / `Failed`。**只读这五个字段；零个按 `name` 的分支。** |
| `TurnCommitting` | `phase = Committing` |
| `TurnCommitted { commit_id, .. }` | push `Status(Committed)` |
| `CommitFailed { reason }` | push `Status(CommitFailed)` |
| `TurnFailed { reason }` | push `Status(TurnFailed)` |
| `TurnCancelled { reason }` | push `Status(Cancelled)` |
| `LlmRequestError { attempt, max_attempts, will_retry, delay_ms, message, .. }` | `will_retry` → `Status(LlmRetry)`，否则 `Status(LlmFailed)` |
| `TurnStarted` / `ModelStarted` / `ModelFinished` / `ReasoningDelta` | 静默（与今日 REPL 一致；reasoning 折叠是 v2） |
| `CepCallStarted` / `CepCallFinished` | 静默（deprecated，随 #34 删除） |

Tool block 的通用状态行形态：

```text
  · observe.workspace                 (running)
  ✓ observe.workspace  1234 chars
  ✗ mutate.edit        0 chars  precondition failed
```

**禁止**任何 `match name { "observe.workspace" => ... }`。出现一个就是建了
ADR 0004 §2 禁止的 adapter-specific 分派表，#149 就从「接线」退化成「拆掉再接」。

---

## 10. Terminal lifecycle

```rust
let mut terminal = ratatui::try_init()?;              // 已含 panic hook 恢复
let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
if kitty {
    crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    )?;
}
// ... run loop ...
if kitty { crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags)?; }
crossterm::execute!(std::io::stdout(), DisableBracketedPaste)?;
ratatui::restore();
```

`ratatui::try_init()` 内部调 `set_panic_hook()`，恢复 alternate screen 与 raw
mode，但**不**关闭 bracketed paste、不 pop keyboard flags——panic 后用户 shell
会残留这两个状态。因此在 `try_init()` 之后再链一层 panic hook，额外执行
`DisableBracketedPaste` 与（当 `kitty` 为真时）`PopKeyboardEnhancementFlags`，
然后调用前一个 hook。约 10 行，属于「防止终端损坏」，不在克制清单的裁剪范围内。

**不新建 terminal 生命周期模块**——以上全部内联在 `app::run_app` 里。

---

## 11. Tests

上限不含 `#[cfg(test)]`。全部为纯状态机测试，**无渲染快照**。

`input.rs`

- `paste_with_newlines_never_submits`：`insert_paste("a\nb\nc")` 后缓冲为三行，
  且 `resolve_key` 未产生 `Submit`。
- `enter_submits_alt_enter_newlines`
- `shift_enter_newlines_only_with_kitty`：`kitty = false` 时 `Shift+Enter` 解析为
  `Submit`（终端本就无法区分），`kitty = true` 时为 `Newline`。
- `up_at_line_zero_browses_history_else_moves_cursor`
- `history_roundtrip_persists_to_repl_history_txt`

`view.rs`

- `follow_pins_to_bottom_on_growth`
- `page_up_breaks_follow_and_growth_does_not_move_viewport` ← 「流式 token 不得
  在用户上滚阅读时把视口拽走」的守卫测试。
- `page_down_to_bottom_restores_follow`
- `clear_resets_scroll_and_follow`
- `symbol_falls_back_to_ascii_without_utf8_locale`

`app.rs`

- `token_deltas_coalesce_into_one_assistant_block`
- `tool_finished_updates_the_block_with_matching_id`
- `esc_during_turn_sets_cancel_flag_and_does_not_end_phase`
- `esc_when_idle_clears_input`
- `double_ctrl_c_within_window_quits_single_does_not`
- `ctrl_d_quits_only_on_empty_buffer`
- `builtin_clear_empties_blocks_and_resets_viewport`
- `idle_stream_times_out_and_returns_to_idle`：流永远 pending 时按
  `STREAM_IDLE_TIMEOUT_SECS` 超时，push `TurnFailed` 并回到 `Idle`（#101 A9 缺陷 1）。

`session.rs`

- `required_project_branch` 的三个既有测试**原样保留**。

唯一允许的渲染断言：一个 `ratatui::backend::TestBackend` 冒烟，确认三个区域在
24×80 下都被绘制且不 panic。不断言具体字符位置。

---

## 12. PTY harness migration (必须在同一个 PR 内)

`crates/lattice-system-harness/tests/pty_tui_smoke.rs` 跑在普通 CI lane，不改必红。

`crates/lattice-system-harness/src/pty_tui.rs`：

1. `wait_for_prompt` 现为 `self.wait_for(timeout, |s| s.contains('>'))`，匹配的是
   **ANSI 剥离后**的文本。全屏应用不打印提示符，该判据必挂。
   新增 `wait_for_raw(&mut self, timeout, predicate: impl Fn(&[u8]) -> bool)`，
   在**未剥离**的原始字节上匹配；`wait_for_prompt` 重命名为 `wait_for_ready`，
   实现为等待 alternate screen 进入序列 `\x1b[?1049h`。
   这是终端协议事实，不是呈现细节，符合 RFC #41 决策三「driver 内做就绪识别、
   scenario 不断言显示细节」的原意，且比 `contains('>')` 更严格。
2. `READ_BUDGET` 由 `256 * 1024` 提升至 `4 * 1024 * 1024`。全屏应用按帧重绘，
   字节数不再正比于内容量。注释同步更新，说明预算含义已变。
3. `send_line` 追加 `\n` → 在新 TUI 中即 `Enter` → 提交。**无需改动。**
4. scenario 侧的文本断言经 `strip_ansi` 后落在 24×80 的帧里，短文本仍在一行内。
   若 fake LLM 响应长到会被折行，在 driver 的匹配前把连续空白归一为单个空格
   （归一逻辑留在 driver 内，scenario 不感知）。

---

## 12.5 #101 A9 的三个缺陷：两条被本票吸收，一条必须显式补

A9 的三个缺陷全部落在 `runtime.rs`，而本票删除该文件。若不显式处理，删除会把缺陷
连同修复机会一起抹掉，A9 之后再回来时已无对应代码位置。

| A9 缺陷 | 本票处理 |
|---|---|
| 1. **idle timeout** —— `stream.next()` 无超时 | **必须显式补**（§6.2）。设计原稿漏了这条。 |
| 2. **`error_summary` 不显示** —— 只有 `✗ name (N chars)` | 已吸收：`ToolState::Failed { output_len, error }` 承载 `error_summary`（§6.2 / §9）。 |
| 3. **Ctrl-C 裸窗口** —— watcher 起得太晚、`cancel_task.abort()` 位于多个 `?` 之后、第二次 Ctrl-C 无反馈 | 已吸收且更彻底：不再 spawn watcher，Ctrl-C 是 raw mode 下的 `KeyEvent`，从第一帧起就是活的，不存在裸窗口，也没有需要 Drop guard 的 task；第二次 Ctrl-C 的可见反馈即 §7.3 的双击退出规则。 |

本票落地后，A9 在 #101 中应标为「已由 #118 吸收」并关闭。

---

## 13. Issue bookkeeping (随本票一并提交)

- #101 的 A9 条目：标注三个缺陷已由本票吸收（缺陷 1 显式补，缺陷 2/3 随重写消失），
  可关闭。
- #101 的 C5 条目：删去 `render/` 落点与 sink 抽象部分，保留 `StreamCaps::resolve`
  真值表、`--color` / `colored 3.1` 判据修正、mid-token 换行两份实现去重三件 CLI
  事；「代码面冲突」节中 `crates/lattice-tui/src/runtime.rs` 一条改为指向本票
  （`runtime.rs` 已删除）；C5 的边界前置条件标注为「已由 #118 第一个 commit 解决」。
- #116 正文：`render/` 落点写死为 `crates/lattice-tui/src/render/`（模块，非新
  crate），补「取代 #101 C5 的 `render/` 部分」，行数上限 ≤800。
- #118 正文：按 Round 1–3 的答案重写（删去「保留 REPL 全部行为面」与「会话列表 /
  新建 / 打开」两条，加入 v1 克制清单、键位表、PTY 迁移项）。

---

## 14. Anti-drift checklist（评审时逐条对）

- [ ] `crates/lattice-tui/src/` 恰好 5 个文件，非测试代码 ≤1200 行。
- [ ] `Cargo.toml` 无新增 workspace 依赖；`colored` / `rustyline` 已移除。
- [ ] 全仓搜索 `match .*name` 在 `lattice-tui` 内零命中按工具名的分派。
- [ ] `TurnEvent` 的 `match` 无 `_ =>` 臂。
- [ ] 无 `Block` / `Borders` / box-drawing 边框绘制。
- [ ] 颜色只用 `ratatui::style::Color` 的具名终端色，无 `Color::Rgb`。
- [ ] 无鼠标事件处理（`EnableMouseCapture` 零命中）。
- [ ] 无 v1 克制清单上的任何一项。
- [ ] `lattice-cli/src/main.rs` 未改。
- [ ] `lattice-cli/src/commands/run.rs` 只有 re-export 一行改动；`run_tests.rs` 未改。
- [ ] turn 流有 idle timeout，常量单一来源为 `lattice_api::STREAM_IDLE_TIMEOUT_SECS`
      （全仓不得有第二个字面量 `300` 充当该超时）。
- [ ] `pty_tui_smoke` 通过。
- [ ] `env -u LATTICE_BIN cargo xtask ci` 通过；`FIX.md` 记录。
