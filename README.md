# artemis

A fast, model-centric LLM inference engine. Rust core, Python bindings, micro-agent harness.

**Not** a SaaS. Not a visual workflow builder. Model routing + inference + agent orchestration via TOML-defined pipelines.

## Design

```
artemis/ (Cargo workspace)
├── artemis-core/       Pure Rust: resolve + chat + streaming + retry
├── artemis-agent/      Agent struct, conversation state, tool boundary, memory
├── artemis-memory/     Memory trait + InMemoryMemory + SqliteMemory (async)
├── artemis-token-pool/ TokenPool trait + UnlimitedPool
├── artemis-harness/    Pipeline orchestrator, TOML rule engine, hot reload
├── artemis-plugin/     Plugin trait (typed Input → LLM → Output)
├── artemis-cli/        CLI: run, validate, debug, list agents
├── artemis-tui/        Terminal UI (ratatui)
└── artemis-python/     PyO3 bindings (pip: artemis-core)
```

### Four principles

| | |
|---|---|
| **Fast** | Rust hot path, zero-cost abstractions |
| **Minimal** | No DB required, no external services. Just Rust library + catalog.json. |
| **Pluggable** | Overlay pattern for providers, tools, routing rules |
| **Focused** | One thing well: given model + messages → return response |

### Plugin model

Every plugin is a typed function: `Input → to_prompt() → LLM.invoke() → from_output() → Output`

The code controls the flow. The LLM is just the inference step — it doesn't decide what to do, when to stop, or where to hand off.

```python
class CodeReviewPlugin(Plugin):
    def build_input(self, ctx): ...     # what to feed the LLM
    def to_prompt(self, input): ...     # format the prompt
    def from_output(self, raw): ...     # parse + validate
    def should_handoff(self, output): ... # deterministic routing
```

### Agent pipeline (harness)

Agents are defined as TOML files in `~/.artemis/agents/`. The harness runs them in sequence, using TOML handoff rules to route between agents:

```toml
[agent]
name = "code-review"
model = "sonnet"

[system]
prompt = "You are a senior code reviewer. Return JSON: {issues: [], confidence: 0.x}"

[handoff]
fallback = "human-review"

[[handoff.rules]]
condition = { field = "issues[any].severity", op = "==", value = "critical" }
target = "refactor"

[[handoff.rules]]
default = true
```

Handoff rule operators: `==`, `!=`, `<`, `>`, `<=`, `>=`, `contains`. Compound rules: `all` (AND), `any` (OR). Array matching: `[any]` iterates all elements. JSON schema validation with automatic retry.

## Quick start

### Rust

```rust
use artemis_core;

// Resolve a model alias to a specific provider + credentials
let resolved = artemis_core::resolve("sonnet")?;
// -> ResolvedModel { provider: "anthropic", api_model_id: "claude-sonnet-4-6", ... }

// Streaming: get tokens as they arrive
let messages = vec![Message { role: Role::User, content: "Hello".into() }];
let stream = artemis_core::chat(&resolved, &messages, &[])?;
// -> impl Stream<Item = StreamEvent>

// Agent with tools + memory
let mut agent = Agent::new(resolved)
    .with_memory(memory)
    .with_tools(tool_definitions);
let output = agent.send("Review this code").await?;
```

### CLI

```bash
# Run a single agent
artemis run "Review src/router.rs" --agent code-review

# Run a pipeline
artemis run "Review src/router.rs" --pipeline review

# Validate pipeline chain without calling any LLM
artemis validate review

# List loaded agents
artemis list agents

# Debug model resolution
artemis debug sonnet
```

### Python

```bash
cd artemis-python && maturin develop
```

```python
import artemis_core

engine = artemis_core.ArtemisEngine()
engine.resolve_model("sonnet")
# -> PyResolvedModel(provider="anthropic", api_model_id="claude-sonnet-4-6", ...)

engine.list_authenticated_models()
# -> lists all models with valid credentials in your environment
```

## Current status

Artemis is in **alpha / dogfooding** stage. What works:

- **Model resolution**: 98 models, 37 aliases, 27 provider defaults (23 with base_url)
- **Rust inference**: `resolve()` + `chat()` for OpenAI and Anthropic protocols via unified Transport
- **Streaming**: SSE parsers for both protocols, tool call tracking across chunks
- **Thinking mode**: DeepSeek v4-pro (OpenAI reasoning_content), MiniMax M2.7 (Anthropic thinking_delta)
- **Agent**: multi-turn conversation, tool execution boundary, memory, token budget
- **Harness**: TOML-defined pipelines, handoff rule engine, dry-run validation, hot reload
- **JSON schema validation**: jsonschema crate for LLM output validation with retry loop
- **CLI**: run, validate, debug, list agents
- **HTTPS enforced**: non-localhost HTTP base URLs rejected at the engine level

## Project structure

```
artemis/                 Git root (Cargo workspace)
├── artemis-core/        Pure Rust: resolve, chat, streaming, retry
│   ├── src/
│   │   ├── catalog/     Model catalog, aliases, provider defaults
│   │   ├── router.rs    Model resolution, credential resolution
│   │   ├── provider.rs  ChatRequest/ChatResponse types
│   │   ├── transport/   Unified Transport trait, dispatcher, per-protocol adapters
│   │   ├── streaming/   SSE parsers (OpenAI format, Anthropic format)
│   │   ├── retry.rs     Error classification, jittered exponential backoff
│   │   ├── tokens.rs    tiktoken integration + token estimation
│   │   ├── errors.rs    ArtemisError enum
│   │   └── types.rs     Role, Message, ToolDefinition, ToolCall, FunctionCall
│   └── tests/e2e/       End-to-end + regression tests
├── artemis-agent/       Agent struct, conversation state, tool boundary
├── artemis-memory/      Memory trait + InMemoryMemory + SqliteMemory
├── artemis-token-pool/  TokenPool trait + UnlimitedPool
├── artemis-harness/     Pipeline orchestrator, TOML rule engine, hot reload
│   ├── src/
│   │   ├── pipeline.rs  Pipeline + PipelineRun + DryRunReport
│   │   ├── handoff_rule.rs  TOML rule parsing + evaluation
│   │   ├── profile.rs   AgentProfile TOML parsing
│   │   ├── registry.rs  AgentRegistry loading + hot reload
│   │   ├── runner.rs    AgentRunner (wraps Agent + profile)
│   │   └── events.rs    EventBus + PipelineEvent
├── artemis-plugin/      Plugin trait (typed Input → LLM → Output)
├── artemis-cli/         CLI commands: run, validate, debug, list
├── artemis-tui/         Terminal UI (ratatui)
└── artemis-python/      PyO3 bindings (pip: artemis-core)
```

## Why not X?

- **OpenRouter / LiteLLM**: SaaS/model gateways. artemis is a library you embed.
- **LangGraph / CrewAI**: Heavy multi-agent frameworks. artemis gives you primitives + lightweight orchestration.
- **n8n / Dify**: Visual workflow builders for non-developers. artemis is for developers.
- **MCP**: Model-to-tool protocol. Complementary — plugins reference tools via MCP internally.
- **A2A**: Agent-to-agent protocol. Reference for artemis's handoff layer.

## Requirements

- Rust 1.80+
- Python 3.12+ (optional, for Python bindings)
- Credentials in environment variables: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, etc.

## Configuration

### `.env` file

Artemis CLI loads a `.env` file from the working directory on startup (via `dotenvy`). This lets you set provider credentials without polluting your shell profile.

1. Copy the template: `cp .env.example .env`
2. Fill in your API keys

Only variables that are set are used — leave unused providers blank or omit them entirely. Shell environment variables take precedence over `.env` values.

See `.env.example` for the full list of supported variables.

## Development

```bash
cargo build                    # Debug build (all crates)
cargo build --release          # Release build
cargo test                     # Run all Rust unit tests
cargo test -p artemis-core     # Single crate tests
cargo test -p artemis-core <test_name>  # Single test
cargo clippy -- -D warnings    # Lint (warnings as errors)
cargo fmt --check --all        # Format check
cargo fmt --all                # Format code
cargo bench -p artemis-core    # Benchmarks

# Python bindings
cd artemis-python && maturin develop
```

See [CLAUDE.md](CLAUDE.md) for detailed development guide.

## License

MIT