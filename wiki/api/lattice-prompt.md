# lattice-prompt API

`lattice-prompt` owns prompt assembly and prompt-time context providers.

## Main Types

- `PromptRegistry`: stores prompt providers and collects contributions.
- `ContextProvider`: trait for runtime prompt contributors.
- `ProviderFactory`: creates providers from prompt specs.
- `SystemPromptStack`: kernel, agent and dynamic prompt deltas.
- `SystemPromptDelta`: trusted dynamic prompt edits.
- `ToolSchemaProvider`: renders tool definitions into prompt context.
- `ProjectInstructionsProvider`: loads project instruction files from root to cwd.

## Compiler Shape

Prompt compilation follows:

```text
Collect -> Sort -> Budget -> Trim -> Render
```

Sections are layered so system instructions, rules, tools, memory, events and user input can be reasoned about independently.
