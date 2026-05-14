# Codebase Concerns

**Analysis Date:** 2026-05-14

## Tech Debt

**High impact: security defaults are inconsistent across entry points**

- Issue: `SandboxConfig::default()` treats empty read/write allowlists as unrestricted, while the CLI/runtime default `project` mode restricts reads and writes to the current workspace. Direct uses of `DefaultToolExecutor::new()`, test helpers, or future entry points can bypass the intended runtime-level security semantics.
- Files: `crates/lattice-sandbox/src/config.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-tools/src/executor.rs`, `crates/lattice-orchestrator/src/assembly.rs`, `crates/lattice-runtime/src/preset.rs`
- Impact: A new tool execution path that does not explicitly call `.with_sandbox(security.sandbox.clone())` inherits the looser low-level default. Sensitive-file protections still exist, but normal path and command boundaries are weaker than CLI users expect.
- Fix approach: Make `DefaultToolExecutor::new()` documentation and type shape clearly express "not bound to runtime security". Prefer a restricted constructor such as `DefaultToolExecutor::new_restricted(project_root, RuntimeSecurity)` or a builder that requires `SandboxConfig`. Add runtime/orchestrator assembly regression tests asserting that production tool executors always carry runtime sandbox state.

**High impact: manifest persistence depends on store capabilities and can silently degrade auditability**

- Issue: `SessionStore::save_with_manifests()` returns an unsupported default, and `finalize_session_turn()` falls back to `store.save(&session)`. The fallback preserves conversation text but drops turn manifests. The CLI default `SqliteCasSessionStore` supports manifests, but tests and custom runtimes may pass `FileSessionStore`.
- Files: `crates/lattice-session/src/lib.rs`, `crates/lattice-runtime/src/turn.rs`, `crates/lattice-cli/src/session.rs`, `crates/lattice-cli/src/test_support.rs`
- Impact: Features that depend on `sessions manifest` for prompt fragments, hook decisions, or tool decisions can return empty results under some stores, making planning and verification flows think no context evidence exists.
- Fix approach: Make manifest support an explicit capability, for example `SessionStoreCapabilities`. Emit a structured warning/event when runtime saves through a store without manifest support. Use `SqliteCasSessionStore` in test helpers except where legacy JSON behavior is being tested intentionally.

**Medium-high impact: several core modules are very large single files**

- Issue: Core responsibilities are concentrated in a few large files: `crates/lattice-session/src/lib.rs` is about 4,841 lines, `crates/lattice-runtime/src/lib.rs` about 2,922 lines, `crates/lattice-tools/src/executor.rs` about 2,076 lines, and `crates/lattice-cli/src/frontend/tui/app.rs` about 1,992 lines.
- Files: `crates/lattice-session/src/lib.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-tools/src/executor.rs`, `crates/lattice-cli/src/frontend/tui/app.rs`
- Impact: Session graph, CAS, manifests, GC, runtime wiring, tool execution, and TUI state all evolve inside large files. Local changes are harder to review and can disturb unrelated behavior.
- Fix approach: Split by existing boundaries without behavior rewrites. Good first targets are `lattice-session` modules for `sqlite_store`, `file_store`, `manifest`, and `gc`; `lattice-tools` modules for filesystem, bash, web, and workspace context; and TUI command handling/session/render/update glue.

**Medium impact: official plugin features are all enabled by default and registration is coupled to sandbox behavior**

- Issue: `lattice-plugins` default features enable `code-review`, `refactor`, `test-gen`, `security-audit`, `doc-gen`, `pptx-gen`, `deep-research`, `image-gen`, `knowledge-base`, `plan-gen`, and `verification`. `official_plugin_bundles()` registers bundles for enabled plugins and uses `BehaviorMode::Yolo { enforce_sandbox: EnforceCommandAllowlist }`.
- Files: `crates/lattice-plugins/Cargo.toml`, `crates/lattice-plugins/src/lib.rs`, `crates/lattice-cli/src/runtime.rs`
- Impact: The default build exposes a broad plugin surface. Adding a plugin to default features automatically expands the runtime pipeline surface. Command allowlists help, but data access, tool selection, and prompt authority still need per-plugin review.
- Fix approach: Add a security classification and test matrix for plugin features. New official plugins should include tool/sandbox expectation tests and a documented reason for default registration near `register_official_plugins()`.

**Medium impact: a synchronous hook performs async download work**

- Issue: `VetHook::pre_execute()` is a synchronous trait method, but internally uses `tokio::runtime::Handle::block_on()` to perform a reqwest download, then rewrites the command to `cat /tmp/lattice-vet-.../file`.
- Files: `crates/lattice-agent/src/hook.rs`, `crates/lattice-agent/src/sandbox.rs`, `crates/lattice-runtime/src/lib.rs`
- Impact: Blocking inside an existing Tokio runtime can amplify latency under slow network conditions or many concurrent hooks. The downloaded artifact is written to a system temp directory and depends on later command allowlist/Landlock checks to preserve the execution boundary.
- Fix approach: Evolve hook traits toward async hooks, or move VetHook download work into a dedicated blocking task. Reuse the sandbox URL SSRF validation path for all download logic.

## Known Bugs

**Unresolved `RTK.md` instruction reference**

- Symptoms: The root instruction include list references `@RTK.md`, but no `RTK.md` file exists at the repository root.
- Files: `AGENTS.md`, `CLAUDE.md`, `RTK.md`
- Trigger: Tools or agents that resolve `@RTK.md` project instructions will encounter a missing file.
- Workaround: Continue using `AGENTS.md` and `CLAUDE.md`. Do not create an empty placeholder `RTK.md`. If the file is required, maintainers should add the real content.

**Repository docs omit `lattice-policy` from the crate table**

- Symptoms: `README.md`, `AGENTS.md`, and `CLAUDE.md` list most crates but do not include the workspace crate `crates/lattice-policy`.
- Files: `README.md`, `AGENTS.md`, `CLAUDE.md`, `Cargo.toml`, `crates/lattice-policy/Cargo.toml`
- Trigger: New agents or planning tasks that rely on the docs can miss that `lattice-policy` is the canonical home for sandbox policy types and approval decisions.
- Workaround: For security, approval, and sandbox work, treat `Cargo.toml` and actual imports as authoritative.

## Security Considerations

**Sandbox protections are distributed across multiple layers**

- Risk: URL, command, path, hook, Landlock, and audit logic is spread across `lattice-sandbox`, `lattice-policy`, `lattice-agent`, `lattice-tools`, and `lattice-runtime`. URL SSRF checks appear in both `lattice-sandbox/src/security.rs` and `lattice-core/src/security.rs`. Command allowlists, metacharacter checks, dynamic approvals, and hook chains also live in separate modules.
- Files: `crates/lattice-sandbox/src/checks.rs`, `crates/lattice-sandbox/src/security.rs`, `crates/lattice-core/src/security.rs`, `crates/lattice-policy/src/authority.rs`, `crates/lattice-agent/src/hook.rs`, `crates/lattice-tools/src/executor.rs`
- Current mitigation: Command metacharacters cannot be bypassed by approvals; path matching uses component boundaries; base URLs reject non-HTTP(S), userinfo, non-localhost HTTP, and private IP resolution; `web_search` blocks redirects and limits response size.
- Recommendations: Consolidate or share SSRF/private-IP logic to avoid drift. New security logic should cover runtime entry points, tool executor entry points, and custom plugin entry points.

**Landlock is disabled by default and best-effort when enabled**

- Risk: CLI config defaults `security.landlock = false`, and runtime defaults also keep Landlock off. Even when enabled, `apply_landlock_sandbox()` returns `Ok(())` when unsupported or when rule addition fails, so execution does not hard-fail.
- Files: `crates/lattice-cli/src/config.rs`, `crates/lattice-cli/src/commands/config_cmd.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-sandbox/src/config.rs`, `crates/lattice-sandbox/src/landlock.rs`, `crates/lattice-tools/src/executor.rs`
- Current mitigation: User-space sandbox checks still enforce allowlists, sensitive-file rules, command checks, and URL checks. Landlock is defense in depth.
- Recommendations: Expose actual Landlock activation status through audit/events/logs in `strict` and `project` modes. Provide a configurable hard-fail mode when `landlock=true` is requested but unsupported or ineffective.

**Credential lifecycle is redacted, but literal API keys are still accepted in config**

- Risk: `CredentialStore::from_config()` supports `api_key = "literal"` and only emits a warning. Error classification and `ResolvedModel` debug/serialization are redacted, but literal keys can still land in local config.
- Files: `crates/lattice-cli/src/credentials.rs`, `crates/lattice-cli/src/config.rs`, `crates/lattice-core/src/catalog/types.rs`, `crates/lattice-core/src/errors.rs`
- Current mitigation: `CredentialStore` debug output hides keys and values; values are zeroized on drop; literal keys warn; `.env.example` is only a template.
- Recommendations: Reject literal API keys by default, or require an explicit `allow_literal_api_keys = true`. Keep error-body redaction tests.

**Security findings exist in the repo but should not become runtime data**

- Risk: The root contains files such as `findings/inline-secrets.txt`, `findings/git-history-credential-scan.txt`, and `findings/credential-lifecycle-review.md`. These appear to be security review artifacts and may contain historical evidence or sensitive context.
- Files: `findings/inline-secrets.txt`, `findings/git-history-credential-scan.txt`, `findings/credential-lifecycle-review.md`
- Current mitigation: This mapping pass did not read those file contents. Sandbox sensitive-file rules block common credential files, but the `findings/` directory itself is not a default sensitive path.
- Recommendations: Clarify the git and publication policy for `findings/`. If it may contain secret fragments, add it to sensitive-read policy or move it to private audit storage.

## Performance Bottlenecks

**SQLite store opens a connection and initializes schema for many operations**

- Problem: `SqliteCasSessionStore::conn()` creates the directory, opens `session.db`, runs `CREATE TABLE IF NOT EXISTS`, and performs migration checks. Most store methods call `self.conn()?`.
- Files: `crates/lattice-session/src/lib.rs`
- Cause: This simplifies connection lifetime but repeats initialization work for frequent session operations, TUI refreshes, manifest queries, and GC preflight.
- Improvement path: Introduce a connection pool or long-lived connection handle and converge migration initialization into construction or explicit `init()`. Keep current SQLite transaction boundaries intact.

**Recursive grep/find tools may be slower than system tools on deep trees**

- Problem: `grep_recursive()` and `find_files_recursive()` walk directories in Rust, canonicalize paths, call `check_read`, skip hidden directories by default, enforce max depth 32, and apply configurable result limits.
- Files: `crates/lattice-tools/src/executor.rs`
- Cause: The implementation avoids calling ripgrep/find directly so sandbox, TOCTOU, and limit handling stays consistent.
- Improvement path: Preserve the current implementation as a safe fallback. Under project sandbox, evaluate a restricted `rg --json` fast path that still goes through equivalent allowlist and sensitive-file filtering.

## Fragile Areas

**Runtime handle registry uses synchronous mutexes around async turn state**

- Files: `crates/lattice-runtime/src/lib.rs`, `crates/lattice-runtime/src/turn.rs`, `crates/lattice-runtime/src/handle.rs`
- Why fragile: Handle registry, approval rules, and task registry state use `std::sync::Mutex` in several places. Current lock scopes appear short, but turn streaming, cancellation, approval, and fork paths all touch shared state. Future await-adjacent logic can easily introduce guard-holding mistakes.
- Safe modification: Do not `.await` while holding `std::sync::MutexGuard`. Add concurrency/cancellation tests before new handle operations, and extract small helper functions to shorten lock scopes.
- Test coverage: `crates/lattice-runtime/src/turn.rs` and `crates/lattice-runtime/src/lib.rs` have many inline tests, but no dedicated external stress tests.

**Session graph supports both legacy JSON and SQLite CAS stores**

- Files: `crates/lattice-session/src/lib.rs`, `crates/lattice-cli/src/session.rs`, `crates/lattice-cli/src/commands/sessions.rs`
- Why fragile: Branch, tag, stash, merge, rebase, GC, artifact, and manifest behavior are not capability-equivalent across stores. SQLite supports CAS/manifests; file store mostly provides legacy compatibility.
- Safe modification: New session functionality should declare capability differences at the trait level. Write primary-path tests for `SqliteCasSessionStore`; treat legacy JSON behavior as compatibility only.
- Test coverage: `lattice-session` has broad inline tests despite being a single source file. After splitting, store-specific test modules would make regressions easier to localize.

**TUI state machine is concentrated in one app file**

- Files: `crates/lattice-cli/src/frontend/tui/app.rs`, `crates/lattice-cli/src/frontend/tui/update.rs`, `crates/lattice-cli/src/frontend/tui/session_state.rs`, `crates/lattice-cli/src/frontend/tui/bash_command.rs`
- Why fragile: `app.rs` handles sandbox-mode switching, approval, session state, render state, and command routing. New UI behavior can interact with unrelated state.
- Safe modification: Put new behavior into narrower existing modules such as `update.rs`, `session_state.rs`, and `bash_command.rs`; keep only shared state fields in `app.rs`.
- Test coverage: `crates/lattice-cli/src/frontend/tui/render_test.rs` and `crates/lattice-cli/tests/frontend_runtime_guards.rs` exist, but interaction-level coverage is still limited.

## Scaling Limits

**Test distribution is uneven across crates**

- Current capacity: Dedicated external tests are concentrated in `lattice-core`, `lattice-cli`, `lattice-agent`, `lattice-sandbox`, and `lattice-tools`. Relevant paths include `crates/lattice-core/tests/*`, `crates/lattice-cli/tests/*`, `crates/lattice-agent/tests/*`, `crates/lattice-sandbox/src/tests.rs`, and `crates/lattice-tools/tests/exposure.rs`.
- Limit: `lattice-hooks`, `lattice-session`, `lattice-orchestrator`, `lattice-plugin`, `lattice-policy`, `lattice-prompt`, and `lattice-runtime` rely mainly on inline unit tests or a small number of files. Cross-crate contracts can be missed during refactors.
- Scaling path: Add contract tests for policy/sandbox approval, session manifest persistence, runtime tool executor assembly, plugin registration feature matrix, and prompt instruction loading.

**Workspace dependency surface is broad**

- Current capacity: The workspace uses `tokio = { features = ["full"] }`, `reqwest` streaming/json/rustls, bundled `rusqlite`, optional SQLite `sqlx`, Linux-only `landlock`, and `notify` with macOS kqueue.
- Files: `Cargo.toml`, `crates/lattice-core/Cargo.toml`, `crates/lattice-agent/Cargo.toml`, `crates/lattice-tools/Cargo.toml`, `crates/lattice-sandbox/Cargo.toml`
- Limit: Multi-platform builds, minimal-feature builds, and slim plugin builds carry more risk. Internal crates often have empty default features, while `lattice-plugins` defaults to all features enabled.
- Scaling path: Split CI into `cargo test --workspace --all-targets`, minimal-feature builds, default plugin builds, Linux Landlock builds, and CLI/TUI smoke tests.

## Dependencies At Risk

**Two SQLite client stacks need clear ownership boundaries**

- Risk: The workspace uses both `rusqlite` and optional `sqlx`. `lattice-session` uses `rusqlite` for the CAS session store; `sqlx` is used for optional blob-store and memory-related features.
- Impact: If more persistence moves to async SQL, the project can end up with synchronous session storage, async blob storage, and runtime async turn execution all using different error and transaction models.
- Migration plan: Assign new persistence features explicitly. Keep session graph transactions on `rusqlite`; use `sqlx` for large blob/event objects. Avoid using both SQLite clients for the same data model.

**Landlock only covers Linux**

- Risk: The `landlock` dependency is only enabled on Linux targets. macOS/BSD path isolation relies on user-space sandbox checks plus resource limits.
- Impact: Cross-platform security promises cannot simply state that kernel isolation is enabled.
- Migration plan: Documentation and config UI should show platform capability and effective sandbox posture.

## Missing Critical Features

**No unified security posture report**

- Problem: CLI/TUI can switch sandbox mode, but users cannot easily see whether hook chain, Landlock, audit, allowlists, approval rules, and tool bundles are actually active for the current execution path.
- Files: `crates/lattice-cli/src/security.rs`, `crates/lattice-cli/src/frontend/tui/app.rs`, `crates/lattice-runtime/src/lib.rs`
- Blocks: Security review and user approval decisions are harder because the effective policy is not visible in one place.

**No automated architecture-boundary check**

- Problem: Docs declare the dependency hierarchy `hooks -> core -> session/prompt/sandbox/tools -> agent -> plugin -> orchestrator -> runtime -> cli`, but the code only shows local guards, such as `crates/lattice-orchestrator/src/agent_spec.rs` preventing imports from `lattice_runtime`.
- Files: `AGENTS.md`, `CLAUDE.md`, `crates/lattice-orchestrator/src/agent_spec.rs`, `Cargo.toml`
- Blocks: New crates or imports can break the declared hierarchy without CI catching it.

## Test Coverage Gaps

**Production executors need stronger end-to-end sandbox binding tests**

- Not tested: Every production CLI/run/code/TUI/pipeline path ends up using an executor bound to `RuntimeSecurity`, not the low-level `SandboxConfig::default()`.
- Files: `crates/lattice-cli/src/main.rs`, `crates/lattice-cli/src/runtime.rs`, `crates/lattice-runtime/src/preset.rs`, `crates/lattice-orchestrator/src/assembly.rs`, `crates/lattice-tools/src/executor.rs`
- Risk: A new entry point can bypass runtime security while still passing unit tests around individual tools.
- Priority: High

**Session manifest persistence needs clearer runtime + CLI regression tests**

- Not tested: After a real turn using the CLI default `SqliteCasSessionStore`, `sessions manifest list/show` always sees runtime-collected manifests. Legacy store fallback should also explicitly assert that manifests are lost.
- Files: `crates/lattice-runtime/src/turn.rs`, `crates/lattice-cli/src/session.rs`, `crates/lattice-cli/src/commands/sessions.rs`, `crates/lattice-session/src/lib.rs`
- Risk: Context audit chains can break while ordinary chat persistence still succeeds, making the issue easy to miss.
- Priority: High

**Plugin feature matrix lacks default/disabled combination tests**

- Not tested: `lattice-plugins` default-all, single-plugin, no-default-feature, and CLI pipeline registration combinations.
- Files: `crates/lattice-plugins/Cargo.toml`, `crates/lattice-plugins/src/lib.rs`, `crates/lattice-cli/src/runtime.rs`
- Risk: Feature-gated exports or registration changes can make plugins disappear or duplicate under some builds.
- Priority: Medium

**Documentation drift is not checked**

- Not tested: Whether README/AGENTS/CLAUDE crate lists, architecture hierarchy, and command examples match `Cargo.toml`.
- Files: `README.md`, `AGENTS.md`, `CLAUDE.md`, `Cargo.toml`
- Risk: Future planning agents can make design decisions from stale documentation.
- Priority: Medium

## Recent Planning Recommendations

1. First converge the security posture: align `SandboxConfig` default semantics, add production executor binding tests, and expose Landlock status.
2. Then make session manifest persistence reliable: explicit store capability, CLI manifest end-to-end coverage, and legacy fallback warnings.
3. Then split large files mechanically. Start with `lattice-session` and `lattice-tools`, and run the target crate tests after each split.
4. Add a documentation drift gate that generates or validates the README/AGENTS/CLAUDE crate lists from `Cargo.toml`, including `lattice-policy`.
5. Add plugin feature matrix CI so the all-default build does not hide minimal-build problems.

---

*Concerns audit: 2026-05-14*
