# Async + Rust Native Refactoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate sync→async bridge layers (SHARED_RUNTIME, run_async, MEMORY_RT), convert tools to pure Rust async, and make the entire call chain async from CLI to Agent.

**Architecture:** Five-layer bottom-up refactor. Layer 1 converts ToolExecutor + tools to async with runtime bridge (run_async wrapper). Layer 2 eliminates the bridge by making Agent::run() async, cascading to AgentRunner → Pipeline. Layers 3-5 mechanically convert remaining callers (PluginDagRunner, CLI).

**Tech Stack:** tokio, async-trait, regex, reqwest (already in deps), thiserror

---

## File Map

| File | Responsibility | Action |
|------|---------------|--------|
| `lattice-agent/Cargo.toml` | Dependencies | Add async-trait, regex |
| `lattice-agent/src/lib.rs` | Agent, ToolExecutor trait, PluginAgent trait, SHARED_RUNTIME | Major refactor |
| `lattice-agent/src/tools.rs` | DefaultToolExecutor, 7 tool implementations | Async conversion + Rust native |
| `lattice-plugin/Cargo.toml` | Dependencies | Add tokio(time) |
| `lattice-plugin/src/erased_runner.rs` | run_plugin_loop, ErasedPluginRunner | Async cascade |
| `lattice-harness/src/runner.rs` | AgentRunner, MEMORY_RT | Async, delete MEMORY_RT |
| `lattice-harness/src/micro_agent.rs` | MicroAgent bus ops | Local RT replacement |
| `lattice-harness/src/pipeline.rs` | Pipeline::run(), run_fork() | SYNC_RT wrapper, delete sync fork |
| `lattice-harness/src/dag_runner.rs` | PluginDagRunner | .await cascade |
| `lattice-cli/src/commands/run.rs` | run_pipeline() | async |
| `lattice-cli/src/main.rs` | CLI entry | .await |

---

### Task 1: Add dependencies

**Files:**
- Modify: `lattice-agent/Cargo.toml`
- Modify: `lattice-plugin/Cargo.toml`

- [ ] **Step 1: Add async-trait and regex to lattice-agent**

In `lattice-agent/Cargo.toml`, add under `[dependencies]`:

```toml
async-trait = "0.1"
regex = "1"
```

- [ ] **Step 2: Add tokio(time) to lattice-plugin**

In `lattice-plugin/Cargo.toml`, add under `[dependencies]`:

```toml
tokio = { version = "1", features = ["time"] }
```

- [ ] **Step 3: Build check**

```bash
cargo build -p lattice-agent -p lattice-plugin
```

- [ ] **Step 4: Commit**

```bash
git add lattice-agent/Cargo.toml lattice-plugin/Cargo.toml
git commit -m "chore: add async-trait + regex to lattice-agent, tokio(time) to lattice-plugin"
```

---

### Task 2: Create ToolError enum

**Files:**
- Create: `lattice-agent/src/tool_error.rs`
- Modify: `lattice-agent/src/lib.rs:1-6` (add module declaration)

- [ ] **Step 1: Create ToolError enum**

Create `lattice-agent/src/tool_error.rs`:

```rust
/// Internal error type for tool execution. Not exposed in the ToolExecutor trait
/// signature — errors are converted to String via Display.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    #[error("IO error accessing {path}: {error}")]
    IoError {
        path: String,
        #[source]
        error: std::io::Error,
    },

    #[error("sandbox violation: {0}")]
    SandboxViolation(String),

    #[error("invalid regex pattern: {0}")]
    RegexError(String),

    #[error("HTTP error: {0}")]
    HttpError(String),

    #[error("command error: {0}")]
    CommandError(String),

    #[error("size limit exceeded: {actual} > {limit}")]
    SizeLimit { limit: usize, actual: usize },

    #[error("file not found: {0}")]
    FileNotFound(String),
}
```

- [ ] **Step 2: Register module in lib.rs**

In `lattice-agent/src/lib.rs`, after line 6 (`pub mod tools;`), add:

```rust
pub mod tool_error;
```

- [ ] **Step 3: Build check**

```bash
cargo build -p lattice-agent
```
Expected: compiles clean

- [ ] **Step 4: Commit**

```bash
git add lattice-agent/src/tool_error.rs lattice-agent/src/lib.rs
git commit -m "feat(agent): add ToolError enum with thiserror derives"
```

---

### Task 3: Make ToolExecutor trait async

**Files:**
- Modify: `lattice-agent/src/lib.rs:33-36`

- [ ] **Step 1: Change ToolExecutor trait to async**

In `lattice-agent/src/lib.rs`, replace lines 33-36:

```rust
/// Executes a tool call and returns the result string.
pub trait ToolExecutor: Send + Sync {
    fn execute(&self, call: &lattice_core::types::ToolCall) -> String;
}
```

with:

```rust
/// Executes a tool call and returns the result string.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &lattice_core::types::ToolCall) -> String;
}
```

- [ ] **Step 2: Add async_trait import at top**

In `lattice-agent/src/lib.rs`, after the existing imports (line ~14), add:

```rust
use async_trait::async_trait;
```

- [ ] **Step 3: Build check — expect compile errors in DefaultToolExecutor**

```bash
cargo build -p lattice-agent 2>&1 | head -20
```
Expected: errors in `tools.rs` — `execute` is now async but impl is sync. Acceptable at this step.

- [ ] **Step 4: Commit**

```bash
git add lattice-agent/src/lib.rs
git commit -m "feat(agent): make ToolExecutor trait async via #[async_trait]"
```

---

### Task 4: Convert read_file, write_file, list_directory to tokio::fs

**Files:**
- Modify: `lattice-agent/src/tools.rs:41-204`

- [ ] **Step 1: Convert read_file (lines 47-64)**

Replace the `read_file` arm body:

```rust
"read_file" => {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = self.sandbox.check_read(path) {
        return e;
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > self.sandbox.max_read_size as u64 => {
            return format!(
                "Sandbox: file size {} exceeds max_read_size {}",
                meta.len(),
                self.sandbox.max_read_size
            );
        }
        Err(e) => return format!("Error accessing {}: {}", path, e),
        _ => {}
    }
    std::fs::read_to_string(path).unwrap_or_else(|e| format!("Error: {}", e))
}
```

with:

```rust
"read_file" => {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = self.sandbox.check_read(path) {
        return e;
    }
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.len() > self.sandbox.max_read_size as u64 => {
            return format!(
                "Sandbox: file size {} exceeds max_read_size {}",
                meta.len(),
                self.sandbox.max_read_size
            );
        }
        Err(e) => return ToolError::IoError { path: path.to_string(), error: e }.to_string(),
        _ => {}
    }
    tokio::fs::read_to_string(path).await
        .unwrap_or_else(|e| ToolError::IoError { path: path.to_string(), error: e }.to_string())
}
```

- [ ] **Step 2: Convert write_file (lines 86-103)**

Replace `std::fs::write(&abs, content)` with `tokio::fs::write(&abs, content).await`, keeping sandbox checks the same. Wrap errors in `ToolError` Display.

```rust
"write_file" => {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let abs = format!("{}/{}", self.project_root, path.trim_start_matches('/'));
    if let Err(e) = self.sandbox.check_write(path) {
        return e;
    }
    if content.len() > self.sandbox.max_write_size {
        return ToolError::SizeLimit {
            limit: self.sandbox.max_write_size,
            actual: content.len(),
        }.to_string();
    }
    match tokio::fs::write(&abs, content).await {
        Ok(_) => format!("Wrote {} bytes to {}", content.len(), path),
        Err(e) => ToolError::IoError { path: abs, error: e }.to_string(),
    }
}
```

- [ ] **Step 3: Convert list_directory (lines 105-128)**

Replace `std::fs::read_dir(path)` with `tokio::fs::read_dir(path).await`. Collect entries from the async stream, sort, then format.

```rust
"list_directory" => {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    if let Err(e) = self.sandbox.check_read(path) {
        return e;
    }
    let mut entries = match tokio::fs::read_dir(path).await {
        Ok(dir) => dir,
        Err(e) => return ToolError::IoError { path: path.to_string(), error: e }.to_string(),
    };
    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await
        .unwrap_or_else(|e| {
            files.push(format!("Error reading entry: {}", e));
            None
        })
    {
        let ty = if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            "DIR"
        } else {
            "FILE"
        };
        files.push(format!("{}  {}", ty, entry.file_name().to_string_lossy()));
    }
    files.sort();
    files.join("\n")
}
```

- [ ] **Step 4: Build check**

```bash
cargo build -p lattice-agent 2>&1 | grep -c "error"
```
Expected: errors reduced — grep, bash, patch, web_search remaining.

- [ ] **Step 5: Commit**

```bash
git add lattice-agent/src/tools.rs
git commit -m "feat(agent): convert read_file/write_file/list_directory to tokio::fs"
```

---

### Task 5: Convert bash, patch, web_search to async

**Files:**
- Modify: `lattice-agent/src/tools.rs:130-206`

- [ ] **Step 1: Add http_client field to DefaultToolExecutor**

At `lattice-agent/src/tools.rs`, modify the struct definition:

```rust
pub struct DefaultToolExecutor {
    pub project_root: String,
    pub sandbox: SandboxConfig,
    pub http_client: reqwest::Client,
}

impl DefaultToolExecutor {
    pub fn new(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_string(),
            sandbox: SandboxConfig::default(),
            http_client: reqwest::Client::new(),
        }
    }

    pub fn with_sandbox(mut self, config: SandboxConfig) -> Self {
        self.sandbox = config;
        self
    }
}
```

- [ ] **Step 2: Convert bash (lines 130-146)**

Replace `std::process::Command::new("sh")...` with `tokio::process::Command::new("sh")...`:

```rust
"bash" => {
    let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = self.sandbox.check_command(cmd) {
        return e;
    }
    let output = tokio::process::Command::new("sh")
        .args(["-c", cmd])
        .output()
        .await;
    match output {
        Ok(o) => {
            let mut result = String::from_utf8_lossy(&o.stdout).to_string();
            if !o.stderr.is_empty() {
                result.push_str(&format!("\nERR:{}", String::from_utf8_lossy(&o.stderr)));
            }
            result
        }
        Err(e) => ToolError::CommandError(e.to_string()).to_string(),
    }
}
```

- [ ] **Step 3: Convert patch (lines 148-188)**

Replace `std::fs::read_to_string(&abs)` with `tokio::fs::read_to_string(&abs).await` and `std::fs::write(&abs, &new_content)` with `tokio::fs::write(&abs, &new_content).await`:

```rust
"patch" => {
    let path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
    let search = args.get("search").and_then(|v| v.as_str()).unwrap_or("");
    let insert = args.get("insert").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = self.sandbox.check_write(path) {
        return e;
    }
    let abs = format!("{}/{}", self.project_root, path.trim_start_matches('/'));
    match tokio::fs::read_to_string(&abs).await {
        Ok(content) => {
            let count = content.matches(search).count();
            if count == 0 {
                format!("Error: search text not found in {}", path)
            } else if count > 1 {
                format!(
                    "Error: search text found {} times in {}. Use a more specific search.",
                    count, path
                )
            } else {
                let new_content = content.replace(search, insert);
                match tokio::fs::write(&abs, &new_content).await {
                    Ok(_) => {
                        let diff_lines: Vec<String> = new_content
                            .lines()
                            .zip(content.lines())
                            .enumerate()
                            .filter(|(_, (a, b))| a != b)
                            .map(|(i, _)| {
                                let old_line = content.lines().nth(i).unwrap_or("");
                                let new_line = new_content.lines().nth(i).unwrap_or("");
                                format!("- {}\n+ {}", old_line, new_line)
                            })
                            .collect();
                        format!("Patched {}. Changes:\n{}", path, diff_lines.join("\n"))
                    }
                    Err(e) => ToolError::IoError { path: abs, error: e }.to_string(),
                }
            }
        }
        Err(e) => ToolError::IoError { path: abs, error: e }.to_string(),
    }
}
```

- [ ] **Step 4: Convert web_search (lines 190-201)**

Replace `reqwest::blocking::get(url)` with `self.http_client.get(url).send().await`:

```rust
"web_search" => {
    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = self.sandbox.check_url(url) {
        return e;
    }
    match self.http_client.get(url).send().await {
        Ok(response) => response
            .text()
            .await
            .unwrap_or_else(|e| ToolError::HttpError(e.to_string()).to_string()),
        Err(e) => ToolError::HttpError(e.to_string()).to_string(),
    }
}
```

- [ ] **Step 5: Build check**

```bash
cargo build -p lattice-agent 2>&1 | grep "error\["
```
Expected: only grep tool remains; also errors in `lib.rs` callers using `execute()` without `.await`.

- [ ] **Step 6: Commit**

```bash
git add lattice-agent/src/tools.rs
git commit -m "feat(agent): convert bash/patch/web_search to tokio async, add http_client to DefaultToolExecutor"
```

---

### Task 6: Convert grep to pure Rust regex

**Files:**
- Modify: `lattice-agent/src/tools.rs:65-85`

- [ ] **Step 1: Implement pure Rust grep**

Replace the `"grep"` arm (lines 65-85):

```rust
"grep" => {
    let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    if let Err(e) = self.sandbox.check_read(path) {
        return e;
    }

    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return ToolError::RegexError(e.to_string()).to_string(),
    };

    let mut results = Vec::new();
    grep_recursive(
        &re,
        std::path::Path::new(path),
        &mut results,
        &self.sandbox,
        0,
        &mut std::collections::HashSet::new(),
    )
    .await;

    if results.is_empty() {
        "(no matches)".to_string()
    } else {
        results.join("\n")
    }
}
```

- [ ] **Step 2: Add grep_recursive helper function**

After `impl ToolExecutor for DefaultToolExecutor` block (around line 205), add:

```rust
use crate::sandbox::SandboxConfig;

const GREP_MAX_DEPTH: u32 = 32;

async fn grep_recursive(
    pattern: &regex::Regex,
    path: &std::path::Path,
    results: &mut Vec<String>,
    sandbox: &SandboxConfig,
    depth: u32,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) {
    if depth > GREP_MAX_DEPTH {
        return;
    }

    // Resolve symlinks to detect cycles
    let canonical = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => return,
    };
    if !visited.insert(canonical) {
        return; // symlink cycle detected
    }

    let path_str = path.to_string_lossy();
    if sandbox.check_read(&path_str).is_err() {
        return;
    }

    if path.is_file() {
        // Skip files that look binary
        match tokio::fs::read_to_string(path).await {
            Ok(content) => {
                if content.contains('\0') {
                    return; // binary file
                }
                if content.len() > sandbox.max_read_size {
                    return;
                }
                for (line_num, line) in content.lines().enumerate() {
                    if pattern.is_match(line) {
                        results.push(format!("{}:{}:{}", path_str, line_num + 1, line));
                    }
                }
            }
            Err(_) => {} // skip files we can't read
        }
    } else if path.is_dir() {
        let mut entries = match tokio::fs::read_dir(path).await {
            Ok(d) => d,
            Err(_) => return,
        };
        let mut children = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            // Skip hidden directories
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') && name_str != "." && name_str != ".." {
                continue;
            }
            children.push(entry.path());
        }
        for child_path in children {
            Box::pin(grep_recursive(pattern, &child_path, results, sandbox, depth + 1, visited)).await;
        }
    }
}
```

- [ ] **Step 3: Add required import to tools.rs**

At the top of `tools.rs`, add:

```rust
use std::collections::HashSet;
```

- [ ] **Step 4: Build check**

```bash
cargo build -p lattice-agent 2>&1
```
Expected: `tools.rs` compiles. Errors should now be in `lib.rs` callers.

- [ ] **Step 5: Commit**

```bash
git add lattice-agent/src/tools.rs
git commit -m "feat(agent): replace shell-out grep with pure Rust regex recursive search"
```

---

### Task 7: Bridge Agent callers — use run_async() for executor.execute()

**Files:**
- Modify: `lattice-agent/src/lib.rs:149-284` (run() and run_async())

- [ ] **Step 1: Update sync run() to bridge executor.execute()**

In `Agent::run()` at line 194, change:

```rust
let result = executor.execute(call);
```

to:

```rust
let result = run_async(executor.execute(call));
```

- [ ] **Step 2: Update async run_async() to .await executor.execute()**

In `Agent::run_async()` at line 277, change:

```rust
let result = executor.execute(call);
```

to:

```rust
let result = executor.execute(call).await;
```

- [ ] **Step 3: Build check**

```bash
cargo build -p lattice-agent 2>&1
```
Expected: compiles. All bridge calls now consistent.

- [ ] **Step 4: Commit**

```bash
git add lattice-agent/src/lib.rs
git commit -m "fix(agent): bridge executor.execute() via run_async in sync run(), .await in async run_async()"
```

---

### Task 8: Make PluginAgent trait async

**Files:**
- Modify: `lattice-agent/src/lib.rs:44-60` (trait definition)
- Modify: `lattice-agent/src/lib.rs:558-594` (impl PluginAgent for Agent)

- [ ] **Step 1: Make PluginAgent trait async**

Replace lines 44-60:

```rust
/// Minimal interface for an LLM-calling agent.
/// Used by PluginRunner to call any agent that implements send + system_prompt.
#[async_trait]
pub trait PluginAgent {
    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;
    /// Send a user message and automatically handle tool calls via Agent::run().
    async fn send_message_with_tools(
        &mut self,
        message: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        // Default: delegate to send() for backward compat with non-Agent impls
        self.send(message).await
    }
    fn set_system_prompt(&mut self, _prompt: &str) {}
    fn token_usage(&self) -> u64 {
        0
    }
}
```

- [ ] **Step 2: Update impl PluginAgent for Agent**

Replace lines 558-594 with async methods. `send()` now uses `self.send_message(message)` which is still sync at this point — bridge it:

```rust
impl PluginAgent for Agent {
    fn set_system_prompt(&mut self, prompt: &str) {
        self.state.push_system_message(prompt);
    }

    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>> {
        let events = self.send_message(message);
        let mut content = String::new();
        let mut has_error = false;
        for event in &events {
            match event {
                LoopEvent::Token { text } => content.push_str(text),
                LoopEvent::Error { .. } => has_error = true,
                _ => {}
            }
        }
        if has_error && content.is_empty() {
            Err("Agent returned an error with no content".into())
        } else {
            Ok(content)
        }
    }

    async fn send_message_with_tools(
        &mut self,
        message: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let events = self.run(message, MAX_TOOL_TURNS);
        let mut content = String::new();
        for event in &events {
            if let LoopEvent::Token { text } = event {
                content.push_str(text);
            }
        }
        Ok(content)
    }
}
```

- [ ] **Step 3: Update erased_runner.rs for async PluginAgent calls**

In `lattice-plugin/src/erased_runner.rs`, in `run_plugin_loop()` line 38-40, change:

```rust
let raw = agent
    .send_message_with_tools(&prompt)
    .map_err(|e| PluginError::Other(e.to_string()))?;
```

Since `send_message_with_tools` is now async, we need a temporary bridge. `run_plugin_loop` is still sync at this stage:

```rust
use std::sync::LazyLock;
static AGENT_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("agent runtime")
});

// In run_plugin_loop:
let raw = AGENT_RT
    .block_on(agent.send_message_with_tools(&prompt))
    .map_err(|e| PluginError::Other(e.to_string()))?;
```

Also replace `std::thread::sleep` with `AGENT_RT.block_on(tokio::time::sleep(...))`:

```rust
// Line 77:
AGENT_RT.block_on(tokio::time::sleep(p.jittered_backoff(attempt)));

// Line 90:
AGENT_RT.block_on(tokio::time::sleep(p.jittered_backoff(attempt)));
```

- [ ] **Step 4: Build full workspace**

```bash
cargo build --workspace 2>&1
```
Expected: compiles. All async trait calls bridged.

- [ ] **Step 5: Commit**

```bash
git add lattice-agent/src/lib.rs lattice-plugin/src/erased_runner.rs
git commit -m "feat: make PluginAgent trait async, bridge calls in erased_runner"
```

---

### Task 9: Run tests — verify Layer 1 is complete

**Files:**
- All test files in affected crates

- [ ] **Step 1: Run lattice-agent tests**

```bash
cargo test -p lattice-agent 2>&1
```
Expected: all tests pass (MockAgent tests auto-compatible via #[async_trait]).

- [ ] **Step 2: Run lattice-plugin tests**

```bash
cargo test -p lattice-plugin 2>&1
```
Expected: all tests pass.

- [ ] **Step 3: Run lattice-harness tests**

```bash
cargo test -p lattice-harness 2>&1
```
Expected: all tests pass (if any fail, fix in next tasks).

- [ ] **Step 4: Commit if any fixes made, or mark Layer 1 complete**

No commit needed unless test fixes were required.

---

### Task 10: Layer 2 — Merge run_chat() + run_chat_async() into single async run_chat()

**Files:**
- Modify: `lattice-agent/src/lib.rs:286-474`

- [ ] **Step 1: Rename the fn — delete run_chat() sync, rename run_chat_async() to run_chat()**

Delete lines 286-381 (`fn run_chat() sync`). Rename `fn run_chat_async` at line 383 to `fn run_chat`:

```rust
async fn run_chat(&mut self) -> Vec<LoopEvent> {
    use futures::StreamExt;

    let mut stream = match self.chat_with_retry().await {  // was chat_with_retry_async
        Ok(s) => s,
        Err(e) => {
            return vec![LoopEvent::Error {
                message: e.to_string(),
            }]
        }
    };

    let mut events = Vec::new();
    let mut content_buf = String::new();
    let mut reasoning_buf = String::new();
    let mut tool_builders: HashMap<String, ToolCallAccum> = HashMap::new();

    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Token { content: c } => {
                content_buf.push_str(&c);
                events.push(LoopEvent::Token { text: c });
            }
            StreamEvent::Reasoning { content: r } => {
                reasoning_buf.push_str(&r);
                events.push(LoopEvent::Reasoning { text: r });
            }
            StreamEvent::ToolCallStart { id, name } => {
                tool_builders.insert(id, ToolCallAccum { name, arguments: String::new() });
            }
            StreamEvent::ToolCallDelta { id, arguments_delta } => {
                if let Some(tc) = tool_builders.get_mut(&id) {
                    tc.arguments.push_str(&arguments_delta);
                }
            }
            StreamEvent::ToolCallEnd { .. } => {}
            StreamEvent::Done { usage, .. } => {
                if let Some(ref u) = usage {
                    self.state.add_token_usage(u.total_tokens as u64);
                }
                if !tool_builders.is_empty() {
                    let calls: Vec<lattice_core::types::ToolCall> = tool_builders
                        .iter()
                        .map(|(id, tc)| lattice_core::types::ToolCall {
                            id: id.clone(),
                            function: lattice_core::types::FunctionCall {
                                name: tc.name.clone(),
                                arguments: tc.arguments.clone(),
                            },
                        })
                        .collect();
                    events.push(LoopEvent::ToolCallRequired { calls });
                }
                events.push(LoopEvent::Done { usage });
            }
            StreamEvent::Error { message } => {
                events.push(LoopEvent::Error { message });
            }
        }
    }

    let tool_calls = if tool_builders.is_empty() {
        None
    } else {
        Some(tool_builders.into_iter()
            .map(|(id, tc)| lattice_core::types::ToolCall {
                id,
                function: lattice_core::types::FunctionCall { name: tc.name, arguments: tc.arguments },
            })
            .collect())
    };

    self.state.push_assistant_message(&content_buf, &reasoning_buf, tool_calls);
    events
}
```

- [ ] **Step 2: Rename chat_with_retry — delete sync, rename _async**

Delete lines 476-506 (`chat_with_retry()` sync). Rename `chat_with_retry_async` at line 508 to `chat_with_retry`:

```rust
async fn chat_with_retry(
    &self,
) -> Result<
    std::pin::Pin<Box<dyn futures::Stream<Item = StreamEvent> + Send>>,
    lattice_core::LatticeError,
> {
    use lattice_core::errors::ErrorClassifier;
    let mut attempt = 0u32;

    loop {
        match lattice_core::chat(&self.state.resolved, &self.state.messages, &self.tools).await
        {
            Ok(stream) => return Ok(stream),
            Err(ref e) => {
                if attempt >= self.retry.max_retries || !ErrorClassifier::is_retryable(e) {
                    return Err(e.clone());
                }
                let delay = self.retry.jittered_backoff(attempt);
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}
```

- [ ] **Step 3: Build check**

```bash
cargo build -p lattice-agent 2>&1
```
Expected: errors in callers of the deleted sync `run_chat()`. Acceptable.

- [ ] **Step 4: Commit**

```bash
git add lattice-agent/src/lib.rs
git commit -m "refactor(agent): merge run_chat sync+async into single async fn, same for chat_with_retry"
```

---

### Task 11: Layer 2 — Merge Agent::run() + run_async() into single async run()

**Files:**
- Modify: `lattice-agent/src/lib.rs:149-284`

- [ ] **Step 1: Delete sync run() (lines 149-233)**

Delete the sync `fn run()` completely.

- [ ] **Step 2: Rename run_async() to run(), remove run_async() bridges**

Rename `pub async fn run_async` at line 235 to `pub async fn run`. In the body, `executor.execute(call)` already uses `.await` (from Task 7), and `self.run_chat()` now returns the async version (from Task 10):

```rust
pub async fn run(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent> {
    self.state.push_user_message(content);
    let mut all_events = Vec::new();

    for _ in 0..max_turns {
        let context_len = if self.state.resolved.context_length > 0 {
            self.state.resolved.context_length
        } else {
            131072
        };
        self.state.trim_messages(context_len, 15);

        let mut events = self.run_chat().await;

        let mut retry_count = 0u32;
        while retry_count < MAX_STREAM_RETRIES {
            let has_error = events.iter().any(|e| matches!(e, LoopEvent::Error { .. }));
            if !has_error {
                break;
            }
            self.state.pop_last_assistant_message();
            retry_count += 1;
            events = self.run_chat().await;
        }

        let mut tool_calls = Vec::new();
        for event in &events {
            if let LoopEvent::ToolCallRequired { calls } = event {
                tool_calls.extend(calls.clone());
            }
        }

        all_events.extend(events);

        if tool_calls.is_empty() {
            break;
        }
        if self.tool_executor.is_none() {
            break;
        }
        if let Some(ref executor) = self.tool_executor {
            for call in &tool_calls {
                let result = executor.execute(call).await;
                self.state.push_tool_result(&call.id, &result, None);
            }
        }
    }

    // Auto-save memory entry (unchanged)
    if let Some(ref memory) = self.memory {
        let prompt_summary = if content.len() > 200 {
            let mut end = 200;
            while end > 0 && !content.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &content[..end])
        } else {
            content.to_string()
        };
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let entry = crate::memory::MemoryEntry {
            id: format!("{}-{}", now_secs, self.state.token_usage),
            kind: crate::memory::EntryKind::SessionLog,
            session_id: self.state.resolved.canonical_id.clone(),
            summary: format!(
                "Model: {} | Provider: {} | Tokens: {}",
                self.state.resolved.api_model_id,
                self.state.resolved.provider,
                self.state.token_usage
            ),
            content: prompt_summary,
            tags: vec![],
            created_at: format!("{now_secs}"),
        };
        memory.save_entry(entry);
    }

    all_events
}
```

- [ ] **Step 2b: Delete SHARED_RUNTIME and run_async() helper**

Delete lines 16-31 (the `run_async()` helper function and `SHARED_RUNTIME` static). Remove the `LazyLock::force(&SHARED_RUNTIME)` call from `Agent::new()` (line 79).

- [ ] **Step 3: Update send_message(), submit_tools(), and send_message_async()**

`send_message()` now refers to the async `run_chat()`:

```rust
pub async fn send_message(&mut self, content: &str) -> Vec<LoopEvent> {
    self.state.push_user_message(content);
    self.run_chat().await
}
```

`send_message_async()` becomes redundant — delete it or make it a caller of `send_message().await`.

`submit_tools()`:

```rust
pub async fn submit_tools(
    &mut self,
    results: Vec<(String, String)>,
    max_size: Option<usize>,
) -> Vec<LoopEvent> {
    for (call_id, result) in &results {
        self.state.push_tool_result(call_id, result, max_size);
    }
    self.run_chat().await
}
```

- [ ] **Step 4: Update impl PluginAgent for Agent — remove sync bridges**

Now that `send_message()` and `run()` are async:

```rust
impl PluginAgent for Agent {
    fn set_system_prompt(&mut self, prompt: &str) {
        self.state.push_system_message(prompt);
    }

    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>> {
        let events = self.send_message(message).await;
        let mut content = String::new();
        let mut has_error = false;
        for event in &events {
            match event {
                LoopEvent::Token { text } => content.push_str(text),
                LoopEvent::Error { .. } => has_error = true,
                _ => {}
            }
        }
        if has_error && content.is_empty() {
            Err("Agent returned an error with no content".into())
        } else {
            Ok(content)
        }
    }

    async fn send_message_with_tools(
        &mut self,
        message: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let events = self.run(message, MAX_TOOL_TURNS).await;
        let mut content = String::new();
        for event in &events {
            if let LoopEvent::Token { text } = event {
                content.push_str(text);
            }
        }
        Ok(content)
    }
}
```

- [ ] **Step 5: Build check**

```bash
cargo build -p lattice-agent 2>&1
```
Expected: compiles. `SHARED_RUNTIME` and `run_async()` are gone.

- [ ] **Step 6: Commit**

```bash
git add lattice-agent/src/lib.rs
git commit -m "refactor(agent): merge run+run_async into single async fn, delete SHARED_RUNTIME+run_async() helper"
```

---

### Task 12: Layer 2 — Cascade to AgentRunner

**Files:**
- Modify: `lattice-harness/src/runner.rs`

- [ ] **Step 1: Delete MEMORY_RT, make AgentRunner::run() async**

Delete lines 14-17 (`MEMORY_RT` static and imports).

Make `AgentRunner::run()` async — add `async` to the signature and change `self.run_once()` calls to `.await`:

```rust
pub async fn run(
    &mut self,
    input: &str,
    max_turns: u32,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let schema = /* ... schema setup unchanged ... */;

    let enriched_input = self.enrich_input(input);
    let mut output = self.run_once(&enriched_input, max_turns).await?;

    // JSON Schema validation + retry loop
    if let Some((ref schema_json, ref validator)) = schema {
        for retry in 0..MAX_SCHEMA_RETRIES {
            let mut errors = validator.iter_errors(&output);
            let first_error = errors.next();
            if first_error.is_none() {
                break;
            }
            // ... correction hint unchanged ...
            output = self.run_once(&correction_hint, max_turns).await?;
        }
    }
    Ok(output)
}
```

Make `run_once()` async — change `self.agent.run(input, max_turns)` to `self.agent.run(input, max_turns).await`:

```rust
async fn run_once(
    &mut self,
    input: &str,
    max_turns: u32,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let events = self.agent.run(input, max_turns).await;
    // ... rest unchanged ...
}
```

- [ ] **Step 2: Build check — lattice-harness**

```bash
cargo build -p lattice-harness 2>&1
```
Expected: errors in pipeline.rs and micro_agent.rs (callers of MEMORY_RT and runner.run()).

- [ ] **Step 3: Commit**

```bash
git add lattice-harness/src/runner.rs
git commit -m "refactor(harness): AgentRunner::run() async, delete MEMORY_RT"
```

---

### Task 13: Layer 2 — MicroAgent local RT

**Files:**
- Modify: `lattice-harness/src/micro_agent.rs`

- [ ] **Step 1: Replace MEMORY_RT import with local RT**

Remove line 13: `use crate::runner::MEMORY_RT;`

Add after imports:

```rust
use std::sync::LazyLock;
static BUS_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("micro_agent bus runtime")
});
```

- [ ] **Step 2: Replace all MEMORY_RT usage with BUS_RT**

Find and replace all `MEMORY_RT.block_on(...)` calls with `BUS_RT.block_on(...)`.

```bash
grep -n "MEMORY_RT" lattice-harness/src/micro_agent.rs
```
For each occurrence, replace `MEMORY_RT` with `BUS_RT`.

- [ ] **Step 3: Build check**

```bash
cargo build -p lattice-harness 2>&1
```
Expected: micro_agent.rs compiles.

- [ ] **Step 4: Commit**

```bash
git add lattice-harness/src/micro_agent.rs
git commit -m "refactor(harness): replace MEMORY_RT with local BUS_RT in MicroAgent"
```

---

### Task 14: Layer 2 — Pipeline::run() sync wrapper + delete sync fork

**Files:**
- Modify: `lattice-harness/src/pipeline.rs`

- [ ] **Step 1: Replace Pipeline::run() with SYNC_RT delegation**

Replace the ~380-line `pub fn run()` (lines 132-523) with a thin wrapper:

```rust
use std::sync::LazyLock;
static SYNC_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("pipeline sync runtime")
});

/// Sync wrapper — delegates to run_async() for CLI callers.
pub fn run(&mut self, start_agent: &str, input: &str) -> PipelineRun {
    SYNC_RT.block_on(self.run_async(start_agent, input))
}
```

- [ ] **Step 2: Update Pipeline::run_async() — use .await on runner.run()**

At line 594, change `runner.run(&current_input, agent_max_turns)` to `runner.run(&current_input, agent_max_turns).await`.

Also find all other `runner.run(...)` calls in `run_async()` and add `.await`. Check the fork section at line 440:

```rust
match runner.run(&current_input, agent_max_turns)  // line 594
```

Change to:

```rust
match runner.run(&current_input, agent_max_turns).await
```

- [ ] **Step 3: Delete sync run_fork()**

Delete lines 731-845 (`fn run_fork()` and its body). All fork execution now goes through `run_fork_async()`.

- [ ] **Step 4: Build check**

```bash
cargo build -p lattice-harness 2>&1
```
Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add lattice-harness/src/pipeline.rs
git commit -m "refactor(harness): Pipeline::run() delegates to run_async via SYNC_RT, delete sync run_fork()"
```

---

### Task 15: Layer 3 — PluginDagRunner + run_plugin_loop async

**Files:**
- Modify: `lattice-plugin/src/erased_runner.rs`
- Modify: `lattice-harness/src/dag_runner.rs`

- [ ] **Step 1: Make run_plugin_loop async**

In `lattice-plugin/src/erased_runner.rs`, make `run_plugin_loop` async. Remove the `AGENT_RT` bridge added in Task 8, replace with direct `.await`:

```rust
pub async fn run_plugin_loop(
    plugin: &dyn ErasedPlugin,
    behavior: &dyn crate::Behavior,
    agent: &mut dyn lattice_agent::PluginAgent,
    context: &serde_json::Value,
    config: &PluginConfig,
    hooks: Option<&dyn PluginHooks>,
    retry_policy: Option<&RetryPolicy>,
    memory: Option<&dyn lattice_agent::memory::Memory>,
) -> Result<RunResult, PluginError> {
    let prompt = plugin.to_prompt_json(context)?;
    let mut attempt = 0u32;

    if let Some(h) = hooks {
        h.on_start(plugin.name(), (prompt.len() as u32).div_ceil(4));
    }

    loop {
        if attempt >= config.max_turns {
            return Err(PluginError::MaxTurnsExceeded(config.max_turns));
        }

        let raw = agent
            .send_message_with_tools(&prompt)
            .await
            .map_err(|e| PluginError::Other(e.to_string()))?;

        match plugin.parse_output_json(&raw) {
            Ok(output) => {
                let confidence = extract_confidence(&raw);
                let action = behavior.decide(confidence);

                if let Some(h) = hooks {
                    h.on_turn(attempt, None, &action);
                }

                match action {
                    Action::Done => {
                        let json = serde_json::to_string(&output)
                            .map_err(|e| PluginError::Other(e.to_string()))?;
                        if json.len() > config.max_output_bytes {
                            return Err(PluginError::OutputTooLarge(
                                json.len(),
                                config.max_output_bytes,
                            ));
                        }
                        let result = RunResult {
                            output: json,
                            turns: attempt + 1,
                            final_action: Action::Done,
                        };
                        if let Some(h) = hooks {
                            h.on_complete(&result);
                        }
                        if let Some(mem) = memory {
                            save_memory_entries(mem, plugin.name(), &prompt, &result);
                        }
                        return Ok(result);
                    }
                    Action::Retry => {
                        attempt += 1;
                        if let Some(p) = retry_policy {
                            tokio::time::sleep(p.jittered_backoff(attempt)).await;
                        }
                    }
                }
            }
            Err(e) => {
                if let Some(h) = hooks {
                    h.on_error(attempt, &e);
                }
                match behavior.on_error(&e, attempt) {
                    crate::ErrorAction::Retry => {
                        attempt += 1;
                        if let Some(p) = retry_policy {
                            tokio::time::sleep(p.jittered_backoff(attempt)).await;
                        }
                    }
                    crate::ErrorAction::Abort => return Err(e),
                    crate::ErrorAction::Escalate => {
                        return Err(PluginError::Escalated {
                            original: Box::new(e),
                            after_attempts: attempt,
                        });
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 2: Make ErasedPluginRunner::run() async**

```rust
pub async fn run(&mut self, context: &serde_json::Value) -> Result<RunResult, PluginError> {
    run_plugin_loop(
        self.plugin, self.behavior, self.agent, context,
        self.config, self.hooks, self.retry_policy, self.memory,
    ).await
}
```

- [ ] **Step 3: Update PluginDagRunner in dag_runner.rs**

At line 161, change `run_plugin_loop(...)` to `run_plugin_loop(...).await`:

```rust
let result = run_plugin_loop(
    bundle.plugin.as_ref(),
    behavior.as_ref(),
    &mut agent,
    &context,
    &plugin_config,
    None,
    Some(&self.retry_policy),
    self.shared_memory.as_deref().map(|m| m as &dyn Memory),
)
.await
.map_err(|e| DAGError::plugin_error(&current_name, e))?;
```

Make `PluginDagRunner::run()` async:

```rust
pub async fn run(
    &mut self,
    initial_input: &str,
    default_model: &str,
) -> Result<serde_json::Value, DAGError> {
    // ... body unchanged, all .await calls already present from run_plugin_loop.await
}
```

- [ ] **Step 4: Update typed PluginRunner call site in lib.rs**

In `lattice-plugin/src/lib.rs`, find the `run_plugin_loop(...)` call (around line 298) and add `.await?`:

```rust
// Current (sync):
crate::erased_runner::run_plugin_loop(
    &self.inner, self.behavior.as_ref(), agent, context,
    config, None, None, None,
)

// Target (async):
crate::erased_runner::run_plugin_loop(
    &self.inner, self.behavior.as_ref(), agent, context,
    config, None, None, None,
).await
```

- [ ] **Step 5: Build check**

```bash
cargo build -p lattice-plugin -p lattice-harness 2>&1
```
Expected: compiles.

- [ ] **Step 6: Commit**

```bash
git add lattice-plugin/src/erased_runner.rs lattice-plugin/src/lib.rs lattice-harness/src/dag_runner.rs
git commit -m "refactor: run_plugin_loop + PluginDagRunner async, delete AGENT_RT bridge"
```

---

### Task 16: Layer 4 — CLI entry async

**Files:**
- Modify: `lattice-cli/src/commands/run.rs`
- Modify: `lattice-cli/src/main.rs`

- [ ] **Step 1: Make run_pipeline() async**

In `lattice-cli/src/commands/run.rs`, make `run_pipeline` async. The `Pipeline::run()` is now a sync wrapper delegating to `run_async()`, but we want the CLI to use `run_async()` directly:

```rust
pub async fn run_pipeline(
    prompt: &str,
    start_agent: &str,
    agents_dir: Option<&str>,
    _plugins_dir: Option<&str>,
    _tools_dir: Option<&str>,
    verbose: bool,
    json: bool,
) -> Result<()> {
    // ... registry setup unchanged ...

    // Run the pipeline
    let mut pipeline = Pipeline::new(start_agent, registry, None, None)
        .with_plugin_registry(plugin_registry)
        .with_tool_registry(tool_registry);
    let result = pipeline.run_async(start_agent, prompt).await;

    // ... output handling unchanged ...
}
```

- [ ] **Step 2: Update main.rs call site**

In `lattice-cli/src/main.rs`, find where `run_pipeline()` is called in the `Run::pipeline` branch (around line 206) and add `.await`:

```rust
run_pipeline(&prompt, &start_agent, agents_dir.as_deref(), None, None, verbose, json).await?;
```

- [ ] **Step 3: Build check**

```bash
cargo build -p lattice-cli 2>&1
```
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add lattice-cli/src/commands/run.rs lattice-cli/src/main.rs
git commit -m "refactor(cli): run_pipeline() async, use pipeline.run_async().await"
```

---

### Task 17: Full workspace test + clippy

**Files:** All

- [ ] **Step 1: Run all tests**

```bash
cargo test --workspace 2>&1
```
Expected: all tests pass. Fix any failures.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --workspace -- -D warnings 2>&1
```
Expected: zero warnings. Fix any.

- [ ] **Step 3: Run fmt**

```bash
cargo fmt --check --all 2>&1
```
Expected: no formatting issues. Run `cargo fmt --all` if needed.

- [ ] **Step 4: Verify SHARED_RUNTIME is gone**

```bash
grep -r "SHARED_RUNTIME" lattice-agent/src/ lattice-harness/src/
```
Expected: no matches.

- [ ] **Step 5: Verify MEMORY_RT is gone**

```bash
grep -r "MEMORY_RT" lattice-harness/src/
```
Expected: no matches.

- [ ] **Step 6: End-to-end smoke test**

```bash
cargo run -- "1+1=?"
```
Expected: returns a response (requires API key).

- [ ] **Step 7: Final commit**

```bash
git add -A
git commit -m "chore: final cleanup — fmt, clippy, verify SHARED_RUNTIME/MEMORY_RT removed"
```
