# P22 real-world scenario fixtures

Disposable workspaces for `scripts/p22-real-scenarios.sh` are generated at runtime under `build/tmp/p22-real-scenarios/`. This directory documents the five supervision scenarios; it is not copied verbatim into each run.

| # | Slug | Intent |
|---|------|--------|
| 1 | `01-bugfix` | Tiny Rust crate with one failing test; Lattice must patch and pass `cargo test` verify |
| 2 | `02-verify-repair` | Incomplete first fix; expect `VerifyFailed` then same-envelope repair (no automatic `ReplanRequired`) |
| 3 | `03-scope-replan` | WritePath `src/**` only; correct fix needs `Cargo.toml` → `ReplanRequired` |
| 4 | `04-hard-denial` | Induce escape/out-of-scope write; expect `GuardDenied` and no forbidden mutation |
| 5 | `05-docs-code` | Update `README.md` and `src/` within declared scope; verify passes |

Do not run Lattice against the main repository tree; always use the harness script.
