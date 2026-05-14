# LATTICE-Runtime Wiki

![LATTICE banner](https://github.com/Skyline10124/lattice/blob/main/logo-banner.svg)

This wiki documents the runtime repository: the Rust modules that execute model calls, run agents, expose plugin contracts and orchestrate pipelines.

## Quick Navigation

| Topic | Page |
| --- | --- |
| Runtime architecture | [architecture/overview](architecture/overview.md) |
| Crate responsibilities | [architecture/crate-map](architecture/crate-map.md) |
| Model resolution | [architecture/model-resolution](architecture/model-resolution.md) |
| Agent execution | [architecture/agent-runtime](architecture/agent-runtime.md) |
| Pipeline and bus | [architecture/pipeline-and-bus](architecture/pipeline-and-bus.md) |
| Security model | [architecture/security](architecture/security.md) |
| Plugin contract | [reference/plugin-contract](reference/plugin-contract.md) |
| Streaming events | [reference/streaming](reference/streaming.md) |
| Testing | [development/testing](development/testing.md) |

## Status

Runtime is the main execution repository. It includes the `lattice` Rust crate with core, agent, plugin, bus and runtime modules.

`lattice::runtime::Runtime` is the preferred Rust seam. It owns router, profile registry, plugin registry, tool registry, memory, events and pipeline construction, so Swarm and other Rust frontends do not need to wire lower modules manually.

Official plugins are not part of Runtime. They live in `LATTICE-Plugins` and depend on Runtime contracts.

## Invariants

- Runtime must not depend on Swarm or Plugins.
- `lattice-plugin` must define plugin contracts and loading mechanics, not official plugin implementations.
- Shared types used by multiple runtime modules belong in `lattice::core`.
- Security checks for model endpoints and tools must live on the Rust execution path.
