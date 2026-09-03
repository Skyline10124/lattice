# lattice TUI 设计 spec

定稿日期：2026-08-28
实现状态：已由 PR #184 落地；2026-09-03 侧栏与停靠区重构（`docs/plans/2026-09-03-tui-sidebar-dock-plan.md`）落地后同步更新本文。
配色源：仓库内 `docs/cassette-futurism-palette.jsx`（8 hues × 12 steps，OKLCH 生成）
数字来源：2026-08-28 细节锁定 + 2026-08-31 讨论复核 + 2026-08-31 实现前对码复核

这份 spec 记录已经定死并落地的 #182 行为。

---

## 1. 范围

本轮做（2026-09-03 修订后的现状）：

- 左栏对话（对话记录上、统一停靠区下）+ 固定 40 列右侧栏
- 侧栏只在 `frame_w >= frame_h × 3` 且分栏后正文 `>= 72` 列时显示；无动态宽度、无多布局条件
- 说话人独立成行、回合之间空一行、输入提示符只在第一行
- 统一停靠区：`PROMPT` / `WARN` / `ERROR` 三个互斥状态；分隔轨替代旧分割线与独立 Prompt tag 行；`SPACE → PROMPT` 提示只在 WARN / ERROR 出现
- 停靠区内容 1..=5 行；`INFO` 不进停靠区，直接进对话记录
- 右侧栏背景填充 `navy-11`；tool 块默认折叠（Ok 收、Failed 展）；折叠符是状态指示，不可交互
- 五种前景色
- 不画边框、不加区域、不增 Role 枚举
- 侧栏隐藏时出现独立底部状态行（session · agent · phase · 键位），整宽给状态文本，不带 tag

本轮不做：

- `#116` 卡片、`#149`
- `SLASH` / `BASH` / `CONFIG` 及更多 mode tag（结构位留着，视觉和交互都不做）
- 第六种前景色、整套 RGB 灌进终端
- amber 扩散到 `WARN` tag 之外的任何区域（tag 填充色本身已解禁，见 §6）
- logo / mark
- 为折叠加新键或鼠标处理
- 浅色终端适配、主题系统、响应式 UI
- 给已有 UI 文案改名（`[scroll]` 沿用现码）
- 自绘软光标、光标形状/颜色控制
- 嵌套 Turn 数据模型、`render/` 模块、Theme/Palette trait、新依赖

---

## 2. 布局尺寸

单位：列（cell），不是像素。`frame_w` 是终端可用列宽。侧栏固定 40 列，
不再按 `frame_w / 8` 自适应。

```text
frame_w
├── left_w = frame_w - SIDEBAR_W - gap      侧栏可见时
│     ├── gutter = 2          折叠符预留，不含正文
│     └── content_w = left_w - gutter   不设上限
├── gap = 1                   空列，不画线
└── SIDEBAR_W = 40            全高右侧栏，固定值
```

| 量 | 值 |
| --- | --- |
| `SIDEBAR_W` | 40（固定；完整容纳 32 字符 session ID 并留一列内边距） |
| `gap` | 1 |
| `gutter` | 2 |
| 侧栏可见条件 | `frame_w >= frame_h × 3` **且** `frame_w - 40 - 1 - 2 >= 72` |
| 侧栏可见时 | `left_w = frame_w - 40 - gap`，`content_w = left_w - gutter` |
| 侧栏隐藏时 | `left_w = frame_w`，`content_w = frame_w - gutter`，出现独立底部状态行 |

侧栏可见需要同时满足两个条件：比例条件回答窗口是否真正偏横向，剩余宽度
条件回答分出固定侧栏后对话区是否仍然实用。锁定尺寸：`176 × 50` 显示
（`left_w / content_w` = `135 / 133`）；`115 × 47`、`85 × 47`、`56 × 47`
比例不足 3:1，隐藏；`80 × 24` 虽满足比例但分栏后正文仅 37 列，隐藏。所有
减法 saturating，极小终端不 panic、不下溢。

右栏只放界面状态信息。1 列内边距（字不贴 gap），可用宽 = `SIDEBAR_W - 1`。
`session_id` / `agent` 超可用宽截断。不画边框。

`Role` 五枚举不增：布局从动态宽度改固定宽度，枚举不用加。

### 左栏垂直切分

```text
transcript   剩余高度
停靠区       1 行分隔轨 + 1..=5 行内容（2..=6 行，Dock::height() 是唯一来源）
底部状态行   1 行，仅侧栏隐藏时存在
```

transcript 高度 = `frame_h - (dock_h + bottom_h)`，`bottom_h` 侧栏隐藏时为
1、可见时为 0。极矮终端以实际分配到的 `Rect.height` 为准，不假设 transcript
至少一行。

### 侧栏隐藏时的底部状态行

左栏结构为对话记录 + 停靠区 + 底部状态行。底部状态行只占一行：

- 整宽给状态文本，**不再携带 `PROMPT` tag**（tag 已并入停靠区分隔轨）
- 背景 `navy-11` + `DimMeta` 字
- 从左到右：`session_id · agent · phase · [scroll]? · Alt+Enter newline`（kitty 时键位为 `S-Enter newline`）
- 第一次 Ctrl+C 后的两秒内，`Ctrl+C to quit` 暂时替换最低优先级的换行键提示
- 依次让位：先移除键位提示，再截断 `session_id`，再截断 `agent`；`phase` 与 `[scroll]` 在空间允许时优先保留
- 宽度仍不足时按 display width 尽力裁剪。任何宽高下不得 panic，不得发生 `u16` 下溢
- `stream` 中 phase 字走 User；`cancel` phase 走 ToolErr——与右侧栏同一套着色

---

## 3. 左栏结构（从上到下）

侧栏可见：

```text
transcript          不填背景，透出终端底色
── PROMPT ─────     停靠区分隔轨（替代旧分割线 + 独立 Prompt tag 行）
>  prompt           内容 1..=5 行；`>` 只在第一行
   prompt           续行无前缀，与第一行正文对齐
```

侧栏隐藏时底部多一行底部状态行，见 §2。Warn / Error 状态的停靠区分隔轨形如
`── ERROR ──── SPACE → PROMPT ────`。

### Transcript

说话人单独一行：用户 `user`（User / iris-7），助手用当前 agent 名（Assistant / cream-2）。不当每行前缀。正文从下一行起并统一使用 Assistant / cream-2，wrap 在 `content_w`。回合之间空一行当分隔。工具行留在助手回合内，不再加空行。左栏不填背景；透出来的是终端底色，视觉上等于 `navy-12` `#1c293e`。暂驻停靠区的 Warn / Error 不重复出现在对话记录；释放后在原本的时间位置出现且只出现一次。

```text
user
帮我看下 session.rs 里的重开逻辑

assistant
我先读 pending 模块和重开入口。
▸ ✓ observe.workspace · 1204 B
▾ ✗ mutate.edit
  expected_hash mismatch

user
改成先校验再重开
```

**一个助手段只印一次 agent 名。** 渲染时线性扫描 blocks：

1. 遇到用户块 → 空行 + `user` + 正文，重置 header 状态
2. 此后第一次遇到助手正文 / tool / 终态 status → 印一次 agent 名
3. 直到下一个用户块为止，不再重复
4. `Notice`（启动横幅、`/help`）是独立 meta，既不触发也不重置 header

底层 blocks 保持扁平，不引入 Turn 容器、不存 turn ID。

### 停靠区

统一组件，三个互斥状态，`INFO` 永远不进停靠区：

- `PROMPT`：显示输入编辑器
- `WARN`：显示当前需要用户注意的警告（`LlmRetry`）
- `ERROR`：显示当前需要用户注意的错误
- 分隔轨下方内容 1..=5 行：空 Prompt 1 行；Prompt 超 5 个视觉行沿用光标可见的内部滚动；Warn / Error 超 5 个视觉行显示前 5 行，不给只读停靠区增加滚动或展开
- Warn / Error 到来即一次 turnover：旧状态释放回对话记录，新状态暂驻停靠区；WARN / ERROR 不自动消失，无修饰 Space 是唯一的回 Prompt 操作，Space 被消费、不进入输入缓冲
- 分隔轨：tag 左锚定，与输入正文起点同一视觉锚点；`─` 填充，非 UTF-8 回退 `-`；`SPACE → PROMPT` 只在 WARN / ERROR 显示；极小宽度 best effort 截断不 panic；完整错误摘要不进分隔轨，放内容区
- tag 固定占 6 cells：`PROMPT` / `WARN` 补 2 列 padding / `ERROR` 补 1 列 padding

### Status tag

- 停靠区分隔轨上 6 字符色块，不是方括号；不占内容高度
- 文案只有 `PROMPT` / `WARN` / `ERROR`：全大写、无点号、无方括号
- 填充色见 §6 背景填充表，字复用 `Assistant` / `cream-2`（不新增第六种前景色）
- 左对齐，贴左栏内容起始（gutter 之后）
- `SLASH` / `BASH` / `CONFIG` 的结构位保留，本轮不实现、不画

---

## 4. 右栏

- 全高 `SIDEBAR_W`（40）列，背景填充 `navy-11`
- 1 列内边距，可用宽 = `SIDEBAR_W - 1`，字不要贴 gap
- 字 `DimMeta`；`stream` 中 phase 走 User；`cancel` phase 走 ToolErr

自上而下各占一行，不横排：

1. `session_id`（超可用宽截断）
2. `agent`（超可用宽截断）
3. 空行
4. `phase`：短 ASCII，`idle` | `stream` | `commit` | `cancel`（现码印 `streaming`/`committing`/`cancelling`，本轮改短以适配栏宽）
5. 空行
6. 换行键：`Alt+Enter`，下一行 `newline`；kitty 用 `S-Enter`
7. 未跟随时：`[scroll]`（沿用现码措辞，不改名）
8. 第一次 Ctrl+C 后两秒内显示：`Ctrl+C to quit`

Status 在右栏顶，`tip.key` 在右栏底。具体用词可按终端改（kitty），结构不要改。

Ctrl+C 提示有 2 秒时限，但事件循环在 `dirty == false` 时跳过重绘，提示会留死。用现有 33ms ticker 判过期后置 `dirty = true` 即可，不另开 timer。

---

## 5. 细节：说话人和 tool

gutter 两列预留折叠符，**不加新键、不做鼠标处理**。折叠符是状态指示，不是控件：Ok 收起后不可展开。可折叠块才画符；实现用 `▸` / `▾`，ASCII 回退 `>` / `v`，不要把 ASCII 回退当成品字形。

默认按本表：Ok 收 / Failed 展。

| 块 | 默认 |
| --- | --- |
| tool Ok | 一行收起 |
| tool Failed | 展开，露出 `error_summary` |
| 进行中 | 一行，`DimMeta`（`running ·`） |

### 可用字段

事件只有 `id` / `name` / `ok` / `output_len` / `error_summary`，零按工具名分支。

- `id` 是 opaque invocation ID，只用于 started/finished 配对，**默认不显示**；确需排障时可显示截断的 `#<id>`，但不得伪装成操作名
- 没有 operation、hash、参数字段——不要显示不存在的东西
- `output_len` 是**字节数**（saga 取 `byte_length`），显示为 `1204 B`，不写 `chars`、不加千分位

行形如：

```text
▸ ✓ observe.workspace · 1204 B
▾ ✗ mutate.edit
  expected_hash mismatch
```

`✓` 与 metadata 用不同 Span 着色即可，不借机建 rich-text/card renderer。

### running 收口

`ToolCallStarted` 不保证有配对的 `ToolCallFinished`：saga 在两者之间有多条错误提前退出路径。收到 turn 终态事件、或 stream 结束时，把仍为 `Running` 的 tool 收口为现有的 `Failed`，error 用简短原因（`turn ended before tool completion` / `cancelled` / 对应终态原因）。复用 `ToolState::Failed`，不新增 `Interrupted` / `Unknown` 枚举。

Ok 用 `ToolOk` 勾。Failed 的 `✗`、`error_summary`、`TurnFailed`、cancel phase 走 `ToolErr`。其余 meta（工具名、字节数）走 `DimMeta`。

---

## 6. 色

终端不灌 Cassette 全表。前景只许五种；背景填充只许下面四处。左栏不填背景。承诺深色终端主题。

### 五种前景色

| 角色 | 终端命名色 | Cassette token | hex | 用途 |
| --- | --- | --- | --- | --- |
| User | LightBlue | iris-7 | `#8695e3` | 说话人 `user`、提示符 `>`、stream 中 phase |
| Assistant | White | cream-2 | `#f5f3ef` | agent 名、正文、输入文本、三个停靠区 tag 字、停靠区 Warn 正文 |
| ToolOk | Cyan | cyan-7 | `#2ab0b0` | `✓` |
| ToolErr | signal-red-9 | `#cb2b31` | `Red` | `✗`、TurnFailed、cancel phase、停靠区 Error 正文 |
| DimMeta | DarkGray | warm-gray-8 | `#857f78` | 右栏字、分隔轨、`SPACE → PROMPT`、running ·、字节数、键位 |

### 四处背景填充

| 表面 | token | hex | 命名色回退 | 字 |
| --- | --- | --- | --- | --- |
| 右侧栏 bg（底部状态行同此） | navy-11 | `#2c4267` | Blue | DimMeta |
| `PROMPT` tag bg | iris-9 | `#5965ca` | LightBlue | Assistant / cream-2 |
| `WARN` tag bg | amber-9 | `#9f6200` | Yellow | Assistant / cream-2 |
| `ERROR` tag bg | signal-red-9 | `#cb2b31` | Red | Assistant / cream-2 |

真彩色用 hex；否则用上表命名色回退。不探测 256 色、不读 terminfo、不引入 Theme 类型。不要为了这四处把整套 8×12 塞进终端。`signal-red-9` 同时作为 `ToolErr` 前景色与 `ERROR` tag 填充色，hex 不变。amber 取自配色源 `docs/cassette-futurism-palette.jsx` 的 `amber-9`（step 9 为 `solid`，是填充面应取的阶；amber 在该表的语义位就是 warning / caution），`#9f6200` 底上压 cream-2 `#f5f3ef` 对比度约 4.6:1，高于右栏 DimMeta 压 navy-11 的 2.55:1。

终端底（transcript 透出）=`navy-12` `#1c293e`。它不是填出来的色，是默认底。

`DimMeta #857f78` 压在右栏 `#2c4267` 上，sRGB 相对亮度对比度约 2.55:1，小字号下可能偏暗。锁版前真机看一次；确实难读就让 `session_id` / `agent` 这类主值改走 `Assistant`，提示文字继续 `DimMeta`——不引入 accessibility 系统。

### 禁止

- amber 扩散到 `WARN` tag 填充色之外的任何区域
- 第六种前景色
- 给左栏 / transcript 填背景
- 把 Cassette 其他阶当 TUI token 用
- 浅色终端适配、主题系统、响应式 UI
- 把 `[scroll]` 改成别的文案
