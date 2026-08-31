# AGENTS.md

Crate-local guidance for `lattice-profile`.

## Role

This crate owns the strict single-agent `agent.toml` contract: schema structs,
default templates, profile discovery, read-only loading, allowlist validation,
LANCET profile sections, and pure lowering data consumed by API/authority.

## Sub-Architecture

- `src/schema.rs` owns `[agent]`, `[llm]`, `[prompt]`, `[tools]`, and `[lancet]`
  schema structs.
- `src/defaults.rs` owns built-in reader/editor/dev/default templates.
- `src/discovery.rs` owns profile lookup paths and `ProfileSource`.
- `src/load.rs` owns read-only profile loading and validation.
- `src/error.rs` owns `ProfileError`.
- `src/lib.rs` re-exports only the public profile surface.
- Tests should cover strict parsing, defaults, discovery, and LANCET sections.

## Boundaries

- No runtime orchestration, session graph state, CLI UI, or Tool execution.
- Keep loading strict; unknown or unsupported fields should be intentional.
- Keep LANCET policy/profile data compatible with protocol and authority types.
- Do not silently normalize dangerous paths.

## Recent Issues

- `[lancet.capabilities]` (`deny`, `max_boundary`, default `in`) is typed.
  `AgentProfile::capability_admission_policy` lowers it with `[tools].enabled`
  into `CapabilityAdmissionPolicy`; runtime construction rejects unknown deny
  names.
- Retired `lancet.mode` and `[lancet.effects]` input is rejected during strict
  deserialization; no compatibility semantics remain.
- FIXED #62 (Codeberg #40): the Upper microVM `[workspace]` section
  (`WorkspaceSection`, `EgressPolicyConfig`, `CredentialRef`, lifecycle-limit
  constants) was removed. Legacy profiles declaring `[workspace]` now fail
  strict parsing; the loader names it via the `FORBIDDEN_SECTIONS` pre-scan.
  Generated templates emit only the `[lancet.workspace]` policy section.
- FIXED P19-CR-05: `[lancet]` flatten extension now rejects scalar typos instead
  of silently accepting them.
- FIXED M-13: profile canonicalization failures now return
  `ProfileError::SystemPathNotFound` instead of falling back to unsafe paths.
- `tools.enabled` is exposure/admission stage one; generated Component Cell
  descriptors define required capabilities.
- FIXED #46 (Codeberg #24): optional `[llm].timeout_secs` is validated as
  `1..=3600`; zero and upper-bound failures retain field-specific diagnostics.
- WATCH: external policy file schema (`[lancet.policy].path`) is consumed by
  per-turn LANCET composition. Command isolation was retired; do not
  reintroduce `[lancet.isolation]`/`[lancet.command]` typed sections.
## Next Actions

- Keep profile schema names clear about exposure versus authority/policy intent;
  add explicit fields only when the profile really owns a policy choice.
- Add strict-deserialize tests for every new profile section and future table.
- Keep path resolution behavior explicit for missing files, symlinked parents,
  and project-root-relative paths.
- Coordinate LANCET section changes with authority defaults and API lowering.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when schema, defaults, discovery, or
  loading semantics change.
- Record unresolved profile/schema risks in root `FINDINGS.md`.
- Record landed profile fixes in root `FIX.md` with verification commands.
- For partial schema migrations, document the residual compatibility risk in
  `FINDINGS.md`.
- Keep this file near 80 lines; remove stale closed notes after they stop
  guiding local edits.

## Verification

- Local: `cargo test -p lattice-profile`.
- LANCET profile changes: also run API turn/runtime composition tests.
- Path/security changes should include missing path, symlink prefix, and typo
  rejection coverage.
