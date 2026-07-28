# LATTICE

LATTICE is a session-centric LLM runtime whose Agent invokes a closed catalog
of structured Tools against the user's Local Workspace through LANCET.

## Language

**Local Workspace**:
The user's current project tree and the sole filesystem source of truth for
Agent observations and mutations.
_Avoid_: Lower, Turn Workspace, guest workspace, authoritative snapshot

**Policy-Visible Workspace**:
The Local Workspace paths the Agent may observe: tracked and non-ignored files
by default, plus explicitly included ignored paths, minus read denials and
System Path Exclusions. It is a filtered view of current Local state, not a copy.
_Avoid_: Workspace Projection, sandbox tree, transferred workspace

**Tool**:
A typed capability from the closed Agent catalog, with structured parameters
and results rather than an arbitrary program. A Tool call is one invocation of
that capability selected after an Agent Think step.
_Avoid_: command, script, plugin, executable, untyped tool string

**Tool Descriptor**:
The Tool-owned authoritative contract for its name, structured parameters,
result, capabilities, scopes, and observable effects. No parallel central
schema may disagree with it.
_Avoid_: registry-owned duplicate schema, adapter-specific tool definition

**Closed Tool Catalog**:
The exact approved set of Tool Descriptors present in a built runtime. The set
is fixed before runtime starts and composition fails on missing, duplicate, or
unapproved Tools. The default distribution contains five built-in Tools, while
source composition may approve additional Tools before a new runtime is built;
the resulting runtime cannot extend its catalog by loading code.
_Avoid_: runtime plugin registry, arbitrary tool loading, parallel central catalog

**Source Tool**:
A Tool authored and maintained independently as source, then admitted to runtime
composition through a Tool Bundle before the runtime is built. It has no ambient
host authority and can affect the world only through its Tool Host.
_Avoid_: trusted native dependency, runtime-loaded plugin, user process

**Tool Bundle**:
The installable form of a Source Tool: one authoritative Tool Descriptor, one
isolated executable artifact, and integrity/provenance evidence. Installing a
bundle never executes third-party build code on the user's host.
_Avoid_: source dependency, native executable, unverified artifact

**Tool Host**:
The sole authority boundary presented to a Tool. It exposes controlled
workspace, network, and session channels that enforce policy and record effects;
it does not expose ambient filesystem, network, process, or runtime internals.
_Avoid_: convenience context, raw backend handle, ambient host access

**Structured Workspace Operation**:
A typed observation or mutation of the Local Workspace whose data is never
reinterpreted as shell syntax or an Agent-authored instruction.
_Avoid_: execution backend, Upper operation, command string

**Local Revision**:
The content identity of an observed Local file state, used to ensure a later
mutation still applies to that state.
_Avoid_: baseline commit, Turn Snapshot, workspace version

**Mutation Precondition**:
A fact that must still hold before a Local mutation, such as an expected Local
Revision or the continued absence of a create target.
_Avoid_: merge base, best-effort overwrite

**Atomic Local Mutation**:
An approved structured change that becomes Local Workspace state immediately
without exposing a partially written target. It is not rolled back by a later
failure and does not isolate the Agent from concurrent external writers.
_Avoid_: checkpoint, delayed writeback, sandbox promotion

**Replan Required**:
The outcome when current Local state no longer satisfies a Tool call's Mutation
Precondition. The Agent must observe again and issue a new Tool call.
_Avoid_: automatic retry, automatic merge, replay

**System Path Exclusion**:
A runtime-owned path rule that can never be relaxed by profile policy, covering
project Git administrative state, LATTICE private state, and root escape.
_Avoid_: configurable deny pattern, projection exclusion

**Session History**:
The git-style graph of conversation turns and Tool transcripts. It records
workspace interactions but does not own Local Workspace contents.
_Avoid_: workspace checkpoint lineage, Local rollback log, retained sandbox

**Workspace History**:
The durable private record of Atomic Local Mutations LATTICE has successfully
installed. It lives in a private bare Git repository outside the Local
Workspace and the user's project Git, expresses before/after state as Git
trees, and covers only the paths LATTICE has mutated.
_Avoid_: whole-project snapshot, execution workspace, projection, checkpoint,
receipt store

**Workspace-History Object ID**:
The identity a completed Turn arrived at in the Workspace History. Recorded on
the Turn's immutable commit so the Turn's mutations stay resolvable after
session reopen, branch movement, and process restart. A Turn that installed no
mutation records the identity it inherited.
_Avoid_: workspace snapshot id, session commit id, checkpoint handle

**Rewind**:
An explicit move to an earlier Session History point that reverses only the
later Atomic Local Mutations made by LATTICE. It preserves unrelated external
changes and refuses to overwrite a path whose current Local Revision has
diverged from the recorded LATTICE result.
_Avoid_: whole-workspace snapshot restore, automatic overwrite, conversation-only reset

**Rewind Conflict**:
The structured refusal a Rewind returns when a LATTICE-touched path can no
longer be verified against its recorded post-mutation state — changed,
deleted, replaced by another kind of entry, or unreadable. It names every
divergent path with its recorded and observed Local Revision, and nothing
moves: not the Local Workspace, not the private workspace-history ref, not the
Session History ref.
_Avoid_: partial rewind, per-path skip, forced overwrite
