# Lattice Security Audit Notes

Date: 2026-05-02
Scope: full repository review with non-destructive blue-team / self-attack analysis
Method: code review, risky-pattern scan, trust-boundary review, and local PoC reasoning

## Executive summary

The most credible issues are concentrated in three areas:

1. Sandbox path validation relies on naive substring checks and lacks path canonicalization.
2. Process-wide environment variables are used as a credential/config transport in some flows, creating shared-state hazards.
3. Multiple production-path `unwrap` / `expect` uses can turn malformed state or poisoning into denial of service.
4. **NEW (2026-05-02 audit)**: command sandbox uses blacklist model with `sh -c` execution, creating multiple CRITICAL bypass paths via metacharacter omission (\n, &, >, <, ANSI-C quoting).
5. **NEW (2026-05-02 audit)**: URL validation uses string prefix matching instead of URL parsing, enabling multiple SSRF bypass vectors.
6. **NEW (2026-05-02 audit)**: Agent profile TOML files lack integrity verification, enabling pipeline-wide agent compromise.

## Findings index

### Sandbox & Tools (lattice-agent)
- VULN-001 sandbox command-filter bypass via newline/metacharacter family (CRITICAL)
- VULN-002 sandbox write-allowlist path matching is substring-based (HIGH)
- VULN-003 sandbox sensitive-file blocking is substring-based and non-canonical (MEDIUM)
- VULN-007 check_command() `&` background injection bypass (CRITICAL)
- VULN-008 check_url() URL parse confusion SSRF (CRITICAL)
- VULN-009 HTTPS-to-internal-network SSRF unrestricted (CRITICAL)
- VULN-010 bash command no timeout DoS (CRITICAL)
- VULN-011 check_command() `>` and `<` redirection bypass (HIGH)
- VULN-012 read_allowlist is dead code (HIGH)
- VULN-013 TOCTOU race condition in file operations (HIGH)
- VULN-014 web_search no request timeout (HIGH)
- VULN-015 sensitive files list severely incomplete (MEDIUM)
- VULN-016 DNS rebinding SSRF bypass (MEDIUM)
- VULN-017 reqwest redirect tracking SSRF (MEDIUM)
- VULN-018 Bash ANSI-C quoting bypass (MEDIUM)
- VULN-019 multi-vector combination bypass (MEDIUM)

### Core / Transport (lattice-core)
- VULN-020 SSRF via unchecked HTTP redirect following (HIGH)
- VULN-021 URL validation bypass via userinfo injection (MEDIUM)
- VULN-022 arbitrary HTTP header injection via header: prefix (MEDIUM)
- VULN-023 credential leak via error response body (MEDIUM)
- VULN-024 non-HTTP scheme accepted in URL validation (LOW)
- VULN-025 missing overall request timeout (LOW)
- VULN-026 chat_endpoint override enables URL redirection (LOW)
- VULN-027 unbounded model ID length in Gemini URL (LOW)

### Harness / Pipeline (lattice-harness)
- VULN-028 agent profile no integrity verification (CRITICAL)
- VULN-029 unbounded fork target thread bombing (HIGH)
- VULN-030 FTS5 query construction fragile (LOW)
- VULN-031 WebSocket endpoint no auth/origin validation (MEDIUM)
- VULN-032 system.file symlink traversal to LLM (MEDIUM)
- VULN-033 LATTICE_AGENTS_DIR env poisoning (MEDIUM)
- VULN-035 unlimited memory entry storage (LOW)
- VULN-036 deeply nested JSON comparison stack overflow (LOW)

### CLI / Credentials (lattice-cli)
- VULN-004 process-wide environment mutation cross-request contamination (HIGH)
- VULN-034 diagnostics() credential enumeration (LOW)

### Bindings (lattice-python)
- VULN-005 Python binding lock poisoning can panic host (MEDIUM)
- VULN-006 schema-retry JSON fallback hides malformed output (LOW)

## Severity overview

| Severity | Count | IDs |
|----------|-------|-----|
| CRITICAL | 7 | VULN-001, 007, 008, 009, 010, 028, ... |
| HIGH | 9 | VULN-002, 004, 011, 012, 013, 014, 020, 029, ... |
| MEDIUM | 12 | VULN-003, 005, 015, 016, 017, 018, 019, 021, 022, 023, 031, 032, 033 |
| LOW | 8 | VULN-006, 024, 025, 026, 027, 030, 034, 035, 036 |
| **TOTAL** | **36** | |

## Blue-team note

All reproduction guidance is non-destructive and intended for local verification only.
No exploit code for persistence, data theft, or destructive actions is included.
