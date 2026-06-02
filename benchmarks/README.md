# Benchmarks

This directory contains LATTICE benchmarks. Benchmarks are separate from release
smoke tests: they measure how the runtime behaves on representative maintenance
work, not just whether a narrow contract passes.

Current suites:

- `lancet-dogfood/` — real-provider benchmark for the current LANCET catalog
  across smoke, mutation, and command scenarios. Legacy dogfood/live smoke
  scripts live under `lancet-dogfood/legacy/` for historical reproduction.
- `legacy-maintenance/` — real-project benchmark for reviewing, repairing, and
  hardening a stale upstream repository through a single session with multiple
  active-agent profiles.

Benchmark run artifacts must be written under `build/tmp/benchmarks/` so source
checkouts, JSONL streams, reports, and session databases do not enter the
repository.
