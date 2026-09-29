# lattice-tui 工作指引

## 职责

维护 ratatui/crossterm 全屏 TUI、输入、应用状态、对话记录与停靠区渲染，通过公开 LocalClient 操作与观察当前会话。CLI 参数解析和命令分派留在 lattice-cli，调用 `lattice_tui::run`；TUI 不反向依赖 CLI。

## 上位文档

修改前读取 [TUI 交互与视觉合同](../../docs/architecture/tui.md)。键位、布局尺寸、停靠区、配色、折叠指示及非目标均由该合同维护；取消和持久结果读取[会话合同](../../docs/architecture/session.md)，测试读取[测试工程](../../docs/development/testing.md)。色表是[设计资产](../../docs/reference/cassette-futurism-palette.md)，不代替实际用色规则。

## 局部约束

- 新会话准备在 UI 启动前报告配置问题；既有会话与重连走只读入口，新提交才冻结配置。Component 预热与执行由后台所属 API 处理。
- session 负责打开已有会话、新会话的配置准备及 LocalHistory/消息到 Block 的转换；Session 在首次提交接受时创建。connection 只转换回执与退出报告文案。app 持有本客户端一次交互的具体句柄 LocalTurn，发出意图、消费更新，维护呈现与按 InvocationId/来源的工具配对；交付、取消回执、订阅和历史异步读取归 LocalClient。完整交接见 [TUI 组件接线](../../docs/architecture/tui.md#组件接线)。
- input 负责缓冲、历史与按键，view 负责布局、折行、viewport 和停靠区。共享渲染归本 crate，目录和文件按职责组织。
- raw 模式下 Esc/Ctrl+C 经 LocalTurn 登记取消意图，不另起 tokio::signal::ctrl_c watcher。退出、接受未知、快照重同步、尝试边界与连接中断的完整显示规则归 TUI 合同。
- REPL 历史尽力写入 `<session_dir>/repl_history.txt`，保持纯文本行格式。
- 所有布局使用统一 content_w 和安全整数运算，具体不变量引用合同；运行状态的显示不是 Invocation 持久结果权威。
- 目录组织调整不授权新增工具卡片或其他功能；卡片接线仍须以明确数据契约为依据。

## 验证入口

- `cargo test -p lattice-tui`。
- UI 接线变化重建 lattice-cli，指向当前产物运行 `pty_tui_smoke`；CLI 面变化关联 `cargo test -p lattice-cli`。
- 视觉与交互按合同进行必要人工终端核验，不以布局算术或快照替代体验判断；操作配方见[验证指南](../../docs/development/validation-guide.md)。
