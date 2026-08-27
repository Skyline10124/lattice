# AGENTS.md

Crate-local guidance for `lancet-native`.

## Role

This crate owns the transitional CEP server lifecycle around Component Tools:
durable admission handoff, `DispatchStarted`, event transport, per-call host
construction, Component/native routing, and evidence response assembly.

The production native registry is empty after GitHub #32. All five built-ins are
embedded Components. A missing Component has no native fallback.

## Sub-architecture

- `src/server/handle.rs` admits the exact reserved invocation, builds call-scoped
  workspace, mutation, and network hosts, calls `lancet-runtime`, and returns the
  result with one `CellEvidenceTriple`.
- `src/server/hosts.rs` constructs host authority from the admitted descriptor
  and Turn policy. `fetch.web` receives `NetworkFetchHost`; network execution
  itself lives in `lancet-runtime::host_network`.
- `src/server/native_registry.rs` keeps the empty production registry until ADR
  0004 removes the Component/native routing split.
- `src/enforcement/` keeps the residual Guard L1 and legacy effect checks used by
  transitional registry/scaffold paths. Component operation authority is Cell
  admission plus Tool Host checks.
- `src/edit/` and `src/executor/error.rs` compile only in tests. They preserve
  hashline mutation regressions against the live `lancet-runtime` write cap.
- `workspace` is a crate-local alias to `lancet_runtime::workspace`. Rewind and
  residual tests use the same concrete `LocalWorkspace`; there is no second
  filesystem implementation.
- `rewind` re-exports the host-side operator restore path from `lancet-runtime`.
  Rewind is not an Agent Tool and does not append compensation history.

## Boundaries

- Add Tool contracts in `tools/<tool>/manifest.toml`, then regenerate. Do not add
  native handlers, native schemas, compatibility fallbacks, or a native Tool ABI.
- Keep network resolution, sockets, TLS, redirects, response limits, and network
  evidence in `lancet-runtime`. This crate only passes call-scoped policy,
  budget, deadline, cancellation, and admitted capabilities into that host.
- Keep low-level workspace and network authority out of `lattice-agent`.
- Keep the concrete Local Workspace path. Do not reintroduce an
  `ExecutionBackend`, routing enum, workspace projection, or alternate backend.
- Component/native name collisions fail server startup. The empty native catalog
  is still checked rather than silently preferring one implementation.
- The Turn coordinator in `lattice-api` owns terminal publication. This crate
  never creates standalone production invocations.

## Verification

- Local: `cargo test -p lancet-native`.
- Component network: `cargo test -p lancet-native --test
  cep_component_round_trip fetch_web`.
- Lifecycle changes: run affected `lattice-api` round-trip and operator tests.
- Rewind/workspace changes: run the focused `lattice-system-harness` suites.
- Clippy: `cargo clippy -p lancet-native --all-targets -- -D warnings`.

## Record maintenance

Update this file when the crate's remaining lifecycle responsibility changes.
Record landed fixes in root `FIX.md`; keep only open residuals in `FINDINGS.md`.
