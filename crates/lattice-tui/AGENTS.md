# lattice-tui 工作指引

## 职责

维护 ratatui/crossterm 全屏 TUI、输入、应用状态、对话记录与停靠区渲染，通过公开 lattice-api 执行当前会话的 Turn。CLI 参数解析和命令分派留在 lattice-cli，调用 `lattice_tui::run`；TUI 不反向依赖 CLI。

## 上位文档

修改前读取 [TUI 交互与视觉合同](../../docs/architecture/tui.md)。键位、布局尺寸、停靠区、配色、折叠指示及非目标均由该合同维护；取消和持久结果读取[会话合同](../../docs/architecture/session.md)，测试读取[测试工程](../../docs/development/testing.md)。色表是[设计资产](../../docs/reference/cassette-futurism-palette.md)，不代替实际用色规则。

## 局部约束

- 会话、profile、provider 问题在 UI 启动前报告，不静默失败。公开 lib/options/run 接口负责接线和阻塞池 Component 预热。
- session 模块负责打开/创建与 profile，app 负责事件循环、状态和 TurnEvent→Block，input 负责缓冲/历史/按键，view 负责布局/折行/viewport/停靠区。共享对话渲染归本 crate，目录和文件按职责组织，不保留旧五文件、render 目录冻结或切片行数上限。
- 协作取消通过 RunTurnRequest::cancel_flag，在 raw 模式处理 Esc/Ctrl+C KeyEvent；不另起 tokio::signal::ctrl_c watcher，不将取消请求当作已持久提交。
- REPL 历史尽力写入 `<session_dir>/repl_history.txt`，保持纯文本行格式。
- 所有布局使用统一 content_w 和安全整数运算，具体不变量引用合同；运行状态的显示不是 Invocation 持久结果权威。
- 目录组织调整不授权新增工具卡片或其他功能；卡片接线仍须以明确数据契约为依据。

## 验证入口

- `cargo test -p lattice-tui`。
- UI 接线变化重建 lattice-cli，指向当前产物运行 `pty_tui_smoke`；CLI 面变化关联 `cargo test -p lattice-cli`。
- 视觉与交互按合同进行必要人工终端核验，不以布局算术或快照替代体验判断；操作配方见[验证指南](../../docs/development/validation-guide.md)。
