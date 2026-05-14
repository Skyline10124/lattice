# lattice-runtime API

`lattice-runtime` is the preferred application-facing entry point. It owns shared wiring for model routing, registries, security, sessions, pipeline execution and direct agent construction.

## Main Types

- `Runtime`: shared facade for model resolution, chat, agent assembly, pipelines and sessions.
- `RuntimeBuilder`: configures credentials, registries, memory, session store, plugin directories and event capacity.
- `AgentSpec`: CLI-facing agent builder with model, provider override, prompt, messages, security and tool capability selection.
- `RuntimeSecurity` / `RuntimeSecurityConfig`: sandbox mode, allowlists, audit and Landlock settings.
- `BuiltAgent`: returns an `Agent`, resolved model and context length.
- `RuntimeHandle`: shared runtime plus tracked background tasks.

## Common Entry Points

```rust
let runtime = Runtime::builder()
    .credentials(credentials)
    .session_store(session_store)
    .build();

let resolved = runtime.resolve("sonnet")?;
let built = runtime.build_agent(
    AgentSpec::new("sonnet".into())
        .project_root(".")
        .capability_preset("code-agent")
        .security(Some(security)),
)?;
```

Use `Runtime::pipeline()` or `Runtime::run_pipeline()` for profile-backed orchestration. Use `Runtime::build_agent()` for direct CLI/TUI-style agents.
