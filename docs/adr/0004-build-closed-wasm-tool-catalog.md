# ADR 0004: Build-Closed WASM Tool Catalog

- Status: Accepted
- Date: 2026-07-16
- Amends: ADR 0003 catalog and handler-ownership decisions
- Canonical vocabulary: root `CONTEXT.md`

## Context

ADR 0003 correctly removed arbitrary Agent process execution and made the Local
Workspace the sole workspace source of truth, but it also described the current
five native methods and central catalog as permanent implementation boundaries.
LATTICE still needs independently maintained Tools and operator-controlled Tool
composition without giving Tool code ambient host authority or introducing
runtime plugin loading.

Native Rust crates linked into the host cannot be strongly confined by a trait:
they can bypass `ToolHost` and call filesystem, network, or process APIs. Locally
building arbitrary third-party Cargo source has the same problem before runtime
starts because build scripts and procedural macros execute as host code.

## Decision

1. **The catalog is closed per built runtime, not globally fixed to five
   methods.** The default distribution contains `observe.workspace`,
   `mutate.create`, `mutate.edit`, `fetch.web`, and `session.todo_write`.
   Operators may add Source Tools before rebuilding; the resulting runtime has
   one fixed catalog and cannot load more code after startup.

2. **Each Source Tool manifest is the sole Tool Descriptor authority.** It owns
   the method name, structured parameter/result contract, capabilities, scopes,
   effects, version, and description. Build tooling generates WIT bindings,
   host metadata, and `inventory` registration from that manifest. There is no
   separately maintained central schema catalog.

3. **Every Tool, including the five built-ins, executes as a WASM Component.**
   Components receive no ambient WASI filesystem, network, or process access.
   Their only authority is the WIT Tool Host imports supplied by LANCET.

4. **Tool Host is the structural security boundary.** Its workspace channel
   exposes guarded Structured Workspace Operations backed by the concrete Local
   Workspace; its network channel retains native URL/DNS/redirect enforcement;
   its session channel exposes controlled session operations. Policy, Guard L1,
   effect checks, audit, Local Revision checks, Mutation Preconditions, atomic
   installation, and workspace history remain host-enforced.

5. **The Cell effect model remains the descriptor vocabulary.** `CtxOp`,
   `ExtImpact`, scope, and trust form the declared cells. Capability admission,
   per-operation Tool Host authorization, observed effects, audit attribution,
   and context accounting derive from those cells. `in` means controlled by a
   Tool Host domain; `ex` means crossing into the external world, not Upper or
   sandbox containment.

6. **Operators install precompiled Tool Bundles, not Cargo source.** A bundle
   contains the canonical manifest, WASM Component, artifact hash, and
   provenance. Installation verifies descriptor/import consistency and never
   executes third-party build scripts, procedural macros, or native binaries on
   the user's host.

7. **Installation automatically rebuilds the trusted host composition.** Build
   tooling generates an `include_bytes!`/`inventory` wrapper for approved
   bundles, builds a versioned Linux host binary, runs descriptor/artifact and
   exact-catalog gates, then atomically changes the active version used by the
   next invocation. The current process continues with its original catalog.
   Linux is the only supported host; no non-Linux activation or compatibility
   path exists.

8. **LANCET runtime responsibilities converge after Tool migration.**
   `lancet-native`, `lancet-cep`, `lancet-dispatch`, and the central
   `lancet-catalog` surface are absorbed or retired in favor of
   `lancet-runtime`. `lancet-protocol`, `lancet-authority`, `lancet-audit`,
   `net-guard`, and the `lancet-api` facade retain their distinct lower-level or
   public roles. `lancet-runtime` owns Component execution, generated catalog
   registration, dispatch/transport, Tool Host enforcement, concrete Local
   Workspace operations, native network/session channels, and private gix
   workspace history. This is a concrete production composition, not a
   pluggable execution-backend seam.

## Consequences

- ADR 0003 remains authoritative for Direct-Local execution, System Path
  Exclusions, Atomic Local Mutation, and Rewind. Its statements that production
  is permanently limited to five methods and that native handlers are the final
  Tool ownership boundary are superseded by this ADR.
- `inventory` discovers only generated registrations embedded in the host
  binary. Missing, duplicate, unapproved, descriptor-drifted, or import-drifted
  Tools fail the build or startup gate.
- Tool migration must preserve one typed CEP contract and one audit/effect path;
  no permanent native/WASM dual dispatch remains.
- A native executable is not a Tool Bundle. Runtime dynamic loading, arbitrary
  Agent commands, user processes, hosted backend processes, and ambient Tool
  access remain prohibited.
