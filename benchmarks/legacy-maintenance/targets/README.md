# Target Records

Place frozen benchmark target records here after selecting a real upstream
project.

Recommended layout:

```text
targets/<slug>/
  target.md        # repository URL, pinned commit, license, selection reason
  issue.md         # copied issue/task text
  expected.md      # expected behavioral fix and verification notes
```

The runner consumes environment variables rather than parsing these files. The
files exist to make benchmark results auditable and reproducible.

Available frozen targets:

- `rslint-rust-189/` — `rslint/rslint` pinned at
  `7bc011e57c76ee114c440521a51907089584b10a`, using the open Rust 1.89
  compatibility task from `rslint/rslint#165`.
