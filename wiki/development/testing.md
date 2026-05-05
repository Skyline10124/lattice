# 测试

## 运行

```bash
cargo test                           # 全量
cargo test -p lattice-core           # 单 crate
cargo test -p lattice-core <name>    # 单测试
```

## 测试分布

总测试数 ~698，覆盖 6 个 crate。

| Crate | 测试数 |
|-------|--------|
| lattice-core | ~30（resolve + streaming + transport + provider） |
| lattice-agent | ~33（Agent loop + sandbox + tool_executor + state + memory） |
| lattice-plugin | ~100（PluginRunner + Behavior + Registry + builtin plugins + Pipeline + runner + dag_runner + handoff） |
| lattice-cli | ~29（CLI + TUI） |
| lattice-bus | ~5 |
| lattice-python | 4（需要 Python 运行时，CI 中 ignored） |

注：`lattice-memory`、`lattice-token-pool`、`lattice-harness` 已合并到 `lattice-agent` 和 `lattice-plugin`。无独立 `lattice-tui` crate（TUI 在 `lattice-cli` 内）。

## 不用 Python 运行

core + agent + plugin + cli + bus 测试全部不需要 Python 运行时。

```bash
cargo test -p lattice-core -p lattice-agent -p lattice-plugin -p lattice-cli -p lattice-bus
```
