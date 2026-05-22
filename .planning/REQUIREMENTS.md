# Requirements: LATTICE v1.2-alpha Evented Runtime

**Defined:** 2026-05-21
**Core Value:** A user can run one session-centered generic agent through a small, understandable Rust runtime with simple model routing, simple tools, commit-backed history, and a CLI surface that does not hide extra architecture.
**Design Source:** `.planning/alpha-v1.2-evented-runtime-design.md`

## v1.2-alpha Requirements

### Event Vocabulary

- [x] **EVT-01**: API consumers can observe turn execution through explicit `TurnEvent` variants: `TurnStarted`, `ModelStarted`, `ReasoningDelta`, `TokenDelta`, `ToolCallStarted`, `ToolCallFinished`, `ModelFinished`, `TurnFailed`, `TurnCancelled`, `TurnCommitting`, `TurnCommitted`, and `CommitFailed`.
- [x] **EVT-02**: Agent internals emit process-only `AgentTurnEvent` values and do not emit terminal `AgentFailed`, `AgentCancelled`, or generic started/done events.
- [x] **EVT-03**: Terminal status vocabulary across agent, API, and commit layers uses `Completed`, `Error`, and `Cancelled`; `Failed` is removed as a status variant.
- [x] **EVT-04**: Public streamed JSON events have a stable discriminator shape suitable for CLI and future frontend consumers.
- [x] **EVT-05**: Event vocabulary can represent the alpha event ordering contract; behavioral ordering tests are assigned to later verification requirements.

### LLM Streaming

- [x] **LLM-06**: `LlmClient` exposes `stream_chat(request)` returning an `LlmStream` while keeping `chat(request)` available for temporary compatibility and tests.
- [x] **LLM-07**: Existing `LlmClient` implementations continue to compile through a default `stream_chat` fallback that faithfully converts `ChatResponse` content, reasoning_content, tool_calls, finish_reason, and usage into a single-blob stream.
- [x] **LLM-08**: Tests can use a configurable `FakeStreamingLlmClient` that emits token, reasoning, tool-call, done, and error event sequences without provider credentials.
- [x] **LLM-09**: Production LLM execution can consume the existing provider `LlmEvent` stream without adding a model catalog, router policy, or new runtime layer.

### Agent Loop

- [x] **AGENT-03**: AgentLoop consumes `stream_chat` for model requests and emits `ModelStarted`, per-chunk `TokenDelta`, per-chunk `ReasoningDelta`, and `ModelFinished` on successful model completion.
- [x] **AGENT-04**: AgentLoop finalizes streaming deltas into transcript `GraphMessage` values, storing visible content separately from reasoning content.
- [x] **AGENT-05**: AgentLoop preserves complete partial assistant content/reasoning on LLM stream error or explicit cancellation and omits incomplete tool calls from the transcript.
- [x] **AGENT-06**: Tool execution remains sequential, with `ToolCallStarted` emitted only immediately before real execution and `ToolCallFinished` carrying only summary fields.
- [x] **AGENT-07**: A failed tool call is represented as a tool result and does not automatically make the turn status `Error`.
- [x] **AGENT-08**: AgentLoop exposes `run_turn_live(request, sender)` while preserving `run_turn(request)` and the complete buffered `AgentTurnResult.events` vector.
- [x] **AGENT-09**: Cancellation is cooperative and explicit: cancellation before a tool skips the tool, cancellation during a tool waits for the real result, and timeout/max-round/provider failures are `Error`, not `Cancelled`.

### Commit Semantics

- [x] **COMM-01**: Each turn commit stores only the current turn's transcript delta plus metadata, not the full HEAD transcript and not live event logs.
- [x] **COMM-02**: Completed, Error, and Cancelled turns all commit and advance HEAD when persistence succeeds. *(Phase 16 covers Completed and Error via public live path tests; Cancelled commit semantics are guaranteed by `derive_terminal_event` + `build_commit_input` code path but lack a public live path test until Phase 17 introduces a cancellation handle — see VER-12.)*
- [x] **COMM-03**: Commit persistence failure emits `CommitFailed`, emits no `TurnCommitted`, and leaves HEAD unchanged.
- [x] **COMM-04**: Commit messages are derived from the first user message, bounded to 72 characters, and do not encode status prefixes.
- [x] **COMM-05**: `terminal_reason` stays a short human-readable string and no recovery-hint or failed-turn rewrite system is introduced.

### API Streaming

- [x] **API-04**: `stream_turn` prepares profile, model, session, parent commit, optional branch metadata, and head transcript before spawning any live task.
- [x] **API-05**: `stream_turn` returns startup failures as `Result::Err` before returning a stream and reports runtime outcomes as `TurnEvent` values after the stream starts.
- [x] **API-06**: The live public stream uses a single spawned task, bounded `mpsc` channel capacity 256, and `tokio-stream` receiver wrapping.
- [x] **API-07**: The live task maps `AgentTurnEvent` values to public `TurnEvent` values and derives `TurnFailed` / `TurnCancelled` from `AgentTurnResult.status`.
- [x] **API-08**: The live stream drains agent events before terminal and commit events, without nested bridge spawns that can reorder events.
- [x] **API-09**: The live `stream_turn` path bypasses `TurnDeps`; `TurnDeps` remains the buffered test seam for `run_turn_with_deps` and existing tests.
- [x] **API-10**: Dropping the event receiver does not cancel the turn; send errors are ignored and the background task still commits when possible.

### CLI

- [x] **CLI-05**: CLI `run` consumes public `TurnEvent` values and renders `TokenDelta` as assistant output as the stream arrives.
- [x] **CLI-06**: CLI rendering keeps reasoning hidden by default, reports tool start/finish as compact status lines, and sends `TurnFailed` / `CommitFailed` to stderr.
- [x] **CLI-07**: CLI final output surfaces committed session id, commit id, and terminal status without depending on internal `AgentTurnEvent` names.

### Verification

- [x] **VER-07**: Tests prove old status and event names are removed and `Error` maps to `TurnCommitStatus::Error`.
- [x] **VER-08**: Tests prove token deltas commit as one assistant message and reasoning deltas do not pollute visible assistant output.
- [x] **VER-09**: Tests prove tool-call event timing, failed-tool recovery, and sequential tool execution.
- [x] **VER-10**: Tests prove partial assistant output is committed on LLM error and cancelled turns commit with `Cancelled` status.
- [x] **VER-11**: Tests prove cancellation before tool execution skips the tool and cancellation during tool execution waits for the tool result.
- [x] **VER-12**: Tests prove receiver drop does not cancel a live turn and commit failure does not move HEAD.
- [x] **VER-13**: Tests prove `stream_turn` emits token/model/tool activity before commit and remains semantically consistent with `run_turn`.
- [x] **VER-14**: The release gate passes targeted crate tests for `lattice-llm`, `lattice-agent`, `lattice-api`, and `lattice-cli`, plus formatting and workspace compile checks.

## Future Requirements

### v1.2-beta and Later

- **FUT-06**: Add a public cancellation handle or token for `stream_turn` callers.
- **FUT-07**: Consider unifying `run_turn` on top of the live execution path after alpha stream semantics stabilize.
- **FUT-08**: Add a TUI based on public `TurnEvent` after CLI rendering proves the event shape.
- **FUT-09**: Consider durable event logs only after transcript commit semantics remain stable.
- **FUT-10**: Consider explicit history editing for failed/cancelled turns as a separate user-visible feature.

## Out of Scope

| Feature | Reason |
|---------|--------|
| Durable event log or replay | v1.2-alpha treats events as live observation, not canonical history. |
| Runtime crate extraction | `lattice-api` stays a thin facade and transaction boundary for this milestone. |
| Public `TurnAccumulator` / `TurnRunState` | Private helpers are enough to prove streaming finalization. |
| Tool-call argument deltas or output previews | Arguments and full outputs belong in transcript messages, not live events. |
| In-flight tool cancellation | Alpha cancellation is cooperative at agent-loop boundaries only. |
| Parallel tool execution | Sequential execution preserves v1.1 behavior and keeps ordering testable. |
| Recovery hints or automatic failed-turn rewriting | Error and Cancelled turns are ordinary commits in history. |
| Model catalog / automatic routing | Explicit aliases remain the model routing story. |
| TUI | CLI live rendering is the only frontend surface in this milestone. |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| EVT-01 | Phase 12 | Complete |
| EVT-02 | Phase 12 | Complete |
| EVT-03 | Phase 12 | Complete |
| EVT-04 | Phase 12 | Complete |
| EVT-05 | Phase 12 | Complete |
| LLM-06 | Phase 13 | Complete |
| LLM-07 | Phase 13 | Complete |
| LLM-08 | Phase 13 | Complete |
| LLM-09 | Phase 13 | Complete |
| AGENT-03 | Phase 14 | Complete |
| AGENT-04 | Phase 14 | Complete |
| AGENT-05 | Phase 14 | Complete |
| AGENT-06 | Phase 14 | Complete |
| AGENT-07 | Phase 14 | Complete |
| AGENT-08 | Phase 15 | Complete |
| AGENT-09 | Phase 15 | Complete |
| COMM-01 | Phase 16 | Complete |
| COMM-02 | Phase 16 | Complete |
| COMM-03 | Phase 16 | Complete |
| COMM-04 | Phase 16 | Complete |
| COMM-05 | Phase 16 | Complete |
| API-04 | Phase 16 | Complete |
| API-05 | Phase 16 | Complete |
| API-06 | Phase 16 | Complete |
| API-07 | Phase 16 | Complete |
| API-08 | Phase 16 | Complete |
| API-09 | Phase 16 | Complete |
| API-10 | Phase 16 | Complete |
| CLI-05 | Phase 18 | Complete |
| CLI-06 | Phase 18 | Complete |
| CLI-07 | Phase 18 | Complete |
| VER-07 | Phase 12 | Complete |
| VER-08 | Phase 14 | Complete |
| VER-09 | Phase 14 | Complete |
| VER-10 | Phase 17 | Complete |
| VER-11 | Phase 15 | Complete |
| VER-12 | Phase 17 | Complete |
| VER-13 | Phase 17 | Complete |
| VER-14 | Phase 18 | Complete |

**Coverage:**
- v1.2-alpha requirements: 39 total
- Mapped to phases: 39
- Unmapped: 0

---
*Requirements defined: 2026-05-21 from evented runtime design draft*
