# AGENTS.md

Crate-local guidance for `lattice-workspace`.

## Role

This crate owns the backend-neutral Upper workspace boundary: sandbox lifecycle,
typed workspace-relative filesystem and command primitives, baseline delivery,
egress and credential configuration, warm templates, and the production
microsandbox adapter. It also provides a deterministic test adapter for CI.

## Sub-Architecture

- `src/lib.rs` owns the public `Workspace` trait, lifecycle types, backend
  identity, egress and credential contracts, errors, and OID validation.
- `src/microsandbox.rs` owns the production libkrun/microsandbox adapter,
  sandbox lifecycle, baseline delivery, command execution, and SDK translation.
- `src/paths.rs` owns normalized workspace-relative path DTOs, path intents,
  command/search requests, result types, and operation limits.
- `src/primitives.rs` owns shared filesystem primitive helpers used by workspace
  adapters; it is available only to tests and the `test-utils` feature.
- `src/git.rs` owns host-side `git2` baseline tree export and checkout helpers.
- `src/template.rs` owns warm-template naming and staleness metadata.
- `src/testing/` owns `DeterministicWorkspace` and the adapter contract suite.
- `tests/` covers deterministic contracts, credential redaction, egress
  planning, path primitives, SDK shape probes, and optional live KVM behavior.

## Boundaries

- Keep CEP policy, Guard L1, effect decisions, audit persistence, and native
  method handlers in the LANCET crates; this crate only executes through the
  workspace adapter contract.
- Keep `UpperHandle` opaque and free of host paths or live SDK objects.
- Accept typed argv and normalized workspace-relative paths; never introduce
  shell-string execution or expose `/workspace` as a caller-supplied path.
- Validate baseline OIDs before use and keep host repository paths on the host
  side of baseline delivery.
- Never persist resolved credentials in warm templates or expose secret values
  through `Debug`, errors, logs, or test fixtures.
- Keep `DeterministicWorkspace` test-only or behind `test-utils`; it records but
  does not enforce egress policy and must not be treated as a security sandbox.

## Recent Issues

- FIXED M-33-05: git-ready warm templates avoid per-Upper package installation
  and support baseline delivery under blocked egress.
- WATCH: production microsandbox tests require KVM, libkrun firmware, and a
  reachable image registry; normal CI must remain deterministic and KVM-free.
- WATCH: changed-path detection (Upper→Lower write-back) currently relies on
  Git porcelain, so ignored files are an accepted defense-in-depth gap
  documented in `FINDINGS.md`.
- WATCH: the microsandbox SDK is pinned; SDK shape changes must update the
  adapter and the probe coverage together.

## Next Actions

- Preserve parity between `MicrosandboxWorkspace` and the shared adapter
  contract whenever lifecycle or primitive methods change.
- Keep path resolution fail-closed for traversal, absolute paths, symlinks,
  missing parents, wrong kinds, and workspace-root aliases.
- Move changed-path detection to a bounded filesystem snapshot diff before
  Upper state becomes persistent or any Upper-to-Lower write-back is added.
- Keep template creation credential-free and apply per-turn credentials only
  during Upper creation or restoration.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when workspace lifecycle, path
  primitives, backend behavior, templates, credentials, or egress changes.
- Record unresolved workspace isolation or lifecycle risks in root
  `FINDINGS.md`.
- Record landed workspace fixes in root `FIX.md` with verification commands.
- Keep this file near 80 lines by pruning stale closed bullets after they stop
  guiding edits.

## Verification

- Deterministic lane: `cargo test -p lattice-workspace`.
- Feature surface: `cargo test -p lattice-workspace --features test-utils`.
- Compile all integration targets:
  `cargo clippy -p lattice-workspace --features integration_tests --all-targets -- -D warnings`.
- Run live integration tests only on an explicitly provisioned KVM/libkrun
  host; do not make normal CI depend on that lane.
