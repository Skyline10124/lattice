# lattice-cli 重构建议

## 已发现的重构机会

### CLI1 — markdown.rs 1719 行，最大文件

**文件**: `src/frontend/tui/markdown.rs` (1719行)

**问题**: Markdown → ratatui 渲染器是最复杂的模块。包含：
- Markdown AST 解析（pulldown-cmark）
- 多种 block 渲染（paragraph, heading, code block, list, table, blockquote）
- Inline 渲染（bold, italic, code, link）
- 状态追踪（current line, width calculation）

**建议**: 拆分为子模块：
- `markdown/parser.rs` — AST 解析
- `markdown/block.rs` — block level 渲染
- `markdown/inline.rs` — inline level 渲染
- `markdown/mod.rs` — 统一接口

### CLI2 — app.rs 1718 行

**文件**: `src/frontend/tui/app.rs` (1718行)

**问题**: App 主逻辑包含所有交互处理：消息收发、stream 处理、slash 命令、session 管理。

**建议**: 
- `app/state.rs` — App 状态管理
- `app/stream.rs` — Stream 处理逻辑
- `app/commands.rs` — Slash 命令处理
- `app/session.rs` — Session 交互
- `app/mod.rs` — 主逻辑 + 事件循环

### CLI3 — run command 569 行

**文件**: `src/commands/run.rs` (569行)

**问题**: 单次运行命令逻辑包含 stream 输出、session 管理、错误处理。

**建议**: 提取 stream 渲染和 session 管理到共享 helper。

### CLI4 — main.rs 484 行，所有子命令在一处

**文件**: `src/main.rs` (484行)

**问题**: 所有 Commands enum 定义 + dispatch logic 在 main.rs。

**建议**: 
- `commands/dispatch.rs` — 从 Commands enum 到具体 command 的 dispatch
- `main.rs` 只做 CLI 定义 + top-level error handling

### CLI5 — Session SHA-256 ID vs Agent profile hash

**文件**: `src/session.rs`

**问题**: Session 使用 SHA-256 ID（来自 sha2 crate），但 Agent profile 使用 blake3 hash 验证。两个不同的 hash 算法用于类似目的（数据完整性）。

**建议**: 统一为一种算法。blake3 更快，SHA-256 更标准。功能上两者都足够。

### CLI6 — ChatMessage.clear_cache() dead code

**文件**: `src/frontend/tui/app.rs:117`

**问题**: `clear_cache()` method has `#[allow(dead_code)]`, never called anywhere. The cache is invalidated by checking width/content_len mismatch instead.

**建议**: Remove the method, or use it when message content is updated (e.g. streaming append).

### CLI7 — Manual base64_encode in app.rs

**文件**: `src/frontend/tui/app.rs:136-150`

**问题**: OSC52 clipboard support uses a manually implemented `base64_encode()` function instead of using the `base64` crate (which is already a transitive dependency via serde/reqwest).

**建议**: Use `base64::engine::general_purpose::Standard` for encoding. Remove the manual implementation.

### CLI8 — Markdown syntax detection only scans first 500 chars

**文件**: `src/frontend/tui/markdown.rs:24-59`

**问题**: `has_markdown_syntax()` only checks the first 500 chars. If a long plain-text message has markdown syntax after position 500, it won't be parsed as markdown. Conversely, if the first 500 chars have accidental `#` or `>` characters (e.g. code comments), plain text gets unnecessary markdown parsing.

**建议**: This is an acceptable heuristic for terminal rendering (long messages are rare in chat), but document the limitation in the function doc comment.

### CLI9 — run() command has 13 parameters

**文件**: `src/commands/run.rs:17-30`

**问题**: `run()` function has 13 parameters with `#[allow(clippy::too_many_arguments)]`. Hard to read, hard to extend, and any new flag requires modifying the function signature.

**建议**: Group parameters into a `RunConfig` struct. Session/credentials passed separately since they're stateful objects.

### CLI10 — DefaultToolExecutor::new().expect() in run.rs

**文件**: `src/commands/run.rs:57`

**问题**: `DefaultToolExecutor::new(".").expect("failed to build")` — same panic pattern as assembly.rs. If executor construction fails, CLI crashes.

**建议**: Same as B25 — change DefaultToolExecutor::new() to return Result, propagate error.

### CLI11 — Session::message_count() is dead code

**文件**: `src/session.rs:49-50`

**问题**: `#[allow(dead_code)]` on `message_count()` method. Never called anywhere in CLI codebase.

**建议**: Remove. If needed later, add back without the allow directive.

### CLI12 — display_events / EventRenderer 重复逻辑

**文件**: `src/commands/run.rs:428-486` (EventRenderer) + `src/commands/run.rs:488-559` (display_events)

**问题**: `EventRenderer::render()` 和 `display_events()` 对 LoopEvent 的处理逻辑几乎完全相同——两个函数都用相同的 match 分支处理 Token/Reasoning/ToolCallRequired/ToolResult/Done/Error。`flush_reasoning()` 也在两种路径中分别实现。

**建议**: 提取 `render_loop_event()` 共享函数，EventRenderer 和 display_events 都调用它。或者让 EventRenderer 统一处理两种模式（streaming/non-streaming）。

### CLI13 — run_pipeline() 也有过多参数

**文件**: `src/commands/run.rs:224-234`

**问题**: `run_pipeline()` 有 8 个参数，虽然比 run() 少但仍超出可读性阈值。

**建议**: 同 CLI9 建议，将 pipeline 相关参数归入 `PipelineConfig` struct。

### CLI14 — list_agents 手工 TOML 解析

**文件**: `src/commands/list_agents.rs:26-90`

**问题**: `list_agents` 命令手工逐行解析 TOML `[agent]` 段（name/model/tags），注释声称"避免拉入 serde + AgentProfile 依赖"，但 lattice-bus（含 AgentProfile）已是 lattice-cli 的直接依赖。手工解析脆弱：
- 不处理多行字符串值
- 不处理 TOML array of tables（如 `[[plugins.slots]]` 中的 name）
- 每次 TOML 格式变化都需手动适配

**建议**: 使用 `AgentProfile::load()` 做正式解析，直接取结构化字段。

### CLI15 — messages_for_agent() 丢失 tool_calls 数据

**文件**: `src/session.rs:215-236`

**问题**: `messages_for_agent()` 将 `SessionMessage` 转为 `lattice_core::types::Message` 时，所有 `tool_calls`、`reasoning_content`、`tool_call_id`、`name` 字段都设为 `None`。这意味着续会话时，Agent 收不到之前的 tool call 结果，无法继续 tool loop。这是一个数据丢失 bug。

**建议**: 扩展 `SessionMessage` 结构以保存 tool_calls/reasoning_content/tool_call_id，并在 `messages_for_agent()` 中正确还原。

### CLI16 — SessionManager dead code: from_dir + latest

**文件**: `src/session.rs:120-123` (from_dir) + `src/session.rs:200-207` (latest)

**问题**: `SessionManager::from_dir()` 和 `SessionManager::latest()` 都有 `#[allow(dead_code)]`。`latest()` 实际在 main.rs 的 `load_requested_session()` 中被使用（line 390），但 `from_dir()` 仅用于测试。

**建议**: `latest()` 不需要 `#[allow(dead_code)]`（已在用），移除标记。`from_dir()` 保留但改用 `#[cfg(test)]` 标注。

### CLI17 — safe_agents_dir 使用 process::exit 而非 Result

**文件**: `src/commands/mod.rs:32`

**问题**: `safe_agents_dir()` 在路径验证失败时调用 `std::process::exit(1)`，而非返回 `Result`。这使得函数无法在库/embedding 上下文中使用，也无法被上层捕获做友好错误提示。

**建议**: 改为返回 `Result<PathBuf, String>`，让 caller 决定如何处理错误。

### CLI18 — validate.rs 每个循环迭代创建新 Pipeline

**文件**: `src/commands/validate.rs:78-79`

**问题**: `validate` 命令在每个 profile 的 dry_run 循环中创建新的 `Pipeline::new("validate", Arc::new(registry.clone()), None, None)`，并 `Arc::new(registry.clone())` 克隆整个 registry 再包装。Pipeline 构造本身很轻但 registry clone 不轻。

**建议**: 在循环外构造 Pipeline，`Arc::new(registry)` 一次，循环内用 `Arc::clone`。

### CLI19 — launch_tui 8 个参数

**文件**: `src/main.rs:406-427`

**问题**: `launch_tui()` 有 8 个参数和 `#[allow(clippy::too_many_arguments)]`。与 CLI9（13 参数）和 CLI13（8 参数）同一模式。

**建议**: 归入 `TuiOptions` struct（已在 `frontend/tui::options_from_config` 中部分实现）。

### CLI20 — App struct 30+ 字段，god object

**文件**: `src/frontend/tui/app.rs:228-266`

**问题**: App 结构体有 30+ 字段（should_quit, messages, input, input_cursor, status, current_model, previous_model, current_provider, token_count, scroll_offset, help_open, transcript_mode, spinner_index, provider_override, credentials, save_sessions, session, pending_user, event_tx, agent, active_turn_id, next_turn_id, active_assistant_index, input_history, history_index, draft_before_history, click_zones, stream_started, reasoning_started, reasoning_duration, thinking_effort, context_limit, selection, visible_rows, visible_rows_origin, menu, clipboard），构成典型的 god object。状态管理困难，任何新功能都需要修改 App。

**建议**: 拆分为子结构体：
- `AppInput` — input, input_cursor, input_history, history_index, draft_before_history
- `AppStream` — stream_started, reasoning_started, reasoning_duration, active_turn_id, next_turn_id, active_assistant_index
- `AppSession` — session, save_sessions, pending_user, credentials

### CLI21 — handle_slash_command 290 行 match 块

**文件**: `src/frontend/tui/app.rs:965-1255`

**问题**: `handle_slash_command()` 是一个 290 行的 match 块，内联处理 12 个 slash 命令（/help, /clear, /new, /model, /status, /provider, /tokens, /trace, /effort, /expand, /copy, /save, /sessions, /quit）。每个分支直接修改 App 状态，无法独立测试。

**建议**: 提取每个命令为独立方法（如 `cmd_help()`, `cmd_model()`, `cmd_provider()` 等），`handle_slash_command` 只做 dispatch。也可考虑用 command trait 或 table-driven approach。

### CLI22 — submit() 中 LoopEvent match 是第 4 个副本

**文件**: `src/frontend/tui/app.rs:899-947`

**问题**: `submit()` 中的 `agent.run_streaming()` callback 包含 LoopEvent match（Token/Reasoning/ToolCallRequired/ToolResult/Done/Error），这是与 run.rs EventRenderer、run.rs display_events、debug.rs 之后的第 4 个 LoopEvent 处理副本。强化 CLI12 的重复逻辑发现。

**建议**: 提取统一的 `dispatch_loop_event(event, turn_id, tx)` 函数，所有 4 处调用共享。或者定义 `LoopEventAdapter` trait 让不同 UI 模式各自实现转换策略。

### CLI23 — Bash mode 无沙箱执行

**文件**: `src/frontend/tui/app.rs:747-790`

**问题**: TUI 的 `!command` bash 模式直接调用 `tokio::process::Command::new("bash")` 执行用户输入，没有经过沙箱、命令验证或安全检查。这完全绕过了 4 层防御模型（HookChain + metacharacters + env overrides + command allowlist）。

**建议**: 将 bash 模式命令通过 `DefaultToolExecutor` 或沙箱路径执行，至少应经过命令 allowlist 检查。或者明确文档说明 TUI bash mode 是特权操作（用户已在本机终端中）。

### CLI24 — trim_tool_result 硬编码 4000 字符截断

**文件**: `src/frontend/tui/app.rs:1538`

**问题**: `trim_tool_result()` 硬编码 `LIMIT = 4000`，不可配置。长工具输出被截断但用户无法调整。

**建议**: 改为从 App 或 Config 可配的常量，或至少使用 `const` 并在文档中说明。