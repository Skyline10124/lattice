# Research Summary

**Milestone:** v1.0 Single Agent Core
**Synthesized:** 2026-05-14
**Confidence:** HIGH

## Stack Additions

No external stack additions are recommended. The milestone should remove dependencies rather than add new runtime capabilities.

Recommended internal stack change:

- Introduce `lattice-profile` or an equivalent narrowly scoped profile module if it cleanly extracts `AgentProfile`, profile validation, capability/behavior config, and pure turn/build specs from `lattice-orchestrator`.
- Remove `lattice-plugin`, `lattice-plugins`, and `lattice-orchestrator` as workspace members after runtime/CLI no longer depend on them.

## Feature Table Stakes

- One generic `Agent` loop.
- One generic runtime turn path used by CLI and TUI.
- Profile/config describes model, prompt, tools, behavior, cwd/project root, and security intent.
- Runtime owns assembly, handle reuse, session seeding/persistence, sandbox-bound tools, and prompt providers.
- Coding is a profile/tool capability, not `AgentPreset::Coding` and not `coding_agent.rs`.
- Plugin/orchestrator/pipeline/subagent/handoff code is removed from active source.
- Verification includes compile closure, targeted runtime/CLI/TUI tests, sandbox binding checks, and grep-based absence checks.

## Watch Out For

- Do not delete `lattice-orchestrator` before extracting useful profile/spec/assembly pieces.
- Do not preserve coding behavior as a renamed preset branch.
- Do not leave TUI plugin state or slash commands behind.
- Do not weaken runtime sandbox binding while moving assembly.
- Do not combine this milestone with unrelated large-file modularization.

## Recommended Roadmap Shape

1. Extract and define generic profile/spec contracts.
2. Move runtime assembly to generic profile/spec and remove coding preset branching.
3. Convert CLI/TUI to generic runtime path and remove plugin/pipeline/code-agent commands.
4. Remove plugin/orchestrator crates and examples, then update docs and tests.
5. Run final boundary and regression verification.

---
*Research summary for: v1.0 Single Agent Core*
