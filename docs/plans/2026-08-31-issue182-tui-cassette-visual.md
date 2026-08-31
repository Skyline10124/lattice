# Issue #182 — `lattice-tui` Cassette Futurism 视觉刷新

Date: 2026-08-31
Issue: <https://github.com/Skyline10124/lattice/issues/182>
Spec: `docs/specs/2026-08-31-issue182-tui-cassette-futurism-spec.md`（设计权威）
配色源：Cassette Futurism v2（7 hues × 12 steps，OKLCH 生成）
Follow-on: #116（卡片渲染器）、#149（卡片接线）——本 issue 不含

本文档是**接口级实现说明**：固定文件集、类型与函数签名、布局参数、配色和键位行为。
凡未写在此处的均不在本 issue 范围——范围外一律不做是硬约束。

2026-08-31 实现前对码复核已把以下现码事实并入本文：几何三处口径不一、`TokenDelta`
在 tool 之后另起 `Assistant` 块、`ToolCallStarted` 可无配对 `Finished`、`output_len`
是字节、bracketed paste 已完整落地、`ratatui::layout::Layout` 已占名。

---

## 1. 决策表

| # | 决策 |
| D1 | 深色终端主题。浅色终端、主题系统、主题选择器、响应式 UI 均不做，也不留扩展位。 |
| D2 | 输入高度 2..=5 行自适应（取代现码 1..=6）；发送/清空后收回 2 行。 |
| D3 | status tag 恒为 `PROMPT`（全大写、无点号、无方括号，6 字符色块），不随 phase 变化。`SLASH`/`BASH`/`CONFIG` 只留结构位，不画不实现。 |
| D4 | 前景五种色 + 两处背景填充，无第六种；`PROMPT` tag 字复用 `Assistant`，不新增 `cream-1`；两处背景各有命名色回退（Blue / LightBlue）。 |
| D5 | truecolor 终端用 hex，否则就近命名色。本 issue 正式取代 `view.rs` 的 "terminal semantic colors only — no RGB" 注释。不探测 256 色、不读 terminfo。 |
| D6 | 折叠符 `▸/▾`（ASCII 回退 `>/v`）是**状态指示，不是控件**：不加新键、不做鼠标处理，Ok 收起后不可展开。 |
| D7 | 工具行只读 `name`/`ok`/`output_len`/`error_summary`，零按工具名分支。`id` 是 opaque 配对键，默认不显示。`output_len` 是字节，显示 `1204 B`。 |
| D8 | 布局：`status_w=clamp(frame_w/8,16,24)`（整数除法）、`gap=1`、`gutter=2`、`content_w=left_w−gutter`（无上限）；右栏可用宽 = `status_w-1`。`frame_w<56` 取消右栏，`PROMPT` tag 与 status 合并为底栏一行（各自保留配色）。 |
| D9 | `Role` 五枚举不增；`TurnPhase` 四值不增。`ToolState` 三值不增。不引入 Turn 容器类型。 |
| D10 | 输入提示符 `>` 只在第一行；续行无前缀，与第一行正文对齐。`>` 走 User，输入文本走 Assistant。 |
| D11 | `[scroll]` 沿用现码措辞，不改名。 |
| D12 | **单一几何真相源**：渲染、行数统计、viewport、输入换行、光标定位全部走 `content_w`，行数取渲染结果长度，不再各算各的。 |
| D13 | **一个助手段只印一次 agent 名**，靠渲染期线性扫描实现，blocks 保持扁平。 |
| D14 | **running tool 必须收口**：turn 终态或 stream 结束时，仍 `Running` 的 tool 落为 `Failed`，复用现有枚举。 |
| D15 | bracketed paste 已在 main 完整落地（enable/disable、`Event::Paste`、CRLF 归一、测试），本 issue **不改动**，只在新布局下验证。 |
| D16 | 光标沿用终端默认外观，不自绘软光标、不控制形状颜色。 |
| D17 | 新类型名 `FrameLayout`，避开已导入的 `ratatui::layout::Layout`。 |

---

## 2. 契约文档修订（第一个 commit，纯文档）

1. `crates/lattice-tui/AGENTS.md`
   - §Role / §v1 克制清单：三区示意改为「左栏（transcript / 分割线 / 输入 / 底行）+ 右栏 status」，输入 1..=6 改 2..=5。
   - 新增 §配色锁：五种前景色表 + 两处背景填充表（含命名色回退），注明 `amber` 禁止与第六种禁止。
   - 新增 §布局锁：§4 参数表逐字抄入，含 `transcript = frame_h - (input_h + 2)`。
   - 新增 §折叠非交互声明（D6 逐字）。
   - 删除/取代任何 "no RGB" 表述（D5）。
2. 根 `AGENTS.md` **不动**：crate 角色描述与依赖边均无变化。

---

## 3. 范围

进本轮：

- 左栏 + 右栏横切布局，统一几何口径
- 说话人独立成行、一个助手段一次 agent 名、回合之间空一行
- 输入区 2..=5 行自适应、`PROMPT` 色块、左栏分割线
- tool 块默认折叠、字节长度、running 收口
- 五种前景色 + 两处背景填充（含命名色回退）+ truecolor 检测
- 窄屏（<56 列）右栏取消、tag 与 status 合并底栏一行、尽力裁剪不 panic
- Ctrl+C 提示过期重绘；提示文案改「2 秒内再按一次 Ctrl+C」

不进本轮：

- `#116` 卡片、`#149`
- 更多 mode tag（`SLASH`/`BASH`/`CONFIG`）占位枚举
- 第六种前景色、整套 RGB 灌终端、`amber`
- logo / mark
- 折叠展开交互（键位或鼠标）
- 把 `[scroll]` 改成别的文案
- Theme / Palette trait、`render/` 模块、嵌套 Turn 模型、新依赖
- bracketed paste 相关改动，含 `insert_paste` 的逐字符插入重写（无已知性能问题）

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

/// 名为 FrameLayout：`Layout` 已被 `ratatui::layout::Layout` 占用（view.rs 已导入）。
pub(crate) struct FrameLayout {
    pub left_w: u16,
    pub content_w: u16,
    pub status: StatusSlot,
}

pub(crate) fn frame_layout(frame_w: u16) -> FrameLayout
```

- `frame_w >= NARROW_MIN`：`sw = status_w(frame_w)`，`left_w = frame_w - sw - GAP`，`content_w = left_w - GUTTER`（不设上限），右栏全高、可用宽 `sw - 1`。
- `frame_w < NARROW_MIN`：`StatusSlot::BottomLine`，无右栏，`left_w = frame_w`。底栏一行 = 左侧 `PROMPT` tag（iris-9 底 + Assistant 字）+ 右侧 status 段（navy-11 底 + DimMeta 字，填满余宽）。让位顺序：键位提示 → `session_id` → `agent`；`phase` 与 `[scroll]` 优先保留；再不够按 display width 尽力裁剪。
- **先判窄屏再算 `left_w`，或一律 `saturating_sub`**：`frame_w = 1` 不得下溢或 panic。
- 垂直：`transcript = frame_h.saturating_sub(input_h + 2)`（分割线 1 + 输入 `input_h` + 底行 1），宽窄同一公式。极矮终端以实际 `Rect.height` 为准。
- 80 列：`status_w = 16`，`left_w = 63`，`content_w = 61`。176 列：`22 / 153 / 151`。
- gap 是空列，不画线；分割线只画左栏，不穿 gap、不进右栏。

---

## 5. 单一几何真相源

现码三处各算各的：`draw_transcript` 用 `area.width` 逐块 `block_lines`，`content_rows(w)`
用 `term_width` 再算一遍，`transcript_height()` 减 `input_h + 1`。加上右栏、gutter、说话人行、
回合空行、failed 详情之后，三者必然错位，表现为 follow / PageDown / resize 后 viewport 跳错。

不做通用布局引擎，只加一个唯一真相源：

```rust
// view.rs
pub(crate) fn transcript_lines(app: &App, content_w: u16) -> Vec<Line<'static>>
```

它一次产出全部：说话人行、agent header 去重、正文换行、助手段内 tool 行、回合空行、
failed tool 第二行、status / notice 行。然后：

- `draw_transcript()` 渲染它的切片
- `App::content_rows()` 直接返回它的 `len()`
- 输入高度、输入换行、光标定位统一用 `content_w`（现码用整幅 `term_width`）
- resize 后即使 `follow == false` 也要把 `scroll` clamp 到新的最大行数，否则窗口变宽后显示空白
- `transcript_height()` 改减 `input_h + 2`

---

## 6. 说话人分组（渲染期，不改数据结构）

`TokenDelta` 只在末块是 `Assistant` 时追加，因此 tool 之后的 token 必然另起一个
`Assistant` 块。若每个 `Assistant` 块都印 agent 名就会出现两次。

线性扫描规则（`transcript_lines` 内部状态，不落库）：

1. `Block::User` → 空行 + `你`（User 色）+ 正文，重置 header 标志
2. 其后第一次遇到 `Assistant` / `Tool` / 终态 `Status` → 印一次 agent 名（Assistant 色），置标志
3. 到下一个 `Block::User` 之前不再重复
4. `StatusKind::Notice` 作为独立 meta 渲染，既不触发也不重置标志（启动横幅与 `/help` 因此无需改造）

---

## 7. 折叠与工具行

- gutter 两列预留折叠符；可折叠块才画符，`▸/▾`，ASCII 回退 `>/v`。
- 默认：Ok 一行收起；Failed 展开露出 `error_summary`；进行中一行 `DimMeta`（`running ·`）。
- 行内容只用 `name` + 字节长度：`▸ ✓ observe.workspace · 1204 B`。失败第二行是 `error_summary`。
- `output_len` 是字节（saga 取 `MaterialAvailability::byte_length`），显示 `NNNN B`，不写 `chars`、不加千分位。
- `id` 只用于 started/finished 配对，默认不渲染。
- `✓` 走 `ToolOk`，`✗` / `error_summary` 走 `ToolErr`，工具名与字节数走 `DimMeta`——用多个 `Span` 拼一行即可，不建卡片渲染器。
- **running 收口**：`apply_turn_event` 收到 turn 终态（`TurnCommitted` / `CommitFailed` / `TurnFailed` / `TurnCancelled` / `RecoveryRequired` / `EvidenceDurabilityFailed`）时，以及 `TurnWait::Ended` / 超时时，把仍 `Running` 的 tool 置为 `ToolState::Failed { output_len: 0, error: Some(短原因) }`。不新增枚举值。

---

## 8. 输入高度

```rust
// app.rs
pub(crate) const INPUT_MIN_ROWS: u16 = 2;  // 取代现码 1
pub(crate) const INPUT_MAX_ROWS: u16 = 5;  // 取代现码 6
```

- 高度 = `clamp(按 content_w 换行后的行数, 2, 5)`；超 5 行区内滚动，光标行始终可见。
- 发送/清空后收回 2 行。
- bracketed paste 保持现有行为（D15）：只验证新布局下 paste 后高度一次更新、内容不提交。

---

## 9. 文件集

改动限于：

| 文件 | 改动 |
| --- | --- |
| `view.rs` | `FrameLayout` / `status_w` / `frame_layout`；`transcript_lines` 单一真相源；配色函数与命名色回退；右栏 / tag / 分割线 / 窄屏底栏渲染；输入与光标改用 `content_w` |
| `app.rs` | 输入高度常数；`content_rows` / `transcript_height` 改口径；running tool 收口；Ctrl+C 提示过期置 `dirty` |
| `crates/lattice-tui/AGENTS.md` | 布局与配色契约（见 §2） |

`input.rs` / `session.rs` / `lib.rs` 在未发现新缺陷的前提下**不动**——paste 链路已完整。

净增 ≤400 行是**复审提醒**，不是硬验收门槛：超了就停下来复审范围，但不要为压行数把渲染逻辑写密。

---

## 10. 验证

```text
cargo test -p lattice-tui
重新 build lattice-cli
LATTICE_BIN=<刚建的二进制> 跑 lattice-system-harness 的 pty_tui_smoke
深色 truecolor 终端手动 smoke
非 truecolor 回退手动 smoke
```

（PTY smoke 是 `crates/lattice-tui/AGENTS.md` §Verification 的既有要求。）

四组纯函数测试：

1. **布局边界**：`frame_layout` 在 `1 / 7 / 55 / 56 / 80 / 176` 下均不 panic、不下溢；`56` 必须走宽屏分支，`55` 走 `BottomLine`；80 → `Right{16}` / 63 / 61，176 → `Right{22}` / 153 / 151。
2. **说话人分组与行数一致**：`User → Assistant → Tool → Assistant → Committed → User` 只印一次 agent 名、tool 前后无多余空行，且 `transcript_lines().len() == content_rows()`。
3. **running 收口与字节显示**：终态事件后仍 `Running` 的 tool 变 `Failed`；stream 结束同理；长度渲染为 `NNNN B`。
4. **输入**：2..=5 夹紧（空=2、3 行=3、6 行=5）；`>` 只出现在第一视觉行；CJK 换行后光标位置正确。

渲染快照极少（延续 #118 D5）。
