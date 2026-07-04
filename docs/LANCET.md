# LANCET runtime-enforced surface

> **DRIFT NOTICE — 2026-06-30:** This document was written against the
> v1.4-alpha boundary and contains several claims that no longer match the
> production code. Known drift corrected elsewhere in this pass:
> - The canonical **agent** CEP catalog has **5 methods**. `session.envelope`
>   was removed earlier; `exec.command_run` and post-mutation verify were
>   permanently retired; `observe.audit` / `observe.audit_replay` were removed
>   from the agent catalog (issue #11) — audit reads are operator-only via
>   `lattice audit` / `lancet-api`.
> - The production execution path is the **MSB/Upper workspace boundary**
>   (`lattice-workspace`); Direct-mode naming was retired and host-verify was
>   deleted. Write-scope is enforced as a blacklist, not an allowlist envelope.
> - The `[lancet.isolation]` profile section and landlock/netns command
>   isolation were **removed** (commit a29072e2). The
>   "Command physical-boundary configuration", overlay/artifact commit, and
>   live-canary sections below describe that **removed** system and are
>   retained as historical design context only — do not treat them as current
>   behavior. Current configuration is documented in `docs/CONFIGURATION.md`
>   and current architecture in `docs/ARCHITECTURE.md`.
> - The session graph is **git-backed**, not SQLite. SQLite remains only for
>   CEP audit (`lancet-audit`).

LATTICE agents execute tools through **LANCET**, the controlled execution and failure-learning subsystem. LANCET uses **CEP** (Controlled Execution Protocol) internally: JSON-RPC methods with capability envelopes, policy gating, path guards, and a flat append-only audit store.

## Public surface (v1.4-alpha boundary contract)

The documented public LANCET contract is framed by **`lancet-cep`**, which
re-exports the CEP protocol/catalog/dispatch interface and owns tool exposure
plus base-authority planning. The underlying contract remains split across
**`lancet-protocol`** and **`lancet-catalog`**:

| Component | Description |
|-----------|-------------|
| `lancet-cep` | Public CEP facade: protocol/catalog/dispatch re-exports, catalog-derived `ToolDefinition`, `ToolExposure`, descriptor-declared base authority aggregation, and explicit `BaseAuthorityPlan` construction |
| `lancet-catalog::CATALOG` | The **5-method** canonical agent CEP catalog plus descriptor metadata, including method-local base authority requirements: `observe.workspace`, `mutate.create`, `mutate.edit`, `fetch.web`, `session.todo_write`. (`session.envelope`, `exec.command_run`, `mutate.batch`, post-mutation verify, and agent-visible audit CEP methods were removed — catalog and tests assert their absence.) |
| `lancet-api::audit` | Operator audit query/tail/replay over the project-keyed audit store (`query_audit`, `tail_audit`, `replay_audit`); not agent CEP methods. |
| Method params/results | Typed `*Params` / `*Result` structs per catalog method in `lancet-catalog` |
| `CepError` / `CepErrorCode` | Typed error families with `Retriability` in `lancet-protocol` |
| `ExecutionEnvelope` / `Capability` / `Scope` | CEP capability, scope, and impact envelope types in `lancet-protocol` |
| `PolicyHook` / `PolicyDecision` | Pluggable policy hook interface in `lancet-protocol` |
| Audit/provenance schemas | `ProvGraphFragment` wire shapes in `lancet-protocol`; audit rows, filters, and attestation persistence in `lancet-audit` |

Internal implementation is split across `lancet-authority` (default policy, Guard L1, active envelope, effect-region semantics) and `lancet-native` (production handlers, transport lifecycle, edit/web integration).

**Not public surface:** `lancet-native` handler internals, private low-level executor helpers, authority implementation details, and any `#[doc(hidden)]` re-exports. These are implementation details of the native LANCET server.

Pre-v1.3.1 method names are removed. They are not accepted by the adapter or
native registry, and LLM-facing tool definitions are projected only from the 5
canonical agent catalog methods. Tool exposure is separate from base authority:
enabled methods filter the model-visible catalog projection, while
catalog descriptors declare method-local base authority requirements.
`ToolExposure::base_authority()` aggregates those requirements, and
`BaseAuthorityPlan` materializes them with runtime policy inputs such as allowed
fetch hosts. The base envelope never grants workspace write or command scopes.
Projection APIs live in `lancet-cep`; `lancet-native` and `lancet-api` do not
carry compatibility projection shims.

### Effect-contract tool surface

The canonical method names are grouped by primary effect class:

| Method | Primary effect contract |
|--------|-------------------------|
| `observe.workspace` | Workspace observation. `mode=read_file` has single-path scope; `mode=grep`, `mode=list_directory`, and `mode=find_files` use root/directory observation scope. |
| `mutate.create` | Workspace mutation by pure file creation. Existing targets are rejected; missing parent directories are rejected. |
| `mutate.edit` | Workspace mutation of existing files through semantic replace/insert operations. The caller provides `expected_hash`; LANCET computes hashline anchors internally. |
| `fetch.web` | External observation through URL fetch policy. |
| `session.todo_write` | Session-state mutation for todo state. |

Audit query, tail, and turn replay are **not** catalog methods. Operators use
`lattice audit` (default: query/tail; `lattice audit replay --turn <id>` for
turn provenance reconstruction), backed by `lancet-api::{query_audit,
tail_audit, replay_audit}`; see **Provenance Audit** below.

### Release gate and self-check

`scripts/release-gate-v1.4.sh` is offline deterministic by default. A default
run uses existing Cargo tests and local scripts only; it does not require API
keys, provider calls, public network access, or live Linux kernel primitives.
The gate keeps release evidence in three separate proof categories:

- **live physical proof:** available only when `--live` is supplied or
  `LATTICE_LINUX_ISOLATION_LIVE=1` is set. This runs the single ignored Rust
  suite, `linux_isolation_live_canary_suite`; no shell-only or parallel live
  canary suite is part of the gate.
- **offline correctness proof:** deterministic non-kernel checks for boundary
  and commit correctness. HashLine retention is offline correctness proof per
  the v1.4-alpha release contract, and cleanup stress uses the same offline
  proof class; neither is relabeled as live physical proof.
- **dogfood proof:** no-key, no-uncontrolled-network production-path scenarios,
  including audit replay. This is not live kernel proof and does not require
  the overlay path to be applied on every developer host. Real-provider dogfood
  is outside the gate.

- **Offline release gate:** `scripts/release-gate-v1.4.sh` — runs `cargo fmt`, `cargo clippy`, targeted crate tests, dogfood suite, LANCET boundary check, and `lattice lancet doctor`. It is offline deterministic by default and runs live Linux physical canaries only with `--live` or `LATTICE_LINUX_ISOLATION_LIVE=1`.
- **LANCET self-check:** `cargo run -p lattice-cli -- lancet doctor` — verifies the production registry fully registers the CATALOG, in-process transport smoke-tests, base envelope constructs, and mode policy parses cleanly.
- **v1.4-alpha release record:** `docs/v1.4-alpha-release-record.md` — named evidence home for capable-host live canary output, host/kernel capability facts, failed canaries, skipped unsupported canaries, and the trust-claim decision. The local gate does not write this record and does not enforce the release-record requirement that at least one capable Linux host reports `failed=0` and `skipped_unsupported=0`; that capable-host condition is judged by the CI/release-record layer.
- **Warn-mode report:** `docs/lancet-warn-report.md` — coverage report for the canonical effect-contract surface; the baseline is N=0 leaks and no `fix-now` triage rows.
- **Capability boundary walkthrough:** `docs/v1.3-alpha-walkthrough.md` — explains what Layer-1 e2e proves and what is deferred to beta.

### Mode semantics

`mode = "off"` under `[lancet]` skips effect-region enforcement only. The CEP policy hook still runs; capability declarations are still required, and effect-kind denial remains a hard fail. Legacy direct tool calls are not re-enabled — there is no legacy tool executor path in the production stack.

Effect-kind denial is enforced at checker time, after runtime effects are
observed. Configure it as `deny = ["workspace_mutation"]` under
`[lancet.effects]` or `[effects]` in an external policy file. This is stronger
than disabling a method prefix: a command whose primary method is
`exec.command_run` can still produce a `WorkspaceMutation` side effect, and that
side effect is caught only by effect-kind denial.

### Command physical-boundary configuration

Physical command-boundary requests live under `[lancet.isolation.command]`.
This namespace is independent from `[lancet] mode` / `cep_mode`: setting
`[lancet] mode = "off"` disables only effect-region checking and does not
disable command isolation preflight, command policy, capability checks,
effect-kind denial, or native LANCET routing.

```toml
[lancet.isolation.command]
mode = "enforce"                 # enforce | audit | off
required_boundaries = ["network", "filesystem", "landlock"]
optional_boundaries = []
read_only_toolchain_paths = ["/usr", "/bin"]
scratch_paths = ["build/tmp"]
landlock = "auto"                # auto | off
unsupported_host = "mode_default" # mode_default | fail_closed | allow_unbounded | not_requested
```

The exact fields are:

| Field | Meaning |
|-------|---------|
| `mode` | Command physical-boundary mode: `enforce`, `audit`, or `off`. Missing command isolation config defaults to `off`. |
| `required_boundaries` | Boundary dimensions that must be satisfied for this command mode. Valid labels are `network`, `filesystem`, and `landlock`. |
| `optional_boundaries` | Boundary dimensions to attempt/report without failing `enforce` when unavailable. The same labels are accepted. A boundary cannot be both required and optional. |
| `read_only_toolchain_paths` | Declared toolchain/cache paths that the Phase 29 supervisor may expose read-only to command children. Missing sources are reported as diagnostics rather than widening the mount view. |
| `scratch_paths` | Declared scratch paths that the Phase 29 supervisor may expose as tmpfs scratch space. Phase 29 does not add overlay discard or artifact commit semantics. |
| `landlock` | Landlock behavior: `auto` or `off`. `landlock = "off"` is rejected when `required_boundaries` contains `landlock`. Landlock is filesystem refinement only. |
| `unsupported_host` | Unsupported-host policy: `mode_default`, `fail_closed`, `allow_unbounded`, or `not_requested`. Contradictory combinations are rejected. |

Mode behavior is intentionally conservative:

| Command isolation mode | Required boundary unavailable, disabled, unknown, or unsupported |
|------------------------|------------------------------------------------------------------|
| `enforce` | Fail closed before the command backend runs and record `effective_boundary` metadata with `failed_closed`, `unsupported`, or `unknown`. |
| `audit` | Allow the existing command backend to run, but record the requested boundary as `unbounded` or `unknown`. Audit mode is measurement, not protection. |
| `off` | Do not request physical boundaries. Per-command metadata is `not_requested`, while normal CEP command policy still applies. |

On non-Linux or otherwise unsupported hosts, `host_probe` reports each primitive
as unavailable with a reason. The same `enforce` / `audit` / `off` rules then
decide whether a requested command fails closed, runs with unbounded labels, or
is marked `not_requested`.

Phase 29 keeps the agent-visible command method stable: commands still enter the
runtime through `exec.command_run`, and physical isolation is a native
implementation detail under `[lancet.isolation.command]`. In isolated modes the
production command path builds a deterministic supervisor plan, starts the child
through the supervised pre-exec handoff, runs child-side Linux setup, reads the
setup report through a close-on-exec pipe, and records `effective_boundary`
metadata from that report. When a required boundary lacks applied evidence,
`enforce` fails closed before the backend command runs. In `audit`, failed setup
may fall back to the existing command backend, but that fallback is labelled
`unbounded` / `not_applied`, not `physically_bounded`. `lattice-agent`, the
catalog method count, and the LLM-facing tool schema do not gain a second command
tool.

The native supervisor setup order is: privilege strategy check, unprivileged
user namespace bootstrap when selected, network namespace, mount namespace,
minimal mount view, cwd mapping, allowlist environment plus fd cleanup, optional
Landlock refinement, then exec. The unprivileged bootstrap checks readable
kernel sysctls such as `unprivileged_userns_clone` and
`max_user_namespaces` before attempting `CLONE_NEWUSER`, then still treats the
real `unshare`, `setgroups`, `uid_map`, and `gid_map` operations as
authoritative. These stages earn boundary credit only from real applied setup
reports, not from host probes or precomputed plans.

The network claim is intentionally narrow. `network: physically_bounded` is
sound only when the command attempt reports applied `network_namespace` evidence
with `source_primitives = ["network_namespace"]`. The supervisor does not
create a veth device, default route, or host namespace join for this no-egress
case. Landlock, mount namespace, OverlayFS, and later overlay rollback cannot
earn network credit and cannot make network effects reversible.

The filesystem claim is tied to a minimal mount view, not to a mount namespace
alone. A fresh mount namespace initially inherits the host mount table, so Phase
29 filesystem evidence requires the mount namespace plus
`mount_view_status = "applied"` for the synthetic view. The intended view
exposes the project at `/project`, configured read-only toolchain/cache paths
when available, scratch tmpfs such as `/tmp`, and minimal devices only. Broad
`$HOME`, credential paths, `/run`, Docker sockets, agent sockets, and host temp
secret paths are not mounted by default. Phase 29 may leave the project root
read-write; Phase 30 owns overlay discard and artifact commit behavior.

Executable resolution is performed against the supervisor view. The command
executable must resolve through `/project` or a configured read-only
toolchain/cache path that is present in the mount plan. Read-only bind scope is
reported honestly; current diagnostics may say `read_only_bind_scope =
"top_level"` rather than implying recursive read-only enforcement.

In isolated modes, command planning constructs the child environment from an
explicit allowlist and generated supervisor values. It starts empty, admits only
validated caller-provided keys, generates `PATH` from planned in-view executable
directories, and sets scratch variables such as `TMPDIR` only when the matching
tmpfs is planned. Non-stdio inherited file descriptors count as applied only
when the child-side fd-cleanup stage reports success. The setup-report pipe is
marked close-on-exec, preserved only through fd cleanup so the setup report can
be written, then closed before the final command exec.

Landlock is optional filesystem refinement after the mount view is in place. It
reports `landlock_status = "unsupported"`, `"not_applied"`, or `"applied"`
honestly, with `landlock_abi_version` and `landlock_rights_mask` when known.
Landlock is not a network boundary and is not a substitute for minimal
mount-view evidence.

### Phase 30 overlay modes and artifact commit

Phase 30 keeps `exec.command_run` as the single command method. Overlay routing
is a native implementation detail selected from command impact and
`[lancet.isolation.command].mode`; it is reported through command metadata and
audit metadata, not through a new agent-visible command tool or public command
result shape.

When command isolation mode is `off`, no overlay is requested. Diagnostic
commands run through the real-workspace compatibility path and metadata records
`overlay_status = "not_requested"`. Workspace-mutation commands also use the
existing snapshot-diff path, so artifact adjudication records
`artifact_adjudication = "skipped_no_overlay"` rather than claiming overlay
protection.

In `enforce` and `audit`, diagnostic commands request a diagnostic overlay. The
command sees an overlay view and upper-layer changes are discarded on success,
nonzero exit, timeout, cancellation, denial, setup failure, and cleanup failure.
Diagnostic lower drift may be reported, but no artifact is committed from this
mode.

Artifact commands request artifact staging. The command writes to overlay upper,
then native LANCET scans that upper layer into a typed manifest before any real
workspace mutation. Unsupported artifact kinds are rejected and discarded in
v1.4-alpha, including device nodes, FIFOs, sockets, hardlinks, unsupported
extended attributes, opaque-directory markers, and other special host semantics.
Rejected kind denial records `artifact_adjudication = "denied_unsupported_kind"`
and `rejected_kinds`.

Accepted artifact manifests are checked against the active WritePath capability
and effect-region policy, then verified against a candidate artifact view. The
verify command runs with its own discard layer; verify writes never become part
of the artifact manifest, committed artifact set, or final lower-baseline
validation object. Verify metadata records `verify_layer`, with
`"discarded"` for a successful discarded verify layer and `"cleanup_failed"` if
verify cleanup could not complete.

Before artifact bytes reach the real workspace, LANCET performs a final lower
consistency recheck against the pre-verify lower/workspace baseline. Lower drift
denies commit, discards staged artifact content, and records
`lower_consistency = "drift_detected"` plus
`artifact_adjudication = "denied_lower_drift"`. Cleanup failures for command
children, overlay mounts, namespace references, verify layers, upper/work dirs,
or busy mountpoints are recorded in `cleanup_outcome`; artifact cleanup failure
blocks commit rather than applying a possibly stale artifact set.

Phase 31 adds a HashLine commit-baseline gate to the same final boundary. When
preserved patch hunk anchors are available, the serialized commit section
revalidates them against the pre-verify lower/workspace baseline before any real
workspace apply. Durable command metadata records
`commit_baseline_validation = "passed" | "failed"`,
`commit_baseline_failure_reason`, and `commit_baseline_details`. Failure
reasons distinguish `lower_drift`, `missing_anchor`, `hash_mismatch`,
`ambiguous_hunk`, and `verify_layer_contamination`; details carry compact
hashes, offsets, changed paths, or contaminated paths, not anchor source text.
`lower_consistency` remains the full-tree lower recheck status and is not reused
for hunk-level baseline failures.

When artifact commit is accepted, the real workspace apply is serialized inside
the native process: lower recheck and stable ordered apply run without
interleaving from another in-process artifact commit. This is serialized
no-interleaving alpha behavior, not crash-safe multi-file transaction
atomicity. If an apply fails partway through, metadata records
`commit_outcome = "partial"`, `applied_entries`, and `failed_entry`; v1.4-alpha
does not claim rollback for already-applied filesystem entries.

Audit fallback is explicit. In `audit` with unsupported-host fallback allowed,
setup failure can run the unbounded real-workspace path, but metadata records
`overlay_status = "not_applied"` and
`artifact_adjudication = "skipped_no_overlay"`. A skipped overlay is diagnostic
only; it is not physical-boundary proof.

Overlay/artifact metadata keys emitted by command metadata include:

| Key | Meaning |
|-----|---------|
| `overlay_mode` | `diagnostic`, `artifact`, `candidate_verify`, or `none` depending on the native path. |
| `overlay_status` | `applied`, `not_applied`, `setup_failed`, or `not_requested`. |
| `overlay_disposition` | `discarded`, `committed`, `denied`, or `not_applied`. |
| `lower_consistency` | Lower recheck status such as `unchanged`, `drift_detected`, `not_checked`, or `check_failed`. |
| `commit_baseline_validation` | HashLine/commit-baseline umbrella result: `passed` or `failed`. |
| `commit_baseline_failure_reason` | Baseline failure reason such as `lower_drift`, `missing_anchor`, `hash_mismatch`, `ambiguous_hunk`, or `verify_layer_contamination`. |
| `commit_baseline_details` | Compact baseline detail object containing hunk summaries, changed paths, or verify contaminated paths; never anchor source text. |
| `artifact_manifest_status` | Manifest outcome such as `accepted`, `denied`, or `not_scanned`. |
| `artifact_adjudication` | Artifact decision such as `accepted`, `skipped_no_overlay`, `denied_unsupported_kind`, `denied_write_scope`, `denied_effect_region`, `denied_verify_failed`, `denied_lower_drift`, or `denied_cleanup_failed`. |
| `verify_layer` | Candidate verify layer status such as `not_run`, `applied`, `discarded`, or `cleanup_failed`. |
| `cleanup_outcome` | Overlay cleanup result, including `cleanup_succeeded` or `cleanup_failed`. |
| `commit_outcome` | Artifact apply result such as `not_started`, `committed`, `denied`, or `partial`. |
| `rejected_kinds` | Unsupported artifact kind records with path, kind, and reason. |
| `applied_entries` | Artifact entries applied before success or partial failure. |
| `failed_entry` | The entry and error that stopped a partial apply or cleanup-gated commit. |

Overlay discard does not make network effects reversible. Command network
soundness remains attributed only to applied network namespace evidence, never
to OverlayFS, Landlock, mount namespaces, or artifact cleanup.

Phase 30 live overlay canaries are part of the ignored
`LATTICE_LINUX_ISOLATION_LIVE=1` suite. They classify each scenario as
`passed`, `skipped_unsupported`, or `failed`. Only `passed` is evidence on that
host; `skipped_unsupported` reports missing kernel/setup primitives and is
diagnostic only.

## Mutation and command observation

### Semantic edits

`mutate.edit` edits one existing file with semantic operations such as
`replace_text`, `insert_before`, and `insert_after`. The caller supplies the
file `expected_hash` from `observe.workspace`; LANCET verifies that hash and
computes hashline anchors inside the native edit backend. The LLM-facing tool
surface does not accept hashline patch documents.

The internal hashline backend still forces edits to be based on the current file
state before commit. Audit metadata records changed files and line-level
add/remove effects. Agent-visible workspace mutation is limited to
`mutate.create` and `mutate.edit` (single-file methods only).

### Create-only writes

`mutate.create` is create-only. It fails if the target already exists and does
not create missing parent directories. Existing-file edits must go through
`mutate.edit`.

### Command boundary metadata

`exec.command_run` remains the stable command method. The command can still be a
partial observation boundary, but v1.4 metadata separates host capability
probes from per-command protection claims; it does not turn LANCET into a
generalized sandbox.

Two metadata layers must stay distinct:

- `host_probe` reports whether the current process/host can use Linux
  primitives such as `overlayfs`, `network_namespace`, `mount_namespace`, and
  `landlock`.
- `effective_boundary` reports what a specific command requested, attempted,
  applied, failed closed on, or did not request for safety dimensions such as
  `network`, `filesystem`, and `landlock`.

`host_probe` is primitive-centered and current-host scoped. The stable primitive
labels are `network_namespace`, `mount_namespace`, `overlayfs`, and `landlock`.
The stable status labels are `available`, `unavailable`, `disabled`, and
`unknown`. Probe diagnostics describe helper availability, errno-style failures,
kernel/container/user-namespace policy, or unsupported platform details; they
are not themselves protection claims.

`effective_boundary` is command-centered and boundary-keyed. The stable boundary
labels are `network`, `filesystem`, and `landlock`. The stable status labels are:

| Status | Meaning |
|--------|---------|
| `physically_bounded` | The command has applied primitive evidence for that boundary. Network credit requires `network_namespace` evidence. Filesystem Phase 29 credit requires minimal mount-view evidence, not mount namespace alone or Landlock alone. |
| `unbounded` | The boundary was requested but the command ran without applied primitive evidence. |
| `not_requested` | The boundary was not requested or command isolation mode is `off`. |
| `failed_closed` | `enforce` mode blocked execution before the backend because a required boundary lacked applied evidence. |
| `unsupported` | The required boundary maps only to unsupported host primitives. |
| `unknown` | Probe data was missing, inconclusive, malformed, or otherwise not reliable enough to claim a boundary. |

Each boundary entry also carries `observed: bool`. `status` is physical
boundary credit: what the command requested, applied, failed closed on, or did
not request. `observed` is effect visibility: whether LATTICE observed command
effects in that dimension. These are independent. For example,
`network: { "status": "physically_bounded", "observed": false }` means the
no-egress network namespace supplied physical command bounds, but LATTICE did
not observe network traffic or network effects.

In v1.4-alpha, `filesystem.observed` is true only after overlay artifact
manifest generation has observed filesystem artifact effects. `network.observed`
is false because command network traffic is not observed; network safety comes
from no-egress physical command bounds, not rollback or traffic replay.
`landlock.observed` is false because Landlock is a filesystem refinement, not
an effect observation channel. No read-set observation, network rollback, or
sandbox guarantee. No sandbox guarantee means these fields are limited
dimension-specific evidence for `exec.command_run`, not a general containment
claim.

Audit/status JSON is keyed by boundary dimension:

```json
{
  "effective_boundary": {
    "network": {
      "boundary": "network",
      "status": "not_requested",
      "observed": false,
      "reason": "command isolation mode is off"
    },
    "filesystem": {
      "boundary": "filesystem",
      "status": "failed_closed",
      "observed": false,
      "reason": "required filesystem boundary requested but no applied primitive evidence exists"
    },
    "landlock": {
      "boundary": "landlock",
      "status": "unsupported",
      "observed": false,
      "reason": "required landlock boundary unsupported on this host"
    }
  }
}
```

Applied-supervisor metadata, when produced by a real setup report, includes
compact diagnostics that status rendering preserves:

```json
{
  "boundary_setup_failed_closed": false,
  "effective_boundary": {
    "network": {
      "boundary": "network",
      "status": "physically_bounded",
      "observed": false,
      "reason": "applied supervisor evidence accepted",
      "source_primitives": ["network_namespace"],
      "diagnostics": {
        "namespace_status": "applied",
        "env_policy": "allowlist",
        "fd_cleanup_status": "applied",
        "fd_cleanup_method": "close_range",
        "boundary_setup_failed_closed": "false"
      }
    },
    "filesystem": {
      "boundary": "filesystem",
      "status": "physically_bounded",
      "observed": true,
      "reason": "applied supervisor evidence accepted",
      "source_primitives": ["mount_namespace"],
      "diagnostics": {
        "namespace_status": "applied",
        "mount_view_status": "applied",
        "project_view_path": "/project",
        "cwd_view_path": "/project",
        "read_only_bind_scope": "top_level",
        "env_policy": "allowlist",
        "fd_cleanup_status": "applied"
      }
    },
    "landlock": {
      "boundary": "landlock",
      "status": "unsupported",
      "observed": false,
      "reason": "optional landlock boundary unsupported by supervisor attempt",
      "diagnostics": {
        "landlock_status": "unsupported",
        "landlock_abi_version": "unknown",
        "env_policy": "allowlist",
        "fd_cleanup_status": "applied"
      }
    }
  }
}
```

Failed child-side setup is reported without physical credit, for example:

```json
{
  "boundary_setup_failed_closed": true,
  "effective_boundary": {
    "network": {
      "boundary": "network",
      "status": "failed_closed",
      "observed": false,
      "reason": "required network boundary requested but supervisor attempt lacked applied evidence",
      "diagnostics": {
        "supervisor_backend": "native_linux",
        "namespace_status": "failed_closed",
        "env_policy": "failed",
        "fd_cleanup_status": "failed",
        "setup_failure_stage": "privilege_check",
        "setup_failure_reason": "check_unprivileged_user_namespace failed: kernel.unprivileged_userns_clone is disabled at /proc/sys/kernel/unprivileged_userns_clone"
      }
    }
  }
}
```

When Landlock applies, the same diagnostics use
`landlock_status = "applied"` and include a hexadecimal
`landlock_rights_mask`, for example `"0xffff"`. When the ABI exists but the
ruleset is disabled or application fails without a required fail-closed policy,
the status is `not_applied`. None of these Landlock outcomes can produce
`network: physically_bounded`.

For `physically_bounded` entries, `source_primitives` must name an allowed
source primitive. `network` accepts only `network_namespace`; `filesystem`
accepts `overlayfs`, `mount_namespace`, or `landlock`; and `landlock` accepts
only `landlock`. Landlock, OverlayFS, and mount namespaces never earn network
credit.

Effect observation metadata remains separate from boundary credit. Commands can
record `effect_observation = "partial"` and `observed_effects` such as
`command_execution` or `workspace_mutation_diff`, but boundary observation is
read from `effective_boundary.*.observed`. Runtime metadata does not add
`observed_dimensions` or `unobserved_dimensions` summary lists; consumers derive
that view from the typed boundary entries when needed.

Command allowlists are admission control only. They constrain which executable
argv prefixes may run; they do not describe everything a permitted binary can do
after it starts.

Boundary credit is attributed, not inferred:

- `network: physically_bounded` can only be constructed from netns evidence.
  Landlock, OverlayFS, and mount namespaces never earn network credit.
- `filesystem` credit may come only from the allowed primitive attribution set:
  OverlayFS, mount namespace, or Landlock.
- `landlock` credit may come only from Landlock evidence, and Landlock remains a
  filesystem refinement. It is not a network boundary.

`lancet.mode = "off"` / `cep_mode=off` skips only effect-region enforcement. It
does not disable `[lancet.isolation.command]` physical-boundary mode, policy,
capability checks, effect-kind denial, or native LANCET routing.

## LANCET CLI boundary reporting

`lattice lancet doctor` is informational by default. It prints or emits JSON for
the current `host_probe`, and unavailable primitives are warnings unless another
doctor check fails. Use `lattice lancet doctor --enforce-profile <profile>` when
the current host must satisfy a profile's
`[lancet.isolation.command].required_boundaries`; that gate exits nonzero when a
required boundary is unavailable, disabled, unknown, or unsupported.

`lattice lancet status` reports two different boundary layers:

- A current `host_probe` summary at the top, describing host primitive
  availability now.
- Recent `exec.command_run` `effective_boundary` metadata from audit rows,
  describing what those command attempts requested or failed to bound.

`status --json` includes the same split. It treats malformed
`effective_boundary` metadata or invalid `physically_bounded` attribution as
`unknown` rather than silently displaying a stronger protection claim.

### Live canary classification

v1.4-alpha live Linux canaries share the single ignored Rust suite
`linux_isolation_live_canary_suite`, gated by
`LATTICE_LINUX_ISOLATION_LIVE=1`. Each canary reports one of three outcomes:
`passed`, `skipped_unsupported`, or `failed`, followed by
`live_canary_summary: passed=N skipped_unsupported=N failed=N`.

`passed` is host-specific evidence for that canary on that host. In release-gate
shorthand, skipped_unsupported is diagnostic only and is not proof of physical
bounds; it must include an exact reason such as `network namespace unavailable`,
`mount namespace unavailable`, `user namespace disabled`,
`uid/gid map unavailable`, `Landlock ABI unsupported`, or
`operation not permitted`. `failed` means the canary attempted the behavior and
observed a violation or unexpected error.

v1.4-alpha physical-bound trust is claimed only from capable-host live
`passed` canaries captured in `docs/v1.4-alpha-release-record.md`. Unsupported,
warn, audit, off, or fallback execution is diagnostic output and must not be
represented as physical-bound proof. Extra rootless, container, distro, or
non-Linux runs may be recorded as diagnostics, but the trust claim needs at
least one capable Linux host with `failed=0` and `skipped_unsupported=0`.
`scripts/release-gate-v1.4.sh --live` runs the suite and prints that summary
for the operator to record; it does not itself gate on `skipped_unsupported`,
and it does not create or populate the release record. Judging the capable-host
condition is the release-record layer's job.

## Phase 29 non-claims

Phase 29 implements command boundary reporting and the applied-evidence model
for `exec.command_run`. Child-side raw-syscall supervisor setup is attempted in
the command child, but Phase 29 does not claim applied physical evidence unless
the setup report says the relevant stages were applied. Phase 29 does not
implement or claim:

- A generalized sandbox for arbitrary process behavior.
- Side-channel protection, cgroups, seccomp, resource containment, or broad
  host compromise resistance.
- Project read-set observation for arbitrary commands.
- No network rollback. OverlayFS, mount namespaces, and Landlock do not make
  network effects reversible or bounded.
- Cross-platform physical isolation. Non-Linux and unsupported Linux hosts are
  reported through unavailable/unsupported metadata.
- The Phase 30 overlay discard/commit artifact pipeline.
- The Phase 31 HashLine commit boundary or effect/audit-centered projection.

## Provenance Audit

### Flat Store, Projected Graph

Audit rows live in SQLite (`cep_audit_entries`). Production persistence uses the
**external project-keyed path** under the platform state directory
(`$XDG_STATE_HOME/lattice/projects/<sha256-canonical-root>/cep_audit.db` via
`lancet_api::audit_db_path_for_project`). Each finalized CEP call is one
append-only row. **PROV-DM** structure (`Activity`, `Entity`, `Agent`, relations)
is **not** stored as a graph; it is **projected** at read time into a
`ProvGraphFragment` wire type (operator `query_audit` / `replay_audit`).

### Agent Triple

Rows carry `session_id`, `turn_id`, and nullable `model_id` / `runtime_version` columns populated from `CepAuditContext` at `start_call` so replay can reconstruct the Agent node from audit alone.

### Metadata Seeds

Handler and transport layers write compact `metadata_json` keys such as paths
and `content_hash`. These fields are intentionally summary-only:
full request and response bodies are not stored in audit metadata.

Native handlers also persist compact execution evidence:

- `cep_policy` records the policy decision, declared effect ceiling,
  capability-match summary, budget/network facts when available, and the
  post-policy authorized region summary.
- `cep_preflight` records method-local pre-execution facts. Command calls
  include declared/effective impact, effective effect kinds,
  RunCommand allowlist evidence, and WritePath region summaries for
  workspace-mutating commands. Mutation calls include target paths, out-of-scope
  paths, expected-hash status, verify configuration, and replan reason when
  known.
- `approval_request` is emitted only for `PolicyDecision::HumanReview`. It is
  approval-ready evidence for a future outer loop: method, reason, required
  capability, and retry hint. LANCET does not auto-approve
  or prompt interactively in the native handler.

Operator audit APIs and `replay_audit` project `cep_policy`,
`cep_preflight`, `approval_request`, and `effective_boundary` from flat row
metadata into PROV activity attributes. This keeps audit rows append-only while
making preflight and approval evidence visible to replay clients.

Transport finalize merges catalog `required_capability` and effect-ledger `observed_capabilities` without changing the audit write lifecycle.

### Attestations

Per-turn **unsigned in-toto** statements are stored in `attestation_records` in the same audit database (not git-tracked files). Signing and key management are deferred beyond alpha; wire shapes remain forward-compatible.

### Time-Range Proxy

Monotonic `audit_id` range (`since_audit_id` / `until_audit_id`) is the
primary time-order proxy when wall-clock `started_at` bounds are omitted.
Optional `started_after` / `started_before` filter on the millis `started_at`
column.

### Operator audit reads

Audit store reads are **host/operator only**. `lattice audit` calls
`lancet-api::{query_audit, tail_audit}` against the project-keyed store (no CEP
server). `replay_audit` rebuilds turn traces and optional in-toto statements from
stored rows. `CepAuditStore::query_rows` returns only **finalized** rows
(`ended_at` and `result_or_error` set). Agents do not receive `ReadAudit` or
audit methods in `[tools].enabled`.
