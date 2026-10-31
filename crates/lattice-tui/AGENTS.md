# lattice-tui 工作指引

## 职责

维护 ratatui/crossterm 全屏 TUI、输入、应用状态、对话记录与停靠区渲染，通过公开 LocalClient 操作与观察当前会话。CLI 参数解析和命令分派留在 lattice-cli，调用 `lattice_tui::run`；TUI 不反向依赖 CLI。

## 上位文档

修改前读取 [TUI 交互与视觉合同](../../docs/architecture/tui.md)。键位、布局尺寸、Dock 与 statusline、评论与排队、配色、折叠指示及非目标均由该合同维护；取消和持久结果读取[会话合同](../../docs/architecture/session.md)，测试读取[测试工程](../../docs/development/testing.md)。色表是[设计资产](../../docs/reference/cassette-futurism-palette.md)，不代替实际用色规则。

## 局部约束

- TUI 不读取 profile、provider 配置或凭据；配置问题在提交时由后台拒绝，显示 API 给出的配置拒绝说明，输入保留。既有会话与重连走只读入口。Component 预热与执行由后台所属 API 处理。
- `/model` 与 `/effort` 的选择由 app 持有，随下一次提交交出，候选项来自后台的模型候选查询；面板、档位回退与 statusline 的已定规则见[选择面板](../../docs/architecture/tui.md#选择面板)。
- session 负责打开已有会话及对话条目到 Block 的转换；新会话的 SessionId 在首次提交时生成，Session 在首次提交接受时创建。connection 生成状态消息的文字并定级，转换退出报告文案；等级只在这里决定，是否暂驻由 Dock 按等级判断，见[状态消息与等级](../../docs/architecture/tui.md#状态消息与等级)。app 持有本客户端一次提交的具体句柄 LocalTurn 与当前会话的视图，发出提交、`watch`、查询与取消，按 NodeId 维护条目的显示；连接、视图与查询的转交及分帧归 LocalClient。完整交接见 [TUI 组件接线](../../docs/architecture/tui.md#组件接线)。
- input 负责缓冲、历史与按键，view 负责布局、折行、viewport、Dock 与 statusline。共享渲染归本 crate，目录和文件按职责组织。
- raw 模式下 Ctrl+C 按 TurnId 取消：本客户端提交的一轮经 LocalTurn 登记取消意图，不另起 tokio::signal::ctrl_c watcher。退出、接受未知、视图推送与路径替换、往上翻取更早的轮、尝试边界与连接中断的完整显示规则归 TUI 合同。
- REPL 历史尽力写入 `<session_dir>/repl_history.txt`，保持纯文本行格式。
- 所有布局使用统一 content_w 和安全整数运算，具体不变量引用合同；运行状态的显示不是 Invocation 持久结果权威。
- 目录组织调整不授权新增工具卡片或其他功能；卡片接线仍须以明确数据契约为依据。

## 验证入口

- 运行 `cargo test -p lattice-tui`。
- 改了 UI 接线时，重新构建 lattice-cli，用当前产物运行 `pty_tui_smoke`；改了 CLI 表面时，再运行 `cargo test -p lattice-cli`。
- 视觉与交互按合同在真实终端中人工核验，不能用布局计算或快照代替体验判断；操作步骤见[验证指南](../../docs/development/validation-guide.md)。
