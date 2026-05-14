# Historical API Notes

These names appear in older documentation but are not current workspace crates.

## `lattice-bus`

The bus/pipeline layer was renamed or reorganized into `lattice-orchestrator`.

Current replacements:

- `lattice-orchestrator::Pipeline`
- `lattice-orchestrator::AgentRunner`
- `lattice-orchestrator::AgentProfile`
- `lattice-orchestrator::EventBus`
- `lattice-orchestrator::MicroAgent`
- `lattice-orchestrator::Orchestrator`

## `lattice-python`

There is no `lattice-python` crate in the current workspace. Python installation and PyO3 API notes are historical unless a Python binding crate is reintroduced.

## `lattice-registry`

There is no standalone `lattice-registry` crate. Agent profile registration and micro-agent discovery live in `lattice-orchestrator`.
