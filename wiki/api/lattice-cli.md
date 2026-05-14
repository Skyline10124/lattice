# lattice-cli API

CLI 二进制 + TUI 终端界面（ratatui）。命令 dispatch 在 `main.rs` (484行)，TUI 在 `frontend/tui/`。

## CLI 子命令

```bash
lattice run <prompt> [--model <name>] [--file <path>] [--stream/--no-stream] [--session <id>] [--continue] [--provider <name>] [--pipeline <agent>] [--dry-run] [--system-prompt <text>] [--max-turns <n>]
lattice resolve <model> [--trace] [--provider <name>] [--json]
lattice models [--auth]
lattice doctor
lattice bus [--json] [--dir <path>]
lattice config {init | get <key> | set <key> <value> | hash <path>}
lattice sessions {list | show <id> | rm <id>}
lattice stats
lattice debug [--model <name>] [--prompt <text>] [--resolve-only] [--provider <name>]
lattice validate [--dir <path>]
lattice list agents
lattice new agent <name>
lattice tui [--model <name>] [--prompt <text>]
```

## Session 管理

```rust
pub struct Session {
    pub id: String,       // SHA-256 derived hex ID
    pub model: String,
    pub provider: String,
    pub title: Option<String>,
    pub messages: Vec<SessionMessage>,
    pub created_at: String,  // RFC 3339
    pub updated_at: String,  // RFC 3339
}

pub struct SessionMessage {
    pub role: String,     // "user", "assistant", "system", "tool"
    pub content: String,
}

pub struct SessionManager;
impl SessionManager {
    pub fn new() -> Self;
    pub fn save(&self, session: &Session) -> Result<()>;  // atomic write+rename
    pub fn load(&self, id: &str) -> Result<Option<Session>>;
    pub fn list(&self) -> Result<Vec<SessionSummary>>;  // sorted by updated_at desc
    pub fn delete(&self, id: &str) -> Result<bool>;
}
```

Session ID: 16-hex-char SHA-256 digest of `model|provider|nanos`. 持久化到 `~/.config/lattice/sessions/`，JSON 格式。

ID 验证：只允许 `[a-zA-Z0-9-_]`，最长 128 字符。文件名 = `{id}.json`。

### 辅助函数

```rust
pub fn messages_for_agent(session: &Session) -> Vec<Message>;  // 过滤 tool 消息，但丢失 tool_calls/reasoning_content/tool_call_id
pub fn finalize_session_turn(prev: Option<Session>, model, provider, user, assistant) -> Session;
```

> **已知问题**: `messages_for_agent()` 丢失 tool_calls/reasoning_content/tool_call_id — session 续会时 Agent 收不到之前的 tool call 结果（见已知问题 #29）。

## TUI 终端界面

位于 `src/frontend/tui/`，基于 ratatui + crossterm。

### App struct

```rust
pub struct App { ... }  // 1718行，最大 CLI 文件
```

关键功能：
- 流式聊天渲染（tokio::spawn background agent, StreamToken/ToolOutput/ModelInfo events → markdown → ratatui widgets）
- Slash 命令（/help, /quit, /clear, /model, /sessions, /effort 等）
- Session 自动持久化
- Catppuccin Mocha 主题
- 鼠标点击区域（ClickZone）和文本选择
- 粘性用户消息钉（scroll up 时顶部显示最后一条 user message）

### Event 系统

```rust
pub enum Event {
    Tick,                           // 250ms tick
    Key(KeyEvent),                  // keyboard input
    Mouse(MouseEvent),              // mouse click
    Paste(String),                  // bracketed paste
    StreamToken { turn_id, content, reasoning, done, error },  // LLM streaming
    ToolOutput { turn_id, name, arguments, result },           // tool execution
    ModelInfo { turn_id, model, provider },                    // resolved model info
}
```

`EventHandler` 使用 tokio unbounded channel，两个 spawn task：tick timer 和 crossterm poll。

### 模块结构

| 文件 | 作用 |
|------|------|
| `app.rs` (1718行) | App 主逻辑：消息收发、stream 处理、slash 命令 |
| `ui.rs` (664行) | ratatui UI 渲染：transcript, welcome, composer, menu overlay |
| `markdown.rs` (1719行) | Markdown → ratatui 渲染器（最复杂模块） |
| `event.rs` (87行) | 终端事件处理（tick + crossterm poll） |
| `theme.rs` (118行) | Catppuccin Mocha 颜色主题 |
| `widgets/message.rs` (235行) | 消息 widget：markdown 缓存、角色样式、折叠 |
| `widgets/statusline.rs` (140行) | 状态栏 widget：模型名、stream 进度、context bar |
| `mod.rs` (168行) | TUI 入口：TerminalGuard、TuiOptions、run_app |

### TuiOptions

```rust
pub struct TuiOptions {
    pub prompt: Option<String>,
    pub model: String,
    pub provider_override: Option<String>,
    pub credentials: HashMap<String, String>,
    pub save_sessions: bool,
    pub previous_session: Option<Session>,
}
```

## Config

```rust
pub struct Config {
    pub path: PathBuf,
    pub core: CoreConfig,  // default_model, stream, save_sessions
    pub ui: UiConfig,      // theme, show_reasoning
    pub providers: HashMap<String, ProviderConfig>,  // api_key, base_url
}
```

从 `~/.config/lattice/config.toml` 加载。`config hash` 子命令使用 SHA-256 计算 agent TOML 的完整性哈希。

## CredentialStore

```rust
pub struct CredentialStore {
    values: HashMap<String, String>,
}

pub const KNOWN_ENV_VARS: &[&str] = &["OPENAI_API_KEY", "ANTHROPIC_API_KEY", ...17 vars];

impl CredentialStore {
    pub fn from_config(config: &Config) -> Result<Self>;  // env vars + config literal keys
    pub fn to_hashmap(&self) -> HashMap<String, String>;    // for ModelRouter::with_credentials()
    pub fn diagnostics(&self) -> Vec<(&str, bool)>;         // which env vars are present
}
```

## Command modules

| 文件 | 作用 |
|------|------|
| `commands/run.rs` (569行) | run + run_pipeline, EventRenderer, display_events, dry_run_prompt |
| `commands/debug.rs` (200行) | debug: resolve + chat + agent summary |
| `commands/resolve.rs` (143行) | resolve: show provider/model/protocol |
| `commands/config_cmd.rs` (157行) | config: init/get/set/hash |
| `commands/list_agents.rs` (128行) | list agents: manual TOML parsing |
| `commands/validate.rs` (101行) | validate: AgentRegistry + dry_run |
| `commands/bus_status.rs` (106行) | bus status: LatticeDir discover |
| `commands/doctor.rs` (80行) | doctor: credentials + models + catalog |
| `commands/sessions.rs` (70行) | sessions: list/show/rm |
| `commands/new_agent.rs` (69行) | new agent: template generation |
| `commands/models.rs` (54行) | models: list authenticated/all |
| `commands/stats.rs` (36行) | stats: session statistics |
| `commands/mod.rs` (64行) | safe_agents_dir, validate_agents_dir |

## Display helpers

```rust
pub fn status_icon(ok: bool) -> &'static str;    // ✓ or ✗
pub fn credential_label(ok: bool) -> ColoredString;  // "set" or "missing"
```