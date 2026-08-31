# AGENTS.md

Crate-local guidance for `lattice-tui`.

## Role

This crate owns the full-screen TUI shell for LATTICE. It enters an alternate
screen via `ratatui`/`crossterm`, executes turns through
`lattice_api::stream_turn`, and renders a left conversation column
(transcript, separator, 2..=5-row input, bottom line) beside a right status
column. The forthcoming transcript card renderer (#116) lands here as the
`render/` module (not a new crate); `lattice-cli` consumes it through the
existing `lattice-cli -> lattice-tui` edge. Do not create `render/` in this
crate until #116.

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

Left conversation column plus right status column, nothing else:

```text
┌──────────────────────────────────────┬─┬────────────────┐
│ transcript                           │ │ status         │
│                                      │ │                │
├──────────────────────────────────────┤ │                │
│ > input                     (2..=5) │ │                │
│                                      │ │                │
│  PROMPT                              │ │                │
└──────────────────────────────────────┴─┴────────────────┘
```

`transcript` takes the remaining height. The input grows from 2 to 5 rows,
then scrolls internally. The bottom line is always one row. Wide frames render
the `PROMPT` tag there and use the full-height right status column. Frames
narrower than 56 columns remove the right column and merge the tag and status
into the bottom line. Do not draw borders; the diagram shows geometry only.

Explicitly NOT in v1:

- 会话选择器 UI（列表 / 搜索 / 预览）。`TuiOptions.session_id` 已经承担会话选择：
  给了就 open，不给就 create。v1 只操作当前会话。
- 鼠标，含滚轮。仅键盘。
- 语法高亮、markdown 渲染。transcript 是纯文本。
- reasoning 折叠 / usage-token 面板 / 搜索 / 命令面板 / 主题。
- 多于三个的界面区域；弹层、模态、toast。
- Tool call 卡片。v1 只有通用状态行（§9）。
- 会话分支 / 时间线视图。

## Layout lock

All dimensions are terminal cells. Integer division applies.

| Quantity | Value |
| --- | --- |
| `status_w` | `clamp(frame_w / 8, 16, 24)` |
| `gap` | 1 |
| `gutter` | 2 |
| `left_w` | `frame_w - status_w - gap` |
| `content_w` | `left_w - gutter`, with no upper bound |
| narrow frame | `frame_w < 56`; no right column |
| transcript height | `frame_h - (input_h + 2)`, saturating at 0 |

The gutter reserves two cells for fold-state symbols and is not body width. The
gap is blank. Wide status content has one cell of left padding. Rendering,
viewport row counts, input wrapping, and cursor placement must all use the same
`content_w`. At 80 columns, `status_w / left_w / content_w` is `16 / 63 / 61`;
at 176 columns it is `22 / 153 / 151`.

## Color lock

The TUI supports dark terminal themes. Truecolor terminals use the listed hex
values; other terminals use the named-color fallback. Do not probe terminfo or
256-color support.

| Foreground role | Cassette token | Hex | Named fallback |
| --- | --- | --- | --- |
| `User` | iris-7 | `#8695e3` | `LightBlue` |
| `Assistant` | cream-2 | `#f5f3ef` | `White` |
| `ToolOk` | cyan-7 | `#2ab0b0` | `Cyan` |
| `ToolErr` | signal-red-9 | `#cb2b31` | `Red` |
| `DimMeta` | warm-gray-8 | `#857f78` | `DarkGray` |

| Filled surface | Cassette token | Hex | Named fallback | Text |
| --- | --- | --- | --- | --- |
| right status / narrow status segment | navy-11 | `#2c4267` | `Blue` | `DimMeta` |
| `PROMPT` tag | iris-9 | `#5965ca` | `LightBlue` | `Assistant` |

Do not add amber, a sixth foreground role, a left-column background, or a theme
abstraction.

## Tool collapse lock

`▸` / `▾` (ASCII `>` / `v`) indicate state; they are not controls. Do not add
keys or mouse handling. Successful tools stay collapsed on one line. Failed
tools stay expanded with their error summary. Running tools stay on one line.

## Verification

- Local: `cargo test -p lattice-tui`.
- PTY smoke lives in `lattice-system-harness` (`pty_tui_smoke`); after TUI
  changes, rebuild `lattice-cli` and run that harness with
  `LATTICE_BIN` pointing at the just-built binary.
- CLI surface changes should also run `cargo test -p lattice-cli`.
