# Security Policy

## Supported Versions

LATTICE is currently in **alpha / dogfooding** stage. Security patches are applied to the latest commit on the `main` branch only. There are no LTS or stable releases yet.

| Version | Supported          |
| ------- | ------------------ |
| main    | :white_check_mark: |
| < 0.1   | :x:                |

## Reporting a Vulnerability

API keys handled by LATTICE grant access to paid LLM provider accounts — a credential leak is a financial and security incident.

If you discover a security vulnerability:

**Do not open a public GitHub issue.**

Instead, report via email:

- **Primary**: [security@lattice.rs](mailto:security@lattice.rs)
- **PGP key**: available at `https://lattice.rs/.well-known/pgp-key.txt` (coming soon)

You can expect:

1. **Acknowledgement** within 48 hours
2. **Assessment and fix timeline** within 5 business days
3. **Disclosure** after a fix is released (typically coordinated disclosure, 30–90 days)

We believe in safe harbour for security researchers acting in good faith.

## Security Architecture

### Credential Safety

- All API keys are injected via **environment variables only** — no config files, no checked-in secrets
- `ResolvedModel` debug output redacts keys: `api_key: "***"`
- Error messages never include credential data
- Credentials cached in process-local memory (`Mutex<HashMap>`) only
- Programmatic credential override via `with_credentials(HashMap)` — for embedding scenarios, never for config

### HTTPS Enforcement

- Non-localhost HTTP `base_url` values are **rejected at the engine level**
- `localhost` / `127.0.0.1` / `::1` are allowed over HTTP for local proxies
- Shared `reqwest::Client` uses system TLS

### Tool Execution Boundary

- Tool results capped at 1 MB by default (configurable)
- `SandboxConfig` controls tool execution:
  - Filesystem read/write path allowlists
  - Command execution allowlists
  - Network access controls (per-domain allow/deny)
- 7 built-in tools: `read_file`, `grep`, `write_file`, `list_directory`, `bash`, `patch`, `web_search`
- Plugin system is the only extensible code path — `Plugin` trait methods (`to_prompt`, `from_output`) are typed, not arbitrary code execution

### Trust Boundaries

| Layer        | Holds Credentials | Extensible       |
| ------------ | ----------------- | ---------------- |
| lattice-core | Yes               | No (via trait)   |
| lattice-agent| No                | No               |
| lattice-plugin| No               | Yes (Plugin trait)|
| lattice-harness| No              | Yes (TOML profiles)|

### Pipeline Security

- Agent profiles are TOML-defined — no live code execution from profiles
- JSON schema validation with retry loop prevents malformed LLM output from corrupting state
- Pipeline execution is deterministic and auditable

## Dependency Management

CI runs `cargo clippy` with warnings-as-errors. Dependency auditing (`cargo audit`, `cargo deny`) is planned but not yet enforced in CI. If you identify a vulnerable dependency, please report it via the channels above.

## Responsible Disclosure

We ask that you:

- Give us reasonable time to fix before public disclosure
- Avoid accessing or modifying user data beyond what is necessary to demonstrate the vulnerability
- Delete any credential data incidentally collected during research

## Recognition

We maintain a security acknowledgements page at `https://lattice.rs/security/hall-of-fame` (coming soon). With your permission, we will credit you there upon disclosure.

---

See [wiki/architecture/security.md](wiki/architecture/security.md) for the internal security architecture reference.
