# Testing

Run the full Runtime suite:

```sh
cargo test
cargo clippy -- -D warnings
cargo fmt --all --check
```

Useful focused checks:

```sh
cargo check -p lattice
cargo test -p lattice
```

## Expectations

- Runtime must compile without Swarm or Plugins.
- `lattice-plugin` tests use local test plugins, not official plugin implementations.
- Security behavior should be tested on Rust execution paths.
- Profile loading should report failed loads instead of silently skipping them.
