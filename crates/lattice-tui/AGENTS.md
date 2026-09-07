# AGENTS.md

`lattice-tui` 的 crate 级 agent 指引。

## 角色

本 crate 拥有 LATTICE 的全屏 TUI shell。它经 `ratatui`/`crossterm` 进入备用屏幕，
通过 `lattice_api::stream_turn` 执行 turn，并在真正超宽的横向终端上渲染左侧
对话列（对话记录、统一停靠区）与固定右侧栏。#116 的对话记录卡片渲染器
将落在本 crate 的 `render/` 模块（不是新 crate）；`lattice-cli` 通过既有的
`lattice-cli -> lattice-tui` 边消费它。#116 之前不得在本 crate 创建 `render/`。

## 边界

- 运行时调用一律走公开的 `lattice-api` 类型。
- CLI 参数解析留在 `lattice-cli`；CLI 调 `lattice_tui::run`。
- 会话 / profile / provider 问题必须在 TUI 启动前暴露，不允许静默失败。
- REPL 历史持久化到 `<session_dir>/repl_history.txt`（尽力而为，与已退役的
  rustyline REPL 同路径、同纯文本行格式）。
- 协作式 turn 取消通过 `RunTurnRequest::cancel_flag`，由 raw 模式下的
  Esc / 第一次 Ctrl+C 置位。不要另起 `tokio::signal::ctrl_c` watcher；
  Ctrl+C 以 `KeyEvent` 形态到达。
- 共享对话记录渲染下沉到本 crate，因为 `lattice-tui` 不能依赖 `lattice-cli`。

## 源码布局

- `lib.rs` — 公开面（`TuiOptions`、`run`）与阻塞池 Component 预热
- `session.rs` — 会话打开/创建、profile 加载、每 turn 的 `SessionApi`
- `app.rs` — 三臂事件循环、`App` 状态、`TurnEvent` → `Block`
- `input.rs` — `InputBuffer`、`History`、按键分发
- `view.rs` — 布局、对话记录折行、`Viewport`、统一停靠区、底部状态行

固定这五个文件。#116 之前不得新增 `render/`。

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
| `PageUp` / `PageDown` | — | 对话记录翻页（§7.1） |
| `Ctrl+End` | — | 回到底部并恢复跟随 |
| `Space`（无修饰） | 停靠区显示 WARN / ERROR | 释放状态回对话记录并回到 PROMPT；Space 被消费，不进入输入缓冲 |
| `Esc` | turn 运行中 | 协作取消（turn 照常 commit） |
| `Esc` | 空闲 | 清空输入缓冲 |
| `Ctrl+C` | — | 同 Esc；2 秒内再按一次退出 |
| `Ctrl+D` | 缓冲为空 | 退出 |

停靠区显示 WARN / ERROR 时，普通输入、编辑和提交被屏蔽（不得修改不可见的
InputBuffer）；PageUp / PageDown、Ctrl+C、Ctrl+D 等全局动作照常。WARN /
ERROR 不会自动消失，也没有定时回退。

`supports_keyboard_enhancement()` 启动时探测一次。状态区无 kitty 协议时显示
`Alt+Enter newline`，有则显示 `S-Enter newline`。首次 Ctrl+C 后的两秒退出
提示是 `Ctrl+C to quit`。不允许静默失败。

## 不复刻清单

不要从其他 TUI 聊天客户端复刻以下内容：

- 主题系统 / 主题选择器
- shell 模式（`!` 前缀）
- 会话分享
- 插件槽位与 JS 运行时
- realtime / 语音
- 多 agent side thread

## v1 克制清单

左栏对话列（对话记录 + 统一停靠区），真正超宽的横向终端上另有固定右侧栏，
除此之外无他：

```text
侧栏可见（真正超宽终端）
┌──────────────────────────────────────────┬─┬────────────────┐
│ transcript                               │ │ sidebar        │
│                                          │ │ session        │
│                                          │ │ agent          │
│                                          │ │ phase          │
├── PROMPT ────────────────────────────────┤ │                │
│ > input                       (1..=5)   │ │                │
└──────────────────────────────────────────┴─┴────────────────┘

侧栏隐藏
┌───────────────────────────────────────────────────────────────┐
│ transcript                                                    │
├── ERROR ──────────────────────────── SPACE → PROMPT ──────────┤
│ permission denied                                             │
├───────────────────────────────────────────────────────────────┤
│ session · agent · idle · Alt+Enter newline                    │
└───────────────────────────────────────────────────────────────┘
```

停靠区是左侧底部的统一组件，只有 `PROMPT` / `WARN` / `ERROR` 三个互斥状态；
分隔轨下的输入或状态正文从 1 行撑到 5 行，超 5 行时 Prompt 内部滚动、Warn /
Error 显示前 5 行。`INFO` 类状态不进停靠区，直接进对话记录。底部状态行
只在侧栏隐藏时存在，用来承接 session、agent、phase、scroll 与快捷键提示，
它不是停靠区的一部分。不画边框；图示仅表示几何关系。

v1 明确不做：

- 会话选择器 UI（列表 / 搜索 / 预览）。`TuiOptions.session_id` 已经承担会话
  选择：给了就 open，不给就 create。v1 只操作当前会话。
- 鼠标，含滚轮。仅键盘。
- 语法高亮、markdown 渲染。对话记录是纯文本。
- reasoning 折叠 / usage-token 面板 / 搜索 / 命令面板 / 主题。
- 多于三个的界面区域；弹层、模态、toast。
- Tool call 卡片。v1 只有通用状态行（§9）。
- 会话分支 / 时间线视图。

## 布局锁

所有尺寸单位是终端 cell。整数运算；所有减法 saturating（极小终端不得
panic、不得下溢）。

| 量 | 值 |
| --- | --- |
| `SIDEBAR_W` | 40（固定，不再随宽度变化） |
| `gap` | 1 |
| `gutter` | 2 |
| 侧栏可见条件 | `frame_w >= frame_h * 3` 且 `frame_w - 40 - 1 - 2 >= 72` 同时成立 |
| 侧栏隐藏 | `left_w = frame_w`，`content_w = frame_w - gutter`，出现独立底部状态行 |
| 侧栏可见时 | `left_w = frame_w - 40 - gap`，`content_w = left_w - gutter`，无底部状态行 |
| 停靠区总高 | 1 行分隔轨 + 1..=5 行内容 = 2..=6 行；`Dock::height()` 是唯一来源 |
| 对话记录高度 | `frame_h - (dock_h + bottom_h)`，`bottom_h` 侧栏隐藏时为 1、可见时为 0 |

gutter 预留两列，不计入正文。gap 是空列。右侧栏内容有一列左边距。渲染、
viewport 行数统计、输入折行、光标定位必须使用同一个 `content_w`。`176 × 50`
时侧栏可见：`left_w / content_w` 为 `135 / 133`。`115 × 47`、`85 × 47`、
`56 × 47`（比例不足 3:1）与 `80 × 24`（分栏后正文仅 37 列）侧栏隐藏。

## 配色锁

TUI 承诺深色终端主题。真彩色终端使用表中 hex 值；其他终端使用命名色
回退。不探测 terminfo 或 256 色支持。

| 前景角色 | Cassette token | Hex | 命名色回退 |
| --- | --- | --- | --- |
| `User` | iris-7 | `#8695e3` | `LightBlue` |
| `Assistant` | cream-2 | `#f5f3ef` | `White` |
| `ToolOk` | cyan-7 | `#2ab0b0` | `Cyan` |
| `ToolErr` | signal-red-9 | `#cb2b31` | `Red` |
| `DimMeta` | warm-gray-8 | `#857f78` | `DarkGray` |

| 填充面 | Cassette token | Hex | 命名色回退 | 字 |
| --- | --- | --- | --- | --- |
| 右侧栏 bg | navy-11 | `#2c4267` | `Blue` | `DimMeta` |
| `PROMPT` tag | iris-9 | `#5965ca` | `LightBlue` | `Assistant` |
| `WARN` tag | amber-9 | `#9f6200` | `Yellow` | `Assistant` |
| `ERROR` tag | signal-red-9 | `#cb2b31` | `Red` | `Assistant` |

`signal-red-9` 同时作为 `ToolErr` 前景色与 `ERROR` tag 填充色，hex 不变。
amber 只解禁 `WARN` tag 这一处填充色，其他区域仍然禁止。配色源收录在仓库
`docs/reference/cassette-futurism-palette.md`（8 hues × 12 steps，step 9 为 `solid`，
是填充面应取的阶）。不新增第六种前景角色、左栏背景或主题抽象。

## 折叠锁

`▸` / `▾`（ASCII `>` / `v`）是状态指示，不是控件。不加按键或鼠标处理。
成功的 tool 保持折叠一行；失败的 tool 保持展开并露出错误摘要；运行中的
tool 保持一行。

## 验证

- 本地：`cargo test -p lattice-tui`。
- PTY 冒烟验证在 `lattice-system-harness`（`pty_tui_smoke`）；TUI 改动后，
  重建 `lattice-cli`，用 `LATTICE_BIN` 指向刚构建的二进制跑该 harness。
- CLI 面变化还应跑 `cargo test -p lattice-cli`。
