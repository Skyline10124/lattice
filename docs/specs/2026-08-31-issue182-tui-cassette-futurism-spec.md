# lattice TUI 设计 spec

定稿日期：2026-08-28
状态：当前可做（细节 + 本轮视觉）
配色源：Cassette Futurism v2
数字来源：2026-08-28 细节锁定 + 2026-08-31 讨论复核

这份 spec 只写已经定死的东西。现码仍是 #118 底栏 + 输入 1..=6 行，那是现状，不是本 spec。

---

## 1. 范围

本轮做：

- 左栏对话（transcript 上、输入下）+ 右栏 status
- 说话人独立成行、回合之间空一行、输入提示符只在第一行
- 输入区（2..=5 行自适应）、输入下单独一行 `PROMPT`、左栏分割线
- 右栏背景填充 `navy-11`；宽终端 `status_w = clamp(frame_w/8, 16, 24)`
- tool 块默认折叠（Ok 收、Failed 展）；折叠符是状态指示，不可交互
- 五种前景色
- 不画边框、不加区域、不增 Role 枚举
- 窄屏（`frame_w < 56`）：右栏取消，`PROMPT` tag 与 status 合并为底栏一行，各自保留配色

本轮不做：

- `#116` 卡片、`#149`
- `SLASH` / `BASH` / `CONFIG` 及更多 mode tag（结构位留着，视觉和交互都不做）
- 第六种前景色、整套 RGB 灌进终端
- `amber` 进 TUI
- logo / mark
- 为折叠加新键或鼠标处理
- 浅色终端适配、主题系统、响应式 UI
- 给已有 UI 文案改名（`[scroll]` 沿用现码）

---

## 2. 布局尺寸

单位：列（cell），不是像素。`frame_w` 是终端可用列宽。`frame_w / 8` 按整数除法。

```
frame_w
├── left_w = frame_w - status_w - gap
│     ├── gutter = 2          折叠符预留，不含正文
│     └── content_w = left_w - gutter   不设上限
├── gap = 1                   空列，不画线
└── status_w = clamp(frame_w/8, 16, 24)  全高右栏
```

| 量 | 值 |
| --- | --- |
| `status_w` | `clamp(frame_w/8, 16, 24)` |
| `gap` | 1 |
| `gutter` | 2 |
| `left_w` | `frame_w - status_w - gap` |
| `content_w` | `left_w - gutter`（不设上限） |
| 窄屏 | `frame_w < 56`：取消右栏，`PROMPT` tag 与 status 合并为底栏一行 |

80 列时：`status_w = 16`，`left_w = 63`，`content_w = 61`。176 列时：`status_w = 22`，`left_w = 153`，`content_w = 151`。

右栏只放界面状态信息。1 列内边距（字不贴 gap），可用宽 = `status_w - 1`。`session_id` / `agent` 超可用宽截断。不画边框。卡片以后落左栏。窄终端右栏不加宽；宽终端由上面的公式自适应。

`Role` 五枚举不增：layout 从竖切改横切，枚举不用加。

### 窄屏底栏

左栏结构不变（transcript / 分割线 / 输入）。底栏只占一行：

- 左侧 `PROMPT` tag：iris-9 底 + cream-1 字，贴 gutter 之后
- 右侧 status 段：navy-11 底 + DimMeta 字，填满余宽
- 各自保留配色，互不串色
- status 段从左到右：`session_id · agent · phase · [scroll]? · Alt+Enter 换行`（kitty 时键位为 `S-Enter 换行`）
- 放不下时先丢键位提示，再截断 `session_id`；`phase` 与 `[scroll]` 永远保留
- `stream` 中 phase 字走 User；`cancel` phase 走 ToolErr——与右栏同一套着色

---

## 3. 左栏结构（从上到下）

宽屏：

```
transcript          不填背景，透出终端底色
────────────        左栏分割线，不到右栏
>  prompt           高 2..=5 行；`>` 只在第一行
   prompt           续行无前缀，与第一行正文对齐
PROMPT              6 字符色块，不是方括号；不占输入高度
```

窄屏底栏把最后一行换成 `PROMPT` + status 段，见 §2。

### Transcript

说话人单独一行：用户 `你`（User / iris-7），助手用当前 agent 名（Assistant / cream-2）。不当每行前缀。正文从下一行起，wrap 在 `content_w`。回合之间空一行当分隔。工具行留在助手回合内，不再加空行。左栏不填背景；透出来的是终端底色，视觉上等于 `navy-12` `#1c293e`。

```
你
帮我看下 session.rs 里的重开逻辑

assistant
我先读 pending 模块和重开入口。
▸ ✓ observe.workspace · read_file · 1,204 chars
▾ ✗ mutate.edit · replace_with_expected
  expected_hash mismatch（stale read）

你
改成先校验再重开
```

### 分割线

- 位置：transcript 与输入区之间
- 只画左栏，不穿 gap、不进右栏
- 色：`DimMeta`（`warm-gray-8`）

### Prompt

- 高 2..=5 行自适应：最少 2 行，第二行空着也留着；随内容撑到 5 行；发送/清空后收回 2 行；超 5 行区内滚动；bracketed paste 一次到位
- 前缀 `>`（大于号 + 空格）只在第一行；续行无前缀，与第一行正文对齐
- `>` 走 User；已输入文本走 Assistant / cream-2
- 空：两行空白 + 光标
- 块光标为 `cream-2` 实心块
- 输入区不含 status tag

### Status tag

- 宽屏：贴在输入底下，单独 1 行，不占输入高度
- 窄屏：与 status 合并为底栏一行，见 §2
- 文案只有 `PROMPT`：全大写、无点号、无方括号——6 个字符的色块
- 空输入和正常打提示词同形：都显示 `PROMPT`，不换 tag、不加第二模式
- 填 `iris-9`，字 `cream-1` `#fbfaf8`
- 左对齐，贴左栏内容起始（gutter 之后）
- `SLASH` / `BASH` / `CONFIG` 的结构位保留，本轮不实现、不画

---

## 4. 右栏

- 全高 `status_w` 列，背景填充 `navy-11`
- 1 列内边距，可用宽 = `status_w - 1`，字不要贴 gap
- 字 `DimMeta`；`stream` 中 phase 走 User；`cancel` phase 走 ToolErr

自上而下各占一行，不横排：

1. `session_id`（超可用宽截断）
2. `agent`（超可用宽截断）
3. 空行
4. `phase`：短 ASCII，`idle` | `stream` | `commit` | `cancel`
5. 空行
6. 换行键：`Alt+Enter`，下一行「换行」；kitty 用 `S-Enter`
7. 未跟随时：`[scroll]`（沿用现码措辞，不改名）
8. 双击退出提示

Status 在右栏顶，`tip.key` 在右栏底。具体用词可按终端改（kitty），结构不要改。

---

## 5. 细节：说话人和 tool

gutter 两列预留折叠符，**不加新键、不做鼠标处理**。折叠符是状态指示，不是控件：Ok 收起后不可展开。可折叠块才画符；实现用 `▸` / `▾`，ASCII 回退 `>` / `v`，不要把 ASCII 回退当成品字形。

默认按本表：Ok 收 / Failed 展。

工具行只读通用字段：`id` / `name` / `ok` / `output_len` / `error_summary`。零按工具名分支。

| 块 | 默认 |
| --- | --- |
| tool Ok | 一行收起 |
| tool Failed | 展开，露出 `error_summary` |
| 进行中 | 一行，`DimMeta`（`running ·`） |

Ok 用 `ToolOk` 勾。Failed 的 `✗`、`error_summary`、`TurnFailed`、cancel phase 走 `ToolErr`。其余 meta（工具名、字符数、hash）走 `DimMeta`。

---

## 6. 色

终端不灌 Cassette 全表。前景只许五种；背景填充只许下面两处。左栏不填背景。承诺深色终端主题。

### 五种前景色

| 角色 | 终端命名色 | Cassette token | hex | 用途 |
| --- | --- | --- | --- | --- |
| User | LightBlue | iris-7 | `#8695e3` | 说话人 `你`、提示符 `>`、stream 中 phase |
| Assistant | White | cream-2 | `#f5f3ef` | agent 名、正文、输入文本 |
| ToolOk | Cyan | cyan-7 | `#2ab0b0` | `✓` |
| ToolErr | Red | signal-red-9 | `#cb2b31` | `✗`、TurnFailed、cancel phase |
| DimMeta | DarkGray | warm-gray-8 | `#857f78` | 右栏字、回合间空行、分割线、running ·、hash、键位 |

### 两处背景填充

| 表面 | token | hex | 字 |
| --- | --- | --- | --- |
| 右栏 bg（窄屏 status 段同此） | navy-11 | `#2c4267` | DimMeta |
| `PROMPT` tag bg | iris-9 | `#5965ca` | cream-1 `#fbfaf8` |

truecolor 用 hex；否则就近命名色。不要为了这两处把整套 8×12 塞进终端。

终端底（transcript 透出）=`navy-12` `#1c293e`。它不是填出来的色，是默认底。

### 禁止

- `amber` 进 TUI
- 第六种前景色
- 给左栏 / transcript 填背景
- 把 Cassette 其他阶当 TUI token 用
- 浅色终端适配、主题系统、响应式 UI
- 把 `[scroll]` 改成别的文案

