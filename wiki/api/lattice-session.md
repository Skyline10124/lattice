# lattice-session API

`lattice-session` stores conversations as session graphs with git-style refs and content-addressed objects.

## Main Types

- `Session`: model/provider metadata, messages, timestamps and graph metadata.
- `SessionMessage`: role/content plus tool-call fields.
- `SessionToolCall`: persisted tool call metadata.
- `SessionStore`: async-safe persistence trait used by runtime and CLI.
- `FileSessionStore`: file-backed store.
- `SqliteCasSessionStore`: SQLite content-addressed store.
- `TurnManifestObject`: prompt fragments, hook decisions and tool decisions for one turn.
- `ArtifactObject` and `ArtifactSummary`: persisted artifacts attached to sessions.

## Git-Style Operations

The store supports branch, checkout, merge, rebase, cherry-pick, stash, tag, diff, log and GC-style operations over session objects.

`lattice-runtime::messages_for_session()` converts persisted sessions back into `lattice-core::types::Message` values for agent continuation.
