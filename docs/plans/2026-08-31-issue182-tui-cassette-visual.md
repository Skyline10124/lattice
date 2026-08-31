# Issue #182 — `lattice-tui` Cassette Futurism 视觉刷新

Date: 2026-08-31
Issue: <https://github.com/Skyline10124/lattice/issues/182>
Spec: `docs/specs/2026-08-31-issue182-tui-cassette-futurism-spec.md`（定稿 2026-08-28，2026-08-31 讨论复核；设计权威）
配色源：Cassette Futurism v2（7 hues × 12 steps，OKLCH 生成）
Follow-on: #116（卡片渲染器）、#149（卡片接线）——本 issue 不含

本文档是**接口级实现说明**：固定文件集、类型与函数签名、布局参数、配色和键位行为。
凡未写在此处的均不在本 issue 范围——范围外一律不做是硬约束。

---

## 1. 决策表

| # | 决策 |
| D1 | 深色终端主题。浅色终端、主题系统、主题选择器、响应式 UI 均不做，也不留扩展位。 |
| D2 | 输入高度 2..=5 行自适应（取代现码 1..=6）；bracketed paste 一次到位；发送/清空后收回 2 行。 |
| D3 | status tag 恒为 `PROMPT`（全大写、无点号、无方括号，6 字符色块），不随 phase 变化。`SLASH`/`BASH`/`CONFIG` 只留结构位，不画不实现。 |
| D4 | 前景五种色 + 两处背景填充，无第六种；`amber` 不进 TUI；不给左栏/transcript 填背景。 |
| D5 | truecolor 终端用 hex，否则就近命名色。本 issue 正式取代 `view.rs` 的 "terminal semantic colors only — no RGB" 注释。 |
| D6 | 折叠符 `▸/▾`（ASCII 回退 `>/v`）是**状态指示，不是控件**：本轮不加新键、不做鼠标处理，Ok 收起后不可展开。展开交互后续随卡片/鼠标一起做。 |
| D7 | 工具行只读通用字段 `id`/`name`/`ok`/`output_len`/`error_summary`，零按工具名分支。 |
| D8 | 布局：`status_w=clamp(frame_w/8,16,24)`（整数除法）、`gap=1`、`gutter=2`、`content_w=left_w−gutter`（无上限）；右栏可用宽 = `status_w-1`。`frame_w<56` 取消右栏，`PROMPT` tag 与 status 合并为底栏一行（各自保留配色）。 |
| D9 | `Role` 五枚举不增；`TurnPhase` 四值（idle/stream/commit/cancel）不增。本轮只改色值映射与布局。 |
| D10 | 输入提示符 `>` 只在第一行；续行无前缀，与第一行正文对齐。`>` 走 User，输入文本走 Assistant。 |
| D11 | `[scroll]` 沿用现码措辞，不改名。 |

---

## 2. 契约文档修订（第一个 commit，纯文档）

1. `crates/lattice-tui/AGENTS.md`
   - 新增 §配色锁：五种前景色表 + 两处背景填充表（逐字抄入 §6），注明 `amber` 禁止与第六种禁止。
   - 新增 §布局锁：§4 参数表逐字抄入。
   - 新增 §折叠非交互声明（D6 逐字）。
   - 删除/取代任何 "no RGB" 表述（D5）。
2. 根 `AGENTS.md` **不动**：crate 角色描述与依赖边均无变化。

---

## 3. 范围

进本轮：

- 左栏（transcript 上、输入下）+ 右栏 status 横切布局
- 说话人独立成行（用户 `你` / User，助手 agent 名 / Assistant），回合之间空一行分隔
- 输入区 2..=5 行自适应 + bracketed paste + `PROMPT` 色块 + 左栏分割线
- tool 块默认折叠（Ok 收 / Failed 展 / 进行中 `running ·`）
- 五种前景色 + 两处背景填充 + truecolor 检测回退
- 窄屏（<56 列）右栏取消、`PROMPT` tag 与 status 合并为底栏一行

不进本轮：

- `#116` 卡片、`#149`
- 更多 mode tag（`SLASH`/`BASH`/`CONFIG`）
- 第六种前景色、整套 RGB 灌终端、`amber`
- logo / mark
- 折叠展开交互（键位或鼠标）
- 把 `[scroll]` 改成别的文案

---

## 4. 布局尺寸（接口）

```rust
// view.rs
pub(crate) const STATUS_MIN: u16 = 16;
pub(crate) const STATUS_MAX: u16 = 24;
pub(crate) const GAP: u16 = 1;
pub(crate) const GUTTER: u16 = 2;      // 折叠符预留，不含正文
pub(crate) const NARROW_MIN: u16 = 56; // 低于此宽取消右栏

pub(crate) fn status_w(frame_w: u16) -> u16 {
    (frame_w / 8).clamp(STATUS_MIN, STATUS_MAX)
}

pub(crate) enum StatusSlot {
    Right { w: u16 },   // 全高右栏，w == status_w(frame_w)
    BottomLine,         // 窄屏：tag + status 合并底栏一行
}

pub(crate) struct Layout {
    pub left_w: u16,
    pub content_w: u16,
    pub status: StatusSlot,
}

pub(crate) fn frame_layout(frame_w: u16) -> Layout
```

- `frame_w >= NARROW_MIN`：`sw = status_w(frame_w)`，`left_w = frame_w - sw - GAP`，`content_w = left_w - GUTTER`（不设上限），右栏全高。右栏可用宽 = `sw - 1`。
- `frame_w < NARROW_MIN`：无右栏；底栏一行 = 左侧 `PROMPT` tag（iris-9 底 + cream-1 字）+ 右侧 status 段（navy-11 底 + DimMeta 字，填满余宽）。status 段从左到右 `session_id · agent · phase · [scroll]? · Alt+Enter 换行`（kitty 为 `S-Enter 换行`）；放不下时先丢键位提示、再截断 `session_id`，`phase` 与 `[scroll]` 永远保留。`stream`/`cancel` phase 着色与右栏相同。左栏结构（transcript / 分割线 / 输入）不变。
- 80 列基准：`status_w = 16`，`left_w = 63`，`content_w = 61`。176 列：`status_w = 22`，`left_w = 153`，`content_w = 151`。
- gap 是空列，不画线；分割线只画左栏，不穿 gap、不进右栏。

---

## 5. 左栏结构（从上到下）

```text
transcript          不填背景，透到终端底（深色主题承诺，视为 navy-12）
────────────        左栏分割线，DimMeta
>  prompt           高 2..=5 行；`>` 只在第一行
   prompt           续行无前缀，与第一行正文对齐
PROMPT              6 字符色块，不是方括号
```

- 说话人单独一行：`你` 走 User，agent 名走 Assistant；正文从下一行起，wrap 在 `content_w`。
- 回合之间空一行分隔；工具行留在助手回合内，不加空行。
- prompt 前缀 `>` 只在第一行（User 色）；输入文本 Assistant 色；块光标 cream-2。
- tag 左对齐，贴左栏内容起始（gutter 之后）；窄屏时并入底栏（见 §4）。

---

## 6. 配色（接口）

```rust
// view.rs
fn truecolor() -> bool               // COLORTERM ∈ {truecolor, 24bit}
fn slot_color(role: Role) -> Color   // truecolor → Color::Rgb(hex)；否则命名色回退
fn surface_bg(surface: Surface) -> Color  // 右栏 / PROMPT tag 仅有的两处背景填充
```

五种前景色：

| 角色 | 命名色回退 | Cassette token | hex |
| --- | --- | --- | --- |
| `User` | LightBlue | iris-7 | `#8695e3` |
| `Assistant` | White | cream-2 | `#f5f3ef` |
| `ToolOk` | Cyan | cyan-7 | `#2ab0b0` |
| `ToolErr` | Red | signal-red-9 | `#cb2b31` |
| `DimMeta` | DarkGray | warm-gray-8 | `#857f78` |

两处背景填充：

| 表面 | token | hex | 字 |
| --- | --- | --- | --- |
| 右栏 bg（窄屏 status 段同此） | navy-11 | `#2c4267` | DimMeta |
| `PROMPT` tag bg | iris-9 | `#5965ca` | cream-1 `#fbfaf8` |

状态字着色：`stream` 中 phase 字走 `User`；`cancel` phase 走 `ToolErr`；其余右栏字走 `DimMeta`。

---

## 7. 折叠与工具行

- gutter 两列预留折叠符；可折叠块才画符，`▸/▾`，ASCII 回退 `>/v`。
- 默认：Ok 一行收起；Failed 展开露出 `error_summary`；进行中一行 `DimMeta`（`running ·`）。
- Ok 的 `✓` 走 `ToolOk`；Failed 的 `✗`、`error_summary`、`TurnFailed`、cancel phase 走 `ToolErr`；工具名、字符数、hash 走 `DimMeta`。
- 折叠符不可交互（D6）。

---

## 8. 输入高度与粘贴

```rust
// app.rs
pub(crate) const INPUT_MIN_ROWS: u16 = 2;  // 取代现码 1
pub(crate) const INPUT_MAX_ROWS: u16 = 5;  // 取代现码 6
```

- 高度 = `clamp(内容行数, 2, 5)`；内容超 5 行时区内滚动，光标行始终可见。
- 发送/清空后收回 2 行。
- 终端 setup 挂 `EnableBracketedPaste`，restore 挂 `DisableBracketedPaste`（与 keyboard enhancement 同生命周期）。
- `Event::Paste(s)` 一次插入、一次重算高度、一帧到位——不做逐字节处理。
- 粘贴内容中的换行是文本换行，不触发 Submit。

---

## 9. 文件集与上限

改动仅限 `crates/lattice-tui/src/`：

| 文件 | 改动 |
| --- | --- |
| `view.rs` | `frame_layout` / `status_w` + 配色函数 + 右栏/tag/分割线渲染 + 说话人行 + tool 折叠渲染 |
| `app.rs` | 输入高度常数与夹紧；`Event::Paste` 折叠进 blocks/phase 的纯函数路径 |
| `input.rs` | `Event::Paste` → 批量插入 action（其余键位表不动） |
| `session.rs` / `lib.rs` | bracketed paste 的 enable/disable 挂进终端生命周期 |

上限：净增 **≤400 行**（不含 `#[cfg(test)]` 模块）。超出先停下来复审范围，不许削测试凑数。

---

## 10. 验证

- `cargo test -p lattice-tui` 全绿（GitHub CI 同一口径）。
- 新增三个纯函数测试（一个函数一组断言，不拆九条）：
  1. `frame_layout`：80 → `Right{16}` / left 63 / content 61；176 → `Right{22}` / 153 / 151；55 → `BottomLine`。
  2. 输入高度：空=2、3 行=3、6 行=5。
  3. 窄屏 status 段截断：先丢键位提示，再截 `session_id`；`phase` 与 `[scroll]` 保留。
- 其余（配色、提示符只第一行、paste 一次插入）靠现有测试 + 真机 smoke，不单开用例。
- 渲染快照极少（延续 #118 D5）。
- 手动 smoke：深色终端真机跑一次，确认右栏/tag 填背景与五种前景色。
