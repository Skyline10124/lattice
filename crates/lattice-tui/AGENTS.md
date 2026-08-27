# AGENTS.md

Crate-local guidance for `lattice-tui`.

## Role

This crate owns the full-screen TUI shell for LATTICE. It enters an alternate
screen via `ratatui`/`crossterm`, executes turns through
`lattice_api::stream_turn`, and renders a three-region layout: transcript
(flex), input (1..=6 rows), status (1 row). The forthcoming transcript card
renderer (#116) lands here as the `render/` module (not a new crate);
`lattice-cli` consumes it through the existing `lattice-cli -> lattice-tui`
edge. Do not create `render/` in this crate until #116.

## Boundaries

- Keep runtime calls through public `lattice-api` types.
- Keep CLI argument parsing in `lattice-cli`; the CLI calls `lattice_tui::run`.
- Surface missing session/profile/provider problems before the TUI starts.
- REPL history persists to `<session_dir>/repl_history.txt` (best effort,
  same path and plain-line format as the retired rustyline REPL).
- Cooperative turn cancellation uses `RunTurnRequest::cancel_flag` set from
  Esc / first Ctrl+C in raw mode. Do not spawn a `tokio::signal::ctrl_c`
  watcher; Ctrl+C arrives as a `KeyEvent`.
- Shared transcript rendering sinks into this crate because `lattice-tui`
  cannot depend on `lattice-cli`.

## Source layout

- `lib.rs` — public surface (`TuiOptions`, `run`) and blocking-pool Component warmup
- `session.rs` — session open/create, profile load, `SessionApi` per turn
- `app.rs` — three-arm event loop, `App` state, `TurnEvent` → `Block`
- `input.rs` — `InputBuffer`, `History`, key dispatch
- `view.rs` — layout, transcript wrapping, `Viewport`, status line

Exactly these five files. Do not add `render/` (that is #116).

## 键位表

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

## 不复刻清单

Do not port the following from other TUI chat clients:

- 主题系统 / 主题选择器
- shell 模式（`!` 前缀）
- 会话分享
- 插件槽位与 JS 运行时
- realtime / 语音
- 多 agent side thread

## v1 克制清单

Three regions, nothing else:

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

Explicitly NOT in v1:

- 会话选择器 UI（列表 / 搜索 / 预览）。`TuiOptions.session_id` 已经承担会话选择：
  给了就 open，不给就 create。v1 只操作当前会话。
- 鼠标，含滚轮。仅键盘。
- 语法高亮、markdown 渲染。transcript 是纯文本。
- reasoning 折叠 / usage-token 面板 / 搜索 / 命令面板 / 主题。
- 多于三个的界面区域；弹层、模态、toast。
- Tool call 卡片。v1 只有通用状态行（§9）。
- 会话分支 / 时间线视图。

## Verification

- Local: `cargo test -p lattice-tui`.
- PTY smoke lives in `lattice-system-harness` (`pty_tui_smoke`); after TUI
  changes, rebuild `lattice-cli` and run that harness with
  `LATTICE_BIN` pointing at the just-built binary.
- CLI surface changes should also run `cargo test -p lattice-cli`.
