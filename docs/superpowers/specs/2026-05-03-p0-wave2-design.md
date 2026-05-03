---
title: P0 Wave 2 — Runtime Architecture + Pipeline Orchestration + Safety Hardening
date: 2026-05-03
status: approved
priority: P0 (7 items, 3 breakthrough points)
strategy: 压强原则 — 3 breakthrough points penetrating 7 P0 items
---

# P0 Wave 2 Design Spec

## Problem Summary

7 P0 items from deep engineering audit remain Partial/Open after cross-entry-consistency fix.
Each item has partial fix but core gap persists.

| # | Issue | Status | Core Gap |
|---|-------|--------|----------|
| P0-01(r2) | Runner silent JSON fallback | Partial | looks_like_error exists, but non-error non-JSON still silently Ok |
| P0-02(r2) | Nested Runtime panic | Open | SYNC_RT/BUS_RT + block_on still present |
| P0-03(r2) | Bus RPC channel capacity = 1 | Open | default max_concurrent_calls = 1 |
| P0-04(r2) | Tool/Plugin registry not wired | Partial | assemble_agent() exists, doesn't use registries; run_async() doesn't delegate to PluginDagRunner |
| P0-05(r2) | web_search no response size limit | Open | response.text().await reads unlimited |
| P0-05(1st) | bash sh -c still present | Partial | timeout + metachar blocking exist, sh -c remains |
| P0-06(1st) | SHA-256 not enforced by default | Partial | opt-in sidecar, not enforced |

## Strategy: 压强原则 (3 Breakthrough Points)

1. **Runtime Architecture** — async-first, eliminate static runtimes (P0-02)
2. **Pipeline Orchestration Closure** — error detection + registry wiring + PluginDagRunner delegation (P0-01, P0-04)
3. **Safety Hardening** — channel capacity + response size limit + SHA-256 enforcement (P0-03, P0-05(r2), P0-06)

P0-05(1st) (bash sh-c) is documented as intentional architecture choice — metacharacter blacklist + timeout + token-prefix allowlist provide sufficient mitigation for Alpha stage.

---

## Breakthrough 1: Runtime Architecture — Eliminate Static Runtimes

### P0-02: Nested Runtime Panic

**Current**: `Pipeline::run()` is sync fn using `SYNC_RT.block_on()`. CLI calls it from `#[tokio::main]` context → panic. `MicroAgent::spawn()` uses `BUS_RT.block_on()` → same risk.

**Changes**:

#### Pipeline::run() → async

- Delete `static SYNC_RT`
- Change `pub fn run(&mut self, ...) -> PipelineRun` to `pub async fn run(&mut self, ...) -> PipelineRun`
- `run()` (formerly `run_async()`) becomes the single async entry point. `run_async()` is deleted — its body is `run()` now.

**API change**:
```rust
// Before
pub fn run(&mut self, start_agent: &str, input: &str) -> PipelineRun

// After
pub async fn run(&mut self, start_agent: &str, input: &str) -> PipelineRun
```

`run_async()` is removed — `run()` is now the single async entry point.

#### CLI adaptation

`commands::run.rs`:
- `run_pipeline()` was sync fn, becomes async
- Main calls it from `#[tokio::main]` directly with `.await`

`main.rs`:
- `run::run_pipeline()` call gets `.await`

#### MicroAgent::spawn() → async

- Delete `static BUS_RT`
- Change `pub fn spawn(self) -> Result<MicroAgentHandle, BusError>` to `pub async fn spawn(self) -> Result<MicroAgentHandle, BusError>`
- `self.bus.register(descriptor)` → `.await` directly (caller is in async context)
- `self.bus.subscribe(...)` → `.await` directly
- `tokio::spawn(async move { ... })` stays (it uses current runtime, not a static one)

#### Python binding adaptation

`lattice-python/src/engine.rs`:
- Dual-path sync boundary:
  ```rust
  if let Ok(handle) = tokio::runtime::Handle::try_current() {
      // Already in a multi-threaded runtime — use block_in_place
      handle.block_in_place(|| handle.block_on(async_fn()))
  } else {
      // No runtime — create temporary one
      let rt = tokio::runtime::Runtime::new().expect("temp runtime");
      rt.block_on(async_fn())
  }
  ```
- `block_in_place` only works on multi-threaded runtime; single-threaded runtime would panic. The fallback to temporary runtime handles this case.
- This is the ONLY remaining `block_on` usage — at the Python sync boundary, which is correct

#### Call Site Changes

| File | Change |
|------|--------|
| lattice-harness/src/pipeline.rs | Delete SYNC_RT, `run()` becomes async, delete `run_async()` |
| lattice-harness/src/micro_agent.rs | Delete BUS_RT, `spawn()` becomes async |
| lattice-cli/src/commands/run.rs | `run_pipeline()` becomes async |
| lattice-cli/src/main.rs | Call `run_pipeline().await` |
| lattice-python/src/engine.rs | Handle::try_current() + block_in_place for sync boundary |
| lattice-harness/src/lib.rs | Re-export unchanged |

**CLAUDE.md update**: "Runtime model" section now states: "LATTICE is fully async. No static runtimes. CLI uses #[tokio::main]. Python uses Handle + block_in_place at sync boundary."

---

## Breakthrough 2: Pipeline Orchestration Closure

### P0-01: Runner Silent JSON Fallback

**Current**: `run_once()` silently wraps non-JSON output as `{"content": raw_text}` and returns Ok. `looks_like_error()` covers 8 prefixes but misses many LatticeError variants.

**Changes**:

#### output_schema-driven behavior

When `output_schema` is configured:
- Non-JSON output → `Err("Agent output is not valid JSON, but output_schema requires structured output")`
- This prevents handoff rule engine from operating on malformed data

When `output_schema` is NOT configured:
- Non-JSON output → `Ok({"content": raw_text})` — this is correct for natural language agents
- This is the existing behavior, preserved

Implementation: `AgentRunner` has `output_schema` field (already exists). In `run_once()`, after JSON parse failure:
```rust
let output = match serde_json::from_str(&json_str) {
    Ok(v) => v,
    Err(_) if self.output_schema.is_some() => {
        return Err("Agent output is not valid JSON; output_schema requires structured output".into());
    }
    Err(_) => serde_json::json!({"content": content}),
};
```

#### looks_like_error() expansion

Add these prefixes to the existing list:
- HTTP status code prefixes: `"401"`, `"403"`, `"429"`, `"500"` (already present)
- Common transport/infrastructure error phrases: `"timeout"`, `"connection refused"`, `"connection reset"`, `"dns"`, `"TLS"`, `"certificate"`
- Provider error patterns: `"rate limit"`, `"authentication"`, `"api key"`, `"invalid request"`, `"server error"`, `"service unavailable"`
- Do NOT add Rust `LatticeError` enum Display prefixes — LLMs never output Rust enum format strings like `"RateLimitError"`. Real provider errors use natural language, not Rust type names.

#### Empty output handling

Empty output → `Err` only when `output_schema.is_some()`. Without schema, empty output may be legitimate (e.g., agent completed via tool calls with no final text response). Do not add empty-output check to the no-schema path.

Implementation:
```rust
// After collecting content from LoopEvent::Token events
if content.is_empty() && self.output_schema.is_some() {
    return Err("Agent produced empty output; output_schema requires structured response".into());
}
```

**File**: lattice-harness/src/runner.rs only

### P0-04: Tool/Plugin Registry Wiring

**Current**: `assemble_agent()` uses hardcoded `default_tool_definitions()`. `run_async()` never checks `profile.plugins` to delegate to PluginDagRunner.

**Changes**:

#### assemble_agent() + ToolRegistry

Add `Option<Arc<ToolRegistry>>` parameter to `assemble_agent()`:
```rust
pub fn assemble_agent(
    profile: &AgentProfile,
    resolved: ResolvedModel,
    memory: Option<Arc<dyn Memory>>,
    tool_registry: Option<Arc<ToolRegistry>>,  // NEW
) -> AgentRunner {
```

When `tool_registry` is present and not empty:
1. Get custom tool definitions from registry
2. Merge with default tools using `merge_tool_definitions()` (existing function in tools.rs)
3. Use `DefaultToolExecutor` with registry fallback — no separate composite executor struct needed:
   ```rust
   // DefaultToolExecutor gains an optional registry reference
   pub struct DefaultToolExecutor {
       working_dir: String,
       registry: Option<Arc<ToolRegistry>>,
   }
   
   // In execute() — check registry first, fall back to default behavior
   fn execute(&self, call: &ToolCall) -> Result<String, ToolError> {
       if let Some(reg) = &self.registry {
           if let Some(entry) = reg.get(&call.function.name) {
               match &entry.handler {
                   ToolHandler::Native(f) => f(call.function.arguments.clone()),
                   ToolHandler::McpBacked { .. } => {
                       // MCP not supported in this executor yet
                       ToolError::McpUnreachable { ... }
                   }
               }
           } else {
               self.execute_default(call) // existing 7 default tools
           }
       } else {
           self.execute_default(call) // existing behavior
       }
   }
   ```
4. Attach merged definitions + enriched executor to Agent

When `tool_registry` is absent or empty:
- Existing behavior: `default_tool_definitions()` + `DefaultToolExecutor`

#### run_async() PluginDagRunner delegation

Add check at the start of each agent turn in `run()` (formerly `run_async()`):
```rust
if profile.plugins.is_some() && self.plugin_registry.is_some() {
    // Delegate entire turn to PluginDagRunner
    let plugins_config = profile.plugins.as_ref().unwrap();
    let pr = self.plugin_registry.as_ref().unwrap();
    // ToolRegistry is required — create empty if Pipeline has none
    let tr = self.tool_registry.as_ref().unwrap_or(&Arc::new(ToolRegistry::new()));
    let mut dag_runner = PluginDagRunner::new(
        plugins_config, pr, tr,
        RetryPolicy::default(),
        self.shared_memory.clone(),
    );
    let dag_result = dag_runner.run(&current_input, &profile.agent.model).await;
    // Convert DAGError/Result<Value> to PipelineRun output format
} else {
    // Standard AgentRunner path (existing)
    let mut runner = assemble_agent(&profile, resolved, ...);
    runner.run(&current_input, ...).await
}
```

#### Pipeline passes registries through

`Pipeline::run()` already has `self.tool_registry` and `self.plugin_registry` fields.
The `assemble_agent()` call in `run_async()` now passes both.

**Files**:
- lattice-harness/src/assembly.rs — add tool_registry param
- lattice-harness/src/pipeline.rs — pass tool_registry/plugin_registry, add plugin delegation
- lattice-harness/src/dag_runner.rs — ensure `run()` method accepts needed params (already exists)
- lattice-harness/src/lib.rs — update re-export if needed

---

## Breakthrough 3: Safety Hardening

### P0-03: Bus RPC Channel Capacity

**Current**: `BusConfig::default()` has `max_concurrent_calls: 1`. Only one RPC request can be buffered per agent.

**Change**: Set default to 32.

```rust
impl Default for BusConfig {
    fn default() -> Self {
        Self {
            timeout_rpc: Duration::from_secs(30),
            delivery_policy: DeliveryPolicy::AtMostOnce,
            subscriber_buffer: 1024,
            max_concurrent_calls: 32,  // Changed from 1
        }
    }
}
```

**Test**: Update `assert_eq!(c.max_concurrent_calls, 1)` → `assert_eq!(c.max_concurrent_calls, 32)`

**Files**: lattice-bus/src/lib.rs (1 line change + 1 test update)

### P0-05(r2): web_search Response Size Limit

**Current**: `response.text().await` reads unlimited response body.

**Change**: Add `max_http_response_size: usize` field to `SandboxConfig`, default 10MB (web pages frequently exceed 1MB). This is a separate field from `max_read_size` (file read limit) — HTTP responses have different size expectations.

Implementation:
```rust
// In SandboxConfig (sandbox.rs)
pub max_http_response_size: usize,  // Default: 10 * 1024 * 1024 (10MB)

// In web_search handler (tools.rs)
"web_search" => {
    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = self.sandbox.check_url(url) {
        return e;
    }
    match self.http_client.get(url).send().await {
        Ok(response) => {
            // Check content-length header first
            if let Some(content_length) = response.content_length() {
                if content_length > self.sandbox.max_http_response_size as u64 {
                    return ToolError::HttpError(format!(
                        "Response too large: {} bytes (max {})",
                        content_length, self.sandbox.max_http_response_size
                    ))).to_string();
                }
            }
            // Read with size limit — read bytes then truncate if oversized
            let bytes = response.bytes().await.unwrap_or_default();
            if bytes.len() > self.sandbox.max_http_response_size {
                let truncated = String::from_utf8_lossy(&bytes[..self.sandbox.max_http_response_size]);
                format!("{}[TRUNCATED: response exceeded {} bytes]", truncated.trim_end(), self.sandbox.max_http_response_size)
            } else {
                String::from_utf8_lossy(&bytes).to_string()
            }
        }
        Err(e) => ToolError::HttpError(e.to_string()).to_string(),
    }
}
```

**Files**: lattice-agent/src/sandbox.rs (add field + default), lattice-agent/src/tools.rs (web_search handler)

### P0-06: SHA-256 Enforcement

**Current**: `verify_config_integrity()` skips check when no `.toml.sha256` sidecar exists.

**Change**: Add strict mode via environment variable `LATTICE_STRICT_CONFIG=1`.

When `LATTICE_STRICT_CONFIG=1`:
- Missing `.toml.sha256` → load fails with error "Config integrity hash not found; set LATTICE_STRICT_CONFIG=0 to allow unverified configs"
- This forces production deployments to generate hash files

When `LATTICE_STRICT_CONFIG` is unset or `0` (default):
- Existing behavior: graceful degradation, skip check when no sidecar

Implementation:
```rust
fn verify_config_integrity(path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>> {
    let hash_path = path.with_extension("toml.sha256");
    if !hash_path.exists() {
        if std::env::var("LATTICE_STRICT_CONFIG").unwrap_or_default() == "1" {
            return Err(format!(
                "Config integrity hash not found at {}; set LATTICE_STRICT_CONFIG=0 to allow unverified configs",
                hash_path.display()
            ).into());
        }
        return Ok(()); // Graceful: no hash file = no integrity check
    }
    // ... existing SHA-256 comparison logic unchanged ...
}
```

Add CLI command `lattice config hash <path>` to generate `.toml.sha256` sidecar:
```rust
// In cli commands
fn hash_config(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let digest = Sha256::digest(content.as_bytes());
    let hash_str = format!("{:x}", digest);
    let hash_path = path.with_extension("toml.sha256");
    std::fs::write(&hash_path, &hash_str)?;
    Ok(hash_str)
}
```

**Files**:
- lattice-harness/src/profile.rs — strict mode check
- lattice-cli/src/commands/ — add `config hash` subcommand
- lattice-cli/src/main.rs — route `config hash` command

### P0-05(1st): bash sh -c — Documented Architecture Choice

**Decision**: Keep `sh -c` execution model. Document it as intentional.

Current mitigations are sufficient for Alpha:
- Metacharacter blacklist blocks `;`, `|`, `&&`, `||`, `$()`, backticks, `\n`, `\r`, `>`, `<`, `&`, ANSI-C quoting `$'\x'`
- Token-prefix allowlist matching (from cross-entry fix)
- Timeout wrapper (30s default)
- `PATH=` and `LD_PRELOAD=` rejection

**Change**: Update sandbox.rs doc comment and CLAUDE.md to explicitly state the sh -c design choice and its mitigation stack.

**Files**: lattice-agent/src/sandbox.rs (doc comment), CLAUDE.md (tool execution section)

---

## File Change Summary

| File | Change Type | P0 Items |
|------|------------|----------|
| lattice-harness/src/pipeline.rs | Major: delete SYNC_RT, async run(), pass registries, plugin delegation | 02, 04 |
| lattice-harness/src/micro_agent.rs | Medium: delete BUS_RT, async spawn() | 02 |
| lattice-harness/src/runner.rs | Medium: output_schema-driven JSON handling, expanded error detection | 01 |
| lattice-harness/src/assembly.rs | Medium: add tool_registry param | 04 |
| lattice-harness/src/dag_runner.rs | Minor: ensure run() method accessible | 04 |
| lattice-bus/src/lib.rs | Tiny: default max_concurrent_calls = 32 | 03 |
| lattice-agent/src/tools.rs | Medium: web_search response size limit | 05(r2) |
| lattice-harness/src/profile.rs | Small: LATTICE_STRICT_CONFIG strict mode | 06 |
| lattice-cli/src/commands/run.rs | Medium: async run_pipeline() | 02 |
| lattice-cli/src/main.rs | Medium: async call + config hash command | 02, 06 |
| lattice-cli/src/commands/config.rs | New: config hash subcommand | 06 |
| lattice-python/src/engine.rs | Medium: Handle + block_in_place at sync boundary | 02 |
| lattice-agent/src/sandbox.rs | Tiny: doc comment update | 05(1st) |
| CLAUDE.md | Minor: runtime model + tool execution doc update | 02, 05(1st) |

**Total**: 14 files, ~1 new file (config.rs), 13 modifications

---

## Testing Plan

### Runtime Architecture (P0-02)
- Pipeline::run() is async fn — compile check
- CLI run_pipeline() works in #[tokio::main] — cargo test + manual run
- MicroAgent::spawn() is async — compile check
- Python Engine sync wrapper works with dual-path (Handle+block_in_place / temp_rt) — unit test
- No static runtimes anywhere — grep for `LazyLock<Runtime>`

### Runner Error Detection (P0-01)
- run_once() with output_schema + non-JSON output → Err
- run_once() without output_schema + non-JSON output → Ok({"content": ...})
- run_once() with expanded error prefix → Err for each new prefix
- run_once() with empty output + output_schema → Err
- run_once() with empty output + no output_schema → Ok (tool-call-only completion)

### Registry Wiring (P0-04)
- assemble_agent() with ToolRegistry → merged definitions + composite executor
- assemble_agent() without ToolRegistry → default tools (unchanged)
- run_async() with profile.plugins + plugin_registry → delegates to PluginDagRunner
- run_async() without plugins → standard AgentRunner path (unchanged)

### Bus Channel (P0-03)
- BusConfig::default() max_concurrent_calls == 32

### web_search Size (P0-05(r2))
- Response within 10MB limit → full content
- Response exceeding 10MB limit → truncated + [TRUNCATED] marker
- content-length header exceeds limit → immediate error
- max_http_response_size is independent of max_read_size

### SHA-256 Strict (P0-06)
- LATTICE_STRICT_CONFIG=1 + missing sidecar → Err
- LATTICE_STRICT_CONFIG unset + missing sidecar → Ok (graceful)
- config hash command → generates valid .toml.sha256

### Integration
- `cargo test --workspace` must pass 629+ tests
- `cargo clippy -- -D warnings` must pass
- `cargo fmt --check --all` must pass