# Release Notes

Runtime is versioned independently from Swarm and Plugins.

## Before Release

```sh
cargo test
cargo clippy -- -D warnings
cargo fmt --all --check
```

Check that downstream repositories still build:

```sh
cd ../LATTICE-Plugins && cargo check
cd ../LATTICE-Swarm && git submodule update --remote LATTICE-Runtime && cargo check
```

## Compatibility

Runtime changes that affect these surfaces require coordinated downstream updates:

- `lattice::core::types`
- `lattice::plugin::Plugin`
- `lattice::plugin::registry`
- `lattice::orchestrator::profile`
- `lattice::tools`
- `lattice::sandbox`
- `lattice::session`
- `lattice::runtime::Runtime`
