# AGENTS.md

Crate-local guidance for `lancet-api`.

## Role

This crate is the unified public API for driving LANCET's Controlled Execution
Protocol runtime, independent of any host application. It owns runtime
composition that previously lived inside `lattice-api`, so LANCET can be
embedded without pulling in the LATTICE session/agent/profile layers.

## Sub-Architecture

- `src/runtime.rs` — `LancetRuntime` (full production runtime: server lifecycle,
  `CepClient`, audit, tool defs), `LancetServer::spawn` (low-level registry +
  transport + server task launcher), `ServerLifecycle`, `base_observation_envelope`,
  and `cep_policy_from_policy_config`.
- `src/config.rs` — `LancetRuntimeConfig` and `AuditBacking`. Policy arrives as
  fully-resolved LANCET-native types; the host translates its own config.
- `src/attestation.rs` — `store_turn_attestation` (unsigned in-toto from audit rows).
- `src/tools.rs` — `resolve_catalog_tool_definitions` / `resolve_enabled_tools`.
- `src/error.rs` — `LancetError` (Config / Internal / UnknownTools).
- `src/lib.rs` — curated re-exports of the types appearing in public signatures.

## Boundaries

- No dependency on `lattice-agent`, `lattice-session`, `lattice-profile`, or
  `lattice-cli`. Do not add one.
- Inputs are LANCET-native (`PolicyConfig`, `CommandIsolationRuntimeConfig`,
  `CepMode`). Translating an application profile into policy belongs to the host
  (today: `lattice-api::turn::cep`).
- Native handler/transport implementation stays in `lancet-native`; this crate
  only composes it.

## Recent Issues

- FIXED M-24-05: turn/LANCET runtime composition extracted out of `lattice-api`
  into this crate; the three hand-rolled server-spawn sites (turn runtime, CLI
  `lancet` smoke, CLI `audit`) now route through `LancetServer`/`LancetRuntime`.
- OPEN L-llm-coupling: `to_tool_definitions` yields `lattice_llm::ToolDefinition`,
  so the LANCET stack still depends on `lattice-llm` (a Layer-0 leaf). Extracting
  `ToolDefinition` (and `lattice_llm::security`) to a LANCET-owned or shared crate
  would make LANCET fully LATTICE-free. Tracked in `FINDINGS.md`.

## Next Actions

- If a profile/config translation needs a new policy field, extend
  `LancetRuntimeConfig` with a LANCET-native type — never accept `LancetSection`.
- When adding a public entry point, re-export the types in its signature from
  `lib.rs` so consumers need not depend on the lower crates directly.
- Pursue L-llm-coupling once a home for the shared tool/security types is chosen.

## Record Maintenance

- Update `Recent Issues`/`Next Actions` when the runtime API surface or
  composition changes.
- Record landed fixes in root `FIX.md` with verification commands; keep residual
  coupling risks in root `FINDINGS.md`.
