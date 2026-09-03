# LATTICE TUI：固定侧栏、真彩色透传与停靠区重构计划

日期：2026-09-03  
状态：讨论收敛稿，待实施  
范围：`lattice-tui` 视觉与底部交互结构的三阶段调整  
原则：KISS、YAGNI；优先删除现有特殊分支，不引入通知系统、主题系统或额外交互层

---

## 1. 目标

本计划分为三个阶段：

1. **固定侧栏策略**：侧栏固定为 40 列，只在终端真正足够横向、且分栏后对话区仍足够宽时显示。
2. **修复 `tui-mock` 的终端能力透传**：保留 harness 的环境隔离，只为交互式 TUI mock 透传必要的终端环境变量。
3. **将现有 Prompt 区重构为统一停靠区**：`PROMPT`、`WARN`、`ERROR` 是同一停靠区的三个互斥状态；`INFO` 直接进入对话记录。

计划不建设通用响应式系统，也不建设通知队列。所有改变都立足于当前 `app.rs` / `view.rs` 的扁平 block 模型。

---

## 2. 总体布局

### 2.1 侧栏可见

```text
┌──────────────────────────────────────────┬─┬────────────────────────────────────────┐
│ transcript                               │ │ sidebar                                │
│                                          │ │ session                                │
│                                          │ │ agent                                  │
│                                          │ │ phase                                  │
│                                          │ │                                        │
├── PROMPT ────────────────────────────────┤ │                                        │
│ > input                                  │ │                                        │
└──────────────────────────────────────────┴─┴────────────────────────────────────────┘
```

左侧只有：

```text
transcript
dock
```

### 2.2 侧栏隐藏

```text
┌───────────────────────────────────────────────────────────────────────────────┐
│ transcript                                                                    │
│                                                                               │
├── ERROR ───────────────────────────────────────────── SPACE → PROMPT ─────────┤
│ permission denied                                                             │
├───────────────────────────────────────────────────────────────────────────────┤
│ session · agent · idle · Alt+Enter newline                                    │
└───────────────────────────────────────────────────────────────────────────────┘
```

左侧结构为：

```text
transcript
dock
bottomline
```

底部状态行只在侧栏隐藏时存在，用来承接 session、agent、phase、scroll 和现有快捷键提示。它不是停靠区的一部分。

---

# 阶段一：固定侧栏

## 3. 侧栏宽度

删除当前动态公式：

```rust
status_w = (frame_w / 8).clamp(16, 24)
```

改为单一固定值：

```rust
const SIDEBAR_W: u16 = 40;
```

理由：

- 侧栏内容没有随宽度变化，动态宽度没有产品价值。
- 40 列可以完整容纳 32 字符 session ID，并留出一列内边距。
- 侧栏只在超宽终端出现，无需再压缩成 16–24 列的细条。
- 固定值更容易理解、调试和做视觉验收。

## 4. 是否显示侧栏

不再使用单一的 `frame_w < 56` 窄屏分支。

侧栏只有同时满足以下两个条件才显示：

```rust
const SIDEBAR_ASPECT: u16 = 3;
const MIN_CONTENT_W_WITH_SIDEBAR: u16 = 72;

let sidebar_content_w = frame_w
    .saturating_sub(SIDEBAR_W)
    .saturating_sub(GAP)
    .saturating_sub(GUTTER);

let sufficiently_landscape =
    frame_w >= frame_h.saturating_mul(SIDEBAR_ASPECT);

let enough_conversation_width =
    sidebar_content_w >= MIN_CONTENT_W_WITH_SIDEBAR;

let show_sidebar =
    sufficiently_landscape && enough_conversation_width;
```

两个条件分别回答：

- **比例条件**：窗口在视觉上是否真正偏横向；
- **剩余宽度条件**：分出固定侧栏后，对话区是否仍然实用。

`frame_layout` 接口改为：

```rust
fn frame_layout(frame_w: u16, frame_h: u16) -> FrameLayout
```

布局状态收敛为：

```rust
enum Sidebar {
    Visible,
    Hidden,
}
```

不再保留动态 `Right { w }`，宽度统一取 `SIDEBAR_W`。

## 5. 已锁定尺寸

以下尺寸按“列 × 行”表示：

| 终端尺寸 | 侧栏 | 原因 |
| --- | --- | --- |
| `176 × 50` | 显示，固定 40 列 | `176 >= 50 × 3`，分栏后正文 133 列 |
| `115 × 47` | 隐藏 | 比例不足 3:1 |
| `85 × 47` | 隐藏 | 比例不足 3:1 |
| `56 × 47` | 隐藏 | 比例不足 3:1 |
| `80 × 24` | 隐藏 | 虽满足比例，但分栏后正文仅 37 列 |

可见状态：

```text
left_w    = frame_w - SIDEBAR_W - GAP
content_w = left_w - GUTTER
```

隐藏状态：

```text
left_w    = frame_w
content_w = frame_w - GUTTER
```

所有减法继续使用 saturating arithmetic，极小终端不得 panic 或发生 `u16` 下溢。

---

# 阶段二：修复 `tui-mock` 的真彩色环境

## 6. 问题原因

Ghostty 外层环境已经具备：

```text
COLORTERM=truecolor
tput colors=256
```

两者并不冲突。`tput colors` 报告的是 terminfo 索引色数量，应用仍可直接输出 24-bit RGB 转义序列。

当前问题来自 `tui-mock` 的启动链路：

```text
Ghostty
  -> tui-mock
  -> ScenarioWorld::command()
  -> env_clear()
  -> lattice tui
```

`ScenarioWorld::command()` 有意清空环境，以保证系统 harness 的隔离性；但 `run_tui_mock()` 没有重新加入 `COLORTERM`，所以真正的 TUI 子进程走了 ANSI `Color::Blue` fallback，而不是设计中的 RGB `#2c4267`。

## 7. 最小修复

保留 `ScenarioWorld::command()` 的全局 `env_clear()` 行为。

只在交互式 `run_tui_mock()` 中透传：

```rust
for key in ["TERM", "COLORTERM", "LANG", "LC_ALL"] {
    if let Ok(value) = std::env::var(key) {
        world.set_env(key, value);
    }
}
```

不做：

- `tput` 调用；
- terminfo 探测；
- Ghostty 特判；
- 256 色能力模型；
- 全局 harness 环境继承；
- 环境透传专用单元测试。

这一阶段只做 Ghostty 真机冒烟验证，确认 `tui-mock` 使用 24-bit RGB 背景和 Unicode 分隔符。

---

# 阶段三：统一停靠区

## 8. 停靠区的职责

停靠区是左侧底部的一个完整组件。

它只有三个互斥状态：

```rust
enum DockView<'a> {
    Prompt,
    Warn(&'a StatusKind),
    Error(&'a StatusKind),
}
```

语义：

- `PROMPT`：显示输入编辑器；
- `WARN`：显示当前需要用户注意的警告；
- `ERROR`：显示当前需要用户注意的错误；
- `INFO`：不进入停靠区，直接进入对话记录。

`PROMPT` 不是停靠区上的常驻槽位，而是停靠区本身的一种状态。

## 9. 停靠区必须作为统一组件实现

外部布局只分配一个完整的 `Dock` Rect：

```rust
struct Dock<'a> {
    view: DockView<'a>,
    input: &'a InputBuffer,
}

impl Dock<'_> {
    /// rail + 内容的总行数，2..=6。视口与布局共用这一个来源。
    fn height(&self, width: u16) -> u16;
    fn render(&self, frame: &mut Frame, area: Rect);
}
```

`Dock::render` 在同一个组件内部完成：

- 分隔轨；
- 状态标签；
- `SPACE → PROMPT` 提示；
- Prompt 输入内容；
- Warn / Error 内容；
- Prompt 光标定位。

不得重新拆成外部 sibling regions 或独立入口：

```rust
draw_dock_header(...)
draw_dock_body(...)
```

低层文本折行、显示宽度、光标计算等纯函数可以继续复用，但分隔轨与内容的布局和绘制必须由单一停靠区组件拥有。

## 10. 停靠区分隔轨

### Prompt

```text
── PROMPT ─────────────────────────────────────────────────────
> input
```

### Warn

```text
── WARN ─────────────────────────────── SPACE → PROMPT ─────────
retrying model request 2/3
```

### Error

```text
── ERROR ────────────────────────────── SPACE → PROMPT ─────────
permission denied
```

约束：

- tag 左锚定，与输入正文起点保持同一视觉锚点；
- 分隔轨使用 `─`，非 UTF-8 环境回退为 `-`；
- `SPACE → PROMPT` 只在 WARN / ERROR 状态显示；
- 极小宽度下 best effort 截断，不得 panic；
- 不将完整错误摘要塞进分隔轨，内容放在停靠区内容区。

标签建议固定占 6 cells：

```text
PROMPT
WARN··
ERROR·
```

`·` 代表 tag 内 padding，不是实际字符。

## 11. 停靠区高度

三种状态共享同一套内容高度：

```rust
const DOCK_CONTENT_MIN_ROWS: u16 = 1;
const DOCK_CONTENT_MAX_ROWS: u16 = 5;
```

这里的 `1..=5` 指分隔轨下方的内容行数。

停靠区总高度为：

```text
1 行分隔轨 + 1..=5 行内容
```

即总高 `2..=6` 行。

`Dock::height()` 是这个高度的唯一来源。`app.rs` 现有的

```rust
self.term_height.saturating_sub(input_h + 2)
```

必须改成按停靠区与底部状态行的实际占用计算：

```rust
let bottom_h = match layout.sidebar {
    Sidebar::Visible => 0,
    Sidebar::Hidden => 1,
};
self.term_height.saturating_sub(dock.height(layout.content_w) + bottom_h)
```

原来的 `+ 2` 是分隔线加底行；停靠区合并后侧栏可见时只剩分隔轨，不改会少算一行。

`input_visual_height()` 不再直接参与视口计算：WARN / ERROR 下停靠区高度由折行后的状态文本决定，继续按 InputBuffer 推导会让视口行数与实际绘制不一致。

这不是三套高度规则：

- Prompt 不再固定最少 2 行；
- Warn 不单独限制 3 行；
- Error 不单独放宽到 5 行；
- 三种状态统一按实际视觉行数 clamp 到 `1..=5`。

具体行为：

- 空 Prompt：1 行输入内容；
- Prompt 超过 5 个视觉行：沿用光标可见的内部滚动；
- Warn / Error 超过 5 个视觉行：显示前 5 个视觉行；
- 完整 Warn / Error 会在后续进入对话记录，不给只读停靠区增加滚动或展开交互。

## 12. 状态数据只保留一份

为避免 Warn / Error 延迟进入对话记录后发生时间顺序倒置，状态事件在发生时仍立即进入 `blocks`，但带有展示位置。

建议模型：

```rust
enum DockSeverity {
    Warn,
    Error,
}

enum StatusPlacement {
    Transcript,
    Dock(DockSeverity),
}

enum Block {
    User(String),
    Assistant(String),
    Tool {
        id: String,
        name: String,
        state: ToolState,
    },
    Status {
        kind: StatusKind,
        placement: StatusPlacement,
    },
}
```

当前 Dock 状态由 blocks 推导：

- 没有 `StatusPlacement::Dock(_)`：`DockView::Prompt`；
- 有 `Dock(Warn)`：`DockView::Warn`；
- 有 `Dock(Error)`：`DockView::Error`。

不变量：`blocks` 中至多存在一个 `StatusPlacement::Dock(_)`。

维护点只有 14.1 的 turnover 和 14.2 的 Space 释放，前者全部收在 `push_dock_status()` 内部；`Block::Status` 的其他写入一律走 `push_status()`，只能产生 `Transcript` placement。`/clear` 清空 `blocks` 天然满足该不变量，不需要 Dock 专用逻辑。

推导时直接取唯一项，不加 `rev()` 兜底：兜底会在不变量被破坏时静默取到某一个，把 bug 藏起来。

这样不需要：

- 第二份 pending message；
- notification queue；
- block index 作为长期状态；
- 消息复制；
- 单独的 Dock 历史。

## 13. 对话记录与停靠区的显示规则

`transcript_lines()`：

- 渲染 `StatusPlacement::Transcript`；
- 跳过当前 `StatusPlacement::Dock(_)`；
- 行数统计与 viewport 同样使用过滤后的渲染结果。

停靠区：

- 从唯一的 `StatusPlacement::Dock(_)` 读取当前 Warn / Error；
- 没有活动状态时显示 Prompt。

Warn / Error 被释放时，只需把：

```rust
StatusPlacement::Dock(...)
```

改为：

```rust
StatusPlacement::Transcript
```

该 status 会在它原本的时间位置出现，且只出现一次。

这能正确处理：

```text
WARN 发生
INFO 发生并直接进入对话记录
用户之后按 Space
```

Warn 被释放后仍位于 Info 之前，不会因延迟 append 而颠倒顺序。

## 14. 停靠区状态转换

### 14.1 新 Warn / Error 到来

统一执行：

1. 释放当前停靠区状态到对话记录；
2. 将新 status 以 `StatusPlacement::Dock(...)` 追加到 blocks；
3. 停靠区显示新状态。

因此以下都属于一次停靠区 turnover：

```text
PROMPT -> WARN
PROMPT -> ERROR
WARN A -> WARN B
WARN A -> ERROR B
ERROR A -> WARN B
ERROR A -> ERROR B
```

旧 Warn / Error 在 turnover 时进入对话记录。

### 14.2 Space 返回 Prompt

WARN / ERROR 状态下，按下**无修饰 Space**：

1. 当前 Warn / Error 的 placement 改为 Transcript；
2. 停靠区回到 Prompt；
3. 本次 Space 被消费；
4. Space 不插入 InputBuffer；
5. 已有输入草稿保持不变；
6. 不改变 turn phase；
7. 不取消正在运行的 turn；
8. 不触发提交。

Prompt 状态下，Space 保持普通输入行为。

### 14.3 其他事件

以下事件不会自动把停靠区切回 Prompt：

```text
ModelStarted
TokenDelta
ToolCallStarted
ToolCallFinished
TurnCommitting
```

明确不做 WARN 自动恢复。

INFO 事件也不切换停靠区，只直接进入对话记录。

除无修饰 Space 外，本阶段不新增其他停靠区切换键。WARN / ERROR 状态下，普通输入、编辑和提交动作不得修改不可见的 InputBuffer；PageUp / PageDown、Ctrl+C、Ctrl+D 等现有全局动作继续沿用现有码语义。

不为 `/clear` 增加停靠区专用逻辑，也不修改现有 `/clear` 产品语义。

## 15. 事件分类

### 进入 WARN 停靠区

```text
LlmRetry
```

### 进入 ERROR 停靠区

```text
LlmFailed
TurnFailed
CommitFailed
EvidenceDurabilityFailed
RecoveryRequired
stream idle timeout
```

当前已经归一为 `CommitFailed` / `TurnFailed` 的错误可以继续复用现有 `StatusKind`，不为停靠区新增重复错误类型。

### 直接进入对话记录，不改变停靠区

```text
TurnCommitted
TurnCancelled
startup Notice
/help Notice
其他 INFO 类状态
```

### 不升级为停靠区状态

```text
普通 tool failure
tool running / success
frame / resize / scroll 状态
Ctrl+C 两秒提示
```

普通 tool failure 继续作为 tool block 保留在对话记录。Agent 可能正常恢复该失败，不应让整个停靠区频繁切成 ERROR。

## 16. 配色

三个停靠区 tag 统一为背景填充块，字一律 `Assistant` / cream-2，与现有 `PROMPT` tag 同形。不给整块停靠区铺高饱和背景。

| tag | Cassette token | hex | 命名色回退 | 字 |
| --- | --- | --- | --- | --- |
| `PROMPT` | iris-9 | `#5965ca` | `LightBlue` | `Assistant` |
| `WARN` | amber-9 | `#9f6200` | `Yellow` | `Assistant` |
| `ERROR` | signal-red-9 | `#cb2b31` | `Red` | `Assistant` |

配色源已收入仓库：`docs/cassette-futurism-palette.jsx`（8 hues × 12 steps）。该表把 step 9 标为 `solid`，正是填充面应取的阶，所以三个 tag 统一取第 9 阶；`amber` 在该表的语义位就是 `warning / caution`。

`PROMPT` 沿用现值。`ERROR` 把 `signal-red-9` 从前景色改作填充色，hex 不变。`WARN` 的 `#9f6200` 底上压 cream-2 `#f5f3ef`，对比度约 4.6:1，高于现有右栏 DimMeta 压 navy-11 的 2.55:1，不需额外复核。

其余部分：

- 分隔轨、`SPACE → PROMPT`、次要 metadata：`DimMeta`；
- 停靠区内容正文：复用 `Assistant`，错误正文可继续使用 `ToolErr`。

实现形态照抄现有 `prompt_style()`：

```rust
fn warn_style() -> Style {
    let bg = if truecolor_terminal() {
        Color::Rgb(0x9f, 0x62, 0x00)
    } else {
        Color::Yellow
    };
    role_style(Role::Assistant).bg(bg)
}
```

因此本阶段**不新增第六种前景色**，`Role` 枚举不动，`view.rs` 的 "Exactly five foreground roles" 也不变；只把 #182 的背景填充表从 2 处扩到 4 处（右栏 bg、`PROMPT`、`WARN`、`ERROR`）。amber 只作为 WARN tag 的填充色进入 TUI，不做 Theme / Palette 抽象，不向其他区域扩散。

---

# 17. 代码改动范围

## `crates/lattice-tui/src/view.rs`

- `frame_layout(frame_w, frame_h)`；
- 固定 `SIDEBAR_W = 40`；
- 侧栏比例与剩余正文宽度判断；
- 删除 `STATUS_MIN`、`STATUS_MAX`、`NARROW_MIN`、`status_w()`；
- 将 `StatusSlot` 收敛为 `Sidebar::Visible / Hidden`；
- 新增统一停靠区组件（`Dock`）；
- 停靠区内部接管分隔轨、输入、Warn / Error 与光标；
- 删除现有独立 Prompt tag 行；
- 删除 separator + Prompt tag 的分散绘制路径；
- 删除 `draw_bottom_status()` 自带的第二份 `PROMPT` tag 与 `tag_width` 宽度扣减，底行整宽给状态文本；
- 保留独立底部状态行，仅在侧栏隐藏时绘制；
- 对话记录渲染跳过 `StatusPlacement::Dock(_)`。

## `crates/lattice-tui/src/app.rs`

- `Block::Status` 增加 placement；
- 增加 `DockSeverity` / `StatusPlacement`；
- 把现有 `push_status()` 拆成两个入口，使不变量由函数边界保证：

  ```rust
  /// 直接进对话记录。
  fn push_status(&mut self, kind: StatusKind);
  /// 先把当前停靠区 status 的 placement 改回 `Transcript`，再以
  /// `Dock(severity)` 追加新 status。turnover 全部在内部完成。
  fn push_dock_status(&mut self, kind: StatusKind, severity: DockSeverity);
  ```

- 按事件分类写入停靠区或对话记录；
- 无修饰 Space 实现 `WARN / ERROR -> PROMPT`；
- WARN / ERROR 下屏蔽不可见输入编辑；
- 输入高度常量收敛为停靠区内容 `1..=5`；
- `transcript_height()` 改用 `Dock::height()` 与底部状态行占用，删除硬编码 `+ 2` 和对 `input_visual_height()` 的依赖。

## `crates/lattice-system-harness/src/tui_mock.rs`

- 仅在 `run_tui_mock()` 中透传：
  - `TERM`
  - `COLORTERM`
  - `LANG`
  - `LC_ALL`

## 文档

同步更新：

- `crates/lattice-tui/AGENTS.md`：
  - 布局锁里的 `status_w = clamp(frame_w / 8, 16, 24)`；
  - 布局锁里的 `narrow frame = frame_w < 56`；
  - 布局锁里的 `transcript height = frame_h - (input_h + 2)`；
  - 三区示意与输入 `2..=5`。
- #182 视觉 spec 中已经过时的：
  - 动态侧栏宽度；
  - `<56` 窄屏分支；
  - 独立 Prompt tag 行；
  - Prompt 2..=5 行；
  - 「amber 进 TUI」禁止项：改为只解禁 `WARN` tag 填充色，其他区域仍禁；
  - 背景填充表从 2 处扩到 4 处（右栏 bg、`PROMPT`、`WARN`、`ERROR`）；
  - `signal-red-9` 同时作为前景色和 `ERROR` tag 填充色；
  - 宽窄底部结构，含窄屏底行不再带 `PROMPT` tag。
  - 「配色源：Cassette Futurism v2」改为指向仓库内的 `docs/cassette-futurism-palette.jsx`。

「第六种前景色」禁止项保持不变。

不新增 `render/` 模块，不新增 crate，不新增依赖。

---

# 18. 实施顺序

## 第一阶段：侧栏几何

1. `frame_layout` 接收宽、高；
2. 侧栏固定 40 列；
3. 使用 3:1 比例和 72 列正文下限；
4. 更新 draw 与 viewport 的 geometry 调用；
5. 更新现有布局纯函数测试。

这一阶段不碰停靠区状态。

## 第二阶段：TUI mock 环境

1. 在 `run_tui_mock()` 透传四个终端环境变量；
2. Ghostty 手动确认 RGB 背景与 Unicode rail；
3. 不增加专用单测。

## 第三阶段：停靠区

1. 为 Status block 增加 placement；
2. 实现 Warn / Error turnover；
3. 实现 Space 返回 Prompt；
4. 实现统一停靠区组件；
5. 将旧分隔线、input、Prompt tag 合并进停靠区；
6. 侧栏隐藏时保留独立底部状态行；
7. 更新对话记录过滤、行数与 viewport，包括把 `transcript_height()` 换成 `Dock::height()`；
8. 更新契约文档。

---

# 19. 测试计划

测试只覆盖稳定语义，不再为固定 UI 文案堆 `contains()` 测试。

## 19.1 侧栏几何：一组表驱动断言

```text
176×50 -> Visible, width 40, content 133
115×47 -> Hidden
85×47  -> Hidden
56×47  -> Hidden
80×24  -> Hidden
```

同时覆盖极小尺寸不 panic、不下溢。

## 19.2 停靠区状态与时间顺序：一组场景测试

覆盖：

```text
初始为 Prompt

LlmRetry A
-> A 只在 WARN 停靠区
-> 对话记录暂不显示 A

Info I
-> I 直接进入对话记录
-> 停靠区仍是 A

CommitFailed B
-> A 释放到对话记录
-> B 留在 ERROR 停靠区

Space
-> B 释放到对话记录
-> 停靠区回到 Prompt
-> Space 未插入 input
-> 原 input 草稿不变
-> 最终对话记录顺序仍为 A, I, B
```

同等级替换 `WARN A -> WARN B` 也应由同一组测试覆盖，证明旧状态不会丢失。

## 19.3 停靠区高度：一组纯函数断言

```text
Prompt 0 / 3 / 6 个视觉行 -> 内容 1 / 3 / 5，Dock::height() = 2 / 4 / 6
Warn   1 / 3 / 6 个视觉行 -> 内容 1 / 3 / 5，Dock::height() = 2 / 4 / 6
Error  1 / 3 / 6 个视觉行 -> 内容 1 / 3 / 5，Dock::height() = 2 / 4 / 6
```

Prompt 额外验证超过 5 行时光标仍可见。

## 19.4 渲染冒烟验证

更新现有宽窄渲染冒烟验证，确认：

- visible 布局只含对话记录 + 停靠区；
- hidden 布局含对话记录 + 停靠区 + 底部状态行；
- tiny terminal 不 panic。

不为 `PROMPT`、`SPACE → PROMPT`、`Ctrl+C to quit` 等固定文案再单独建立测试。

---

# 20. 手动验收

在 Ghostty 中运行 `tui-mock`：

1. `176×50`：
   - 显示固定 40 列侧栏；
   - 右栏为设计 RGB navy，而不是亮 ANSI Blue；
   - 左侧为对话记录 + 停靠区，无底部状态行。
2. `115×47`：
   - 不显示侧栏；
   - 显示独立底部状态行。
3. `85×47`：
   - 不显示侧栏。
4. `56×47`：
   - 不显示侧栏；
   - 停靠区和底部状态行不 panic、不下溢。
5. Error 场景：
   - 错误首次只在 ERROR 停靠区；
   - 分隔轨显示 `SPACE → PROMPT`；
   - 按 Space 后错误进入对话记录；
   - 停靠区显示 Prompt；
   - Space 不进入输入草稿。
6. Prompt 多行输入：
   - 最少 1 行；
   - 最多 5 行；
   - 超出后光标保持可见。

工程验证：

```text
cargo test -p lattice-tui
cargo clippy -p lattice-tui --all-targets -- -D warnings
cargo build -p lattice-cli --bin lattice
使用刚构建的 lattice 运行 pty_tui_smoke
workspace CI
```

---

# 21. 明确不做

本计划不包含：

- 停靠区的 `INFO` 状态；
- WARN 自动恢复；
- 定时自动回 Prompt；
- `/clear` 的新产品语义；
- 停靠区标签选择器；
- 鼠标、点击、焦点导航；
- 动画、闪烁、渐变；
- 通知队列、未读计数、时间戳；
- 停靠区历史或独立日志文件；
- Warn / Error 内部滚动；
- 完整错误摘要写进分隔轨；
- 动态侧栏宽度；
- 多个宽度窄屏分支；
- 终端品牌识别；
- `tput` / terminfo 能力探测；
- 环境透传专用单元测试；
- 固定文案的低信号 buffer `contains()` 测试；
- Theme / Palette trait；
- 新模块、新 crate、新依赖。

---

# 22. 完成定义

本计划完成时，应满足：

- 侧栏宽度恒为 40；
- 只有 `176×50` 这一类真正超宽终端显示侧栏；
- `115×47`、`85×47`、`56×47` 和普通 `80×24` 不显示侧栏；
- `tui-mock` 在 Ghostty 中正确继承真彩色和 locale；
- 停靠区是一个统一组件；
- 停靠区只有 Prompt、Warn、Error 三态；
- Info 永远直接进入对话记录；
- Warn / Error 首次只出现在停靠区；
- 停靠区 turnover 时旧 Warn / Error 在原始时间位置进入对话记录；
- Warn / Error 不自动消失；
- 无修饰 Space 是唯一的 `WARN / ERROR -> PROMPT` 操作；
- Space 被消费，输入草稿保持；
- 停靠区内容统一为 `1..=5` 行；
- 侧栏隐藏时才出现底部状态行；
- 不新增通知系统、主题系统或额外交互复杂度。
