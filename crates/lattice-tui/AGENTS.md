# lattice-tui 工作指引

维护基于 `ratatui` 与 `crossterm` 的全屏终端界面，经 `lattice-api::local::LocalClient` 操作与观察当前会话，被 `lattice-cli` 的 `tui` 子命令调用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | `TuiOptions` 与 `run` 入口，连接 `LocalClient` 并启动终端循环 |
| `src/app.rs` | 主事件循环，持有 `LocalTurn`、会话视图、未提交的 `/model` 与 `/effort` 选择并调度查询与重绘 |
| `src/session.rs` | 打开既有会话或准备新会话上下文，将对话条目按节点与调用身份转换为 `Block` |
| `src/connection.rs` | 将回执、会话头、连接与查询异常、重试提示、Turn 结局及 `DetachReport` 转换为定级状态消息与退出文案 |
| `src/blocks.rs` | 扁平显示 `Block` 列表与按 `NodeId`、`InvocationId` 原位更新 |
| `src/input.rs` | 输入区缓冲、按键处理、粘贴及 `<session_dir>/repl_history.txt` 纯文本历史读写 |
| `src/selection.rs` | `/model` 与 `/effort` 选择面板过滤、光标导航与换模型时的档位回退计算 |
| `src/comment.rs` | 助手正文块级锚定、`<lattice-comment>` 消息编解码与草稿管理 |
| `src/queue.rs` | 运行中排队消息缓冲、原地编辑、暂停与自动发出控制 |
| `src/markdown.rs`、`src/wrapping.rs` | GFM 正文、代码块高亮、diff、表格解析及按显示宽度的统一折行 |
| `src/view.rs`、`src/view/` | 统一 `content_w` 布局、viewport 与浏览光标（`browse.rs`）、评论卡片（`comments.rs`）、Dock（`dock.rs`）与 `statusline.rs` |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [TUI 交互与视觉](../../docs/architecture/tui/README.md)、[工程规范](../../docs/development/engineering.md) |
| 布局尺寸与 statusline | [布局](../../docs/architecture/tui/layout.md) |
| 对话记录、Markdown 与工具行 | [对话记录与工具显示](../../docs/architecture/tui/transcript.md) |
| 输入区、状态栏模块与 `/model`、`/effort` 面板 | [Dock 与输入区](../../docs/architecture/tui/dock.md) |
| 浏览光标、评论、排队与按键 | [浏览光标、评论、排队与键盘](../../docs/architecture/tui/interaction.md) |
| 配色与色阶取值 | [配色与终端约定](../../docs/architecture/tui/palette.md)（实际用色规则）、[Cassette Futurism 配色资产](../../docs/reference/cassette-futurism-palette.md)（原始色值表） |
| 取消、退出交接与持久结局 | [重开、取消与未完成状态](../../docs/architecture/session/recovery.md#取消与退出)、[测试工程](../../docs/development/testing.md) |

## 约束

- 既有会话打开与重连只调用只读查询；新会话在首次提交时由客户端生成 `SessionId` 并在后台接受时创建，提交被配置拒绝时展示 `ConfigRejection` 脱敏说明并保留当前输入，见[启动、提交与运行](../../docs/architecture/tui/README.md#启动提交与运行)。
- `/model` 与 `/effort` 候选项取自后台模型候选查询，选定值保存在 `app` 中随下一次提交发出，面板、档位回退与 statusline 规则见[选择面板](../../docs/architecture/tui/dock.md#选择面板)。
- `connection.rs` 集中生成状态消息文案并决定等级，Dock 只按等级决定暂驻或常驻，组件间状态与句柄交接见[状态消息与等级](../../docs/architecture/tui/README.md#状态消息与等级)与[组件接线](../../docs/architecture/tui/README.md#组件接线)。
- raw 模式下直接从终端输入事件捕获 `Ctrl+C`，经当前 `LocalTurn` 按 `TurnId` 发出取消意图，退出交接、视图推送替换、向上翻页与断连恢复显示按[连接、结局与重载](../../docs/architecture/tui/README.md#连接结局与重载)与[取消、退出与恢复提示](../../docs/architecture/tui/README.md#取消退出与恢复提示)执行。
- `input.rs` 将已提交输入按纯文本行尽力写入 `<session_dir>/repl_history.txt`；所有布局统一以 `content_w` 和饱和整数运算计算几何尺寸，见[水平与垂直分配](../../docs/architecture/tui/layout.md#水平与垂直分配)。
- 界面运行态仅反映实时观察进度，调用终态以会话库节点持久记录为准；新增工具卡片或交互元素以明确的数据契约与[功能边界](../../docs/architecture/tui/README.md#功能边界)为依据。
- 本 crate 之外：命令行参数解析与子命令分派由 `lattice-cli` 负责；IPC 连接分帧、profile/provider/凭据读取、Component 预热与持久 Turn 执行由 `lattice-api` 和 `lattice-llm` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-tui` |
| 修改前后端接线、选择面板或取消流程 | 重新构建 `lattice-cli` 后运行 `cargo test -p lattice-system-harness --test pty_tui_smoke` |
| 修改 `TuiOptions` 或启动参数接口 | `cargo test -p lattice-cli` |
| 修改布局、配色、折行或交互体验 | 按[验证指南](../../docs/development/validation-guide.md)在真实终端中人工核验视觉与交互表现 |
