# Domain Docs

How the engineering skills should consume this repository's domain documentation when exploring the codebase.

## Layout

This repository uses a single-context layout:

```text
/
|-- CONTEXT.md
|-- docs/adr/
`-- crates/
```

`CONTEXT.md` is the repository-wide domain glossary. `docs/adr/` contains architectural decision records that apply across the workspace.

## Before Exploring

- Read `CONTEXT.md` at the repository root.
- Read ADRs under `docs/adr/` that touch the area about to be changed.

If these files do not exist, proceed silently. Do not flag their absence or suggest creating them upfront. The `domain-modeling` skill creates them lazily when terms or decisions are resolved.

## Use The Glossary's Vocabulary

When output names a domain concept in an issue title, refactor proposal, hypothesis, or test name, use the term defined in `CONTEXT.md`. Do not drift to synonyms the glossary explicitly avoids.

If a needed concept is absent, reconsider whether the project uses that language or note a genuine gap for `domain-modeling`.

## Flag ADR Conflicts

If proposed work contradicts an existing ADR, surface the conflict explicitly rather than silently overriding the decision.
