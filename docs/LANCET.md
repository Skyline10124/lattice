# LANCET runtime surface

LANCET is LATTICE's controlled Tool-execution subsystem. Production Tool calls
run through one reservation-gated WASM Component path in `lancet-runtime`.
Source Tool manifests and their generated artifacts are the only Tool contract
authority.

The default distribution embeds five Components:

- `observe.workspace`
- `mutate.create`
- `mutate.edit`
- `fetch.web`
- `session.todo_write`

Components have no ambient WASI filesystem, network, or process authority. They
can reach the host only through the imports declared by their manifests and
pinned by the generated composition metadata.

## Ownership

| Crate | Responsibility |
| --- | --- |
| `lancet-protocol` | Data-only invocation, error, Cell, event, and evidence contracts |
| `lancet-authority` | Cell admission, Guard L1, network policy, and workspace visibility policy |
| `lancet-runtime` | Closed Component composition, exact invocation execution, Tool Host imports, Local Workspace, network transport, evidence, cancellation, events, and private workspace history |
| `lancet-audit` | Project-keyed `InvocationStore` plus operator query, tail, and replay |
| `lancet-api` | Thin runtime and operator facade used by `lattice-api` and frontends |
| `lattice-api` | `TurnCoordinator`, PendingTurn reconciliation, and cross-store Turn saga |

`lattice-api::TurnCoordinator` is the sole product invocation owner. LANCET has
no standalone Tool product, caller-composed catalog, generic execution backend,
or runtime plugin interface.

## Closed catalog

Each Source Tool manifest owns its descriptor, scopes, capabilities, schemas,
and import declarations. `cargo xtask tool-codegen` generates the WIT bindings,
provider projection, host metadata, expected identity, and inventory
registration. The build links only generated registrations into the runtime.

Startup fails closed when a registration is missing or duplicated, or when the
descriptor, artifact digest, actual import set, ABI, or expected identity drifts.
Enabled Tool names only filter the immutable generated inventory; callers cannot
supply descriptors or executable implementations.

Provider-visible Tool definitions are projected directly from the generated
descriptors. There is no second hand-maintained method schema.

## Invocation lifecycle

One typed `InvocationId` identifies a Tool call across its binding,
`InvocationStore` record, Component call context, events, terminal material, and
`CellEvidenceTriple`.

The production sequence is:

1. `TurnCoordinator` binds the provider Tool call to an exact
   `InvocationBinding` and durably reserves the candidate ID.
2. The LANCET facade verifies that the method and canonical arguments match the
   binding.
3. `InvocationStore` durably advances `Reserved` to `DispatchStarted` before
   the Component receives control.
4. `lancet-runtime` admits the generated descriptor under the resolved Cell
   policy and constructs call-scoped workspace, mutation, or network hosts.
5. The Component runs with an independent cancellation signal and host-bound
   event sink.
6. The Turn saga publishes typed terminal material and the exact host-produced
   evidence triple.

An invocation that never crosses `DispatchStarted` is reported as not executed.
After `DispatchStarted`, loss of a reliable terminal answer is reconciled as
`Indeterminate`; reopening never resends the provider request or redispatches a
Tool.

Calls do not share cancellation identity or event identity. Dropping one caller
cannot cancel another invocation. Host event emission overwrites any
caller-supplied `invocation_id` with the bound ID.

## Cell admission and policy

Tool enablement and Cell admission are separate checks:

- the profile's enabled Tool list controls provider visibility;
- `[lancet.capabilities] deny` removes named manifest capabilities;
- `max_boundary` limits the maximum admitted Cell trust boundary;
- every host operation must be covered by an admitted descriptor Cell with the
  exact channel, scope, context operation, and external impact.

Unknown denied capability names fail runtime construction. The default policy
is fail closed.

Current profile sections are:

- `[lancet]`
- `[lancet.observation]`
- `[lancet.network]`
- `[lancet.workspace]`
- `[lancet.capabilities]`

## Local Workspace

The runtime constructs one concrete `LocalWorkspace` for a Turn. Component
imports cannot bypass its path resolution or installation rules.

### Observation

`observe.workspace` supports bounded read, list, grep, and find operations over
tracked and non-ignored Local files by default. `[lancet.workspace]` controls
explicit ignored-path inclusion and read denial. System Path Exclusions always
win.

All observation modes use the same host visibility decision. Hidden paths look
missing to the guest; the hidden reason is recorded only in evidence. Traversal,
content limits, and truncation are host-computed. Long grep/find walks check the
call cancellation signal and finish with a typed cancellation error.

### Mutation

`mutate.create` installs one new file and refuses an existing target.
`mutate.edit` edits one existing file using semantic replace/insert operations.
The caller must provide the `content_hash` returned by `observe.workspace` as
`expected_hash`.

The host rechecks Mutation Preconditions at the atomic installation boundary.
Successful installs are recorded in the project-keyed private
`workspace-history.git`; pre-install refusals do not move history. Workspace
history is separate from Session history and the invocation ledger.

`/rewind` verifies every affected path against recorded post-state, restores
through the same safe resolver and atomic installer, and moves history/session
refs only after workspace restoration succeeds.

## Network

`fetch.web` reaches the network only through the runtime's `network-fetch` host
import. The host enforces:

- GET only;
- explicit `allowed_hosts` matching;
- public HTTPS policy and optional explicit localhost policy;
- complete private, reserved, and special-address rejection;
- DNS resolution with connection pinning and rebinding checks;
- TLS verification;
- per-hop redirect validation;
- header, body, deadline, cancellation, and shared observation-budget limits.

Evidence records normalized per-hop endpoints without userinfo, query, or
fragment. It distinguishes bytes received by the host from bytes delivered to
the guest. Post-I/O failures remain effects rather than being rewritten as
pre-I/O denials.

## Evidence and errors

Every executed call returns an `ExecutionReceipt` containing a typed result and
`CellEvidenceTriple`:

- admission evidence explains which capabilities and boundary were admitted or
  refused;
- observed effects record host-observed operations and result effects;
- denied operations record refusals before a host facility ran.

Evidence is content-free: Tool arguments, result bodies, fetched bytes,
credentials, and host filesystem paths are not persisted. Runtime failures map
to typed `CepError` families with explicit retriability and bounded, redacted
messages.

## Operator ledger

Accepted invocations live in:

```text
$XDG_STATE_HOME/lattice/projects/<ProjectStoreKey>/invocations.git
```

The Git ledger owns ordered batch reservation, durable `DispatchStarted`, atomic
terminal-and-evidence publication, and pinned linear-ancestry reads.

Operator commands are host-only and are not Agent Tools:

- `lattice audit` queries a pinned view;
- `lattice audit tail` reads ordered lifecycle transitions from an exclusive OID
  cursor;
- `lattice audit replay --turn <TurnId>` returns one Turn's invocation trace in
  model-step/part order;
- `lattice lancet status` reports the pinned ledger OID and recent typed states.

Operator reads use `lancet-audit` directly through the `lancet-api` facade. They
do not compose the Tool runtime or take the workspace operation lock.

## Doctor and verification

`lattice lancet doctor` canonicalizes the Local Workspace and verifies that the
generated closed Component catalog composes. It exits nonzero if either check
fails; `--json` emits the same structured result.

Useful verification commands:

```bash
cargo xtask tool-codegen --check
cargo xtask tool-build --verify
cargo xtask lancet-boundary
cargo test -p lancet-runtime -p lancet-api -p lattice-api -p lattice-system-harness
```

See [TESTING.md](./TESTING.md) for the full workspace gate.
