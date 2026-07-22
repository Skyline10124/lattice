# Issue #24 Slice 4 — Fetch, todo, default catalog, and retired methods

## Goal

Complete built-binary coverage of the default five-Tool distribution and prove
retired methods fail explicitly without side effects or fallback.

## Local web fixture

Add a harness-only HTTP fixture server with OS-assigned localhost port, tracked
connection threads, bounded shutdown, fixed status/headers/body, and captured
method/path. It is separate from the OpenAI-compatible fake provider.

```rust
let web = FixtureHttpServer::start(HttpFixture {
    status: 200,
    content_type: "text/plain",
    body: "fixture body",
})?;
```

Profile:

```toml
[tools]
enabled = ["fetch.web", "session.todo_write"]
[lancet.network]
allow_localhost = true
allowed_hosts = ["localhost"]
max_web_response_bytes = 64
```

## Scenarios

- `fetch.web` success against localhost; assert request path, status/body/type,
  response cap/truncation, CEP/audit host identity, and no public Internet.
- `fetch.web` omitted from `[tools].enabled`: forced model call fails before CEP
  dispatch and the fixture receives no request.
- enabled fetch rejected by policy/capability: CEP denial, no request/fallback.
- `session.todo_write` success echoes canonical todo JSON into the next model
  request and records session-state impact/audit.
- todo validation rejection (blank item, two `in_progress`, or budget exceeded)
  produces no hidden state. Do not claim a separate todo store exists.
- catalog exposure: provider request advertises exactly the profile-enabled
  encoded names; across slices the canonical default set is exactly
  `observe.workspace`, `mutate.create`, `mutate.edit`, `fetch.web`,
  `session.todo_write`.
- forced `exec.command_run`, `mutate.batch`, and agent audit method calls fail as
  unknown/disabled, emit no backend CEP activity, and leave files/network/audit
  side effects unchanged except the explicit failed Turn transcript.

## Exact replacement IDs

Land `planned_direct_fetch_capability_denial`,
`planned_direct_disabled_fetch_denial`, and the Tool-error/Tool-call assertions
that are publicly visible through next-request payloads. Retain raw registry
method-not-found invariants internally because no raw CEP CLI is introduced.

## Files

- `crates/lattice-system-harness/src/fixture_http.rs` (new; copyright/SPDX)
- `crates/lattice-system-harness/src/lib.rs`
- `crates/lattice-system-harness/tests/direct_tools.rs` (new)
- mapped internal tests/docs ledger after replacements pass

## Verification

```bash
cargo +1.96.0 test -p lattice-system-harness --lib fixture_http
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo +1.96.0 test -p lattice-system-harness --test direct_tools
cargo +1.96.0 xtask harness-boundary
```
