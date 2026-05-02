# 测试

## 运行

```bash
cargo test                           # 全量
cargo test -p lattice-core           # 单 crate
cargo test -p lattice-core <name>    # 单测试
```

## 测试分布

总测试数 ~150+，覆盖 8 个 crate。

| Crate | 测试数 |
|-------|--------|
| lattice-core | ~30（resolve + streaming + transport + provider） |
| lattice-agent | ~33（Agent loop + sandbox + tool_executor + state） |
| lattice-plugin | ~15（PluginRunner + Behavior + Registry + builtin plugins） |
| lattice-harness | ~85（Pipeline + runner + profile + handoff + memory + dag_runner + tools） |
| lattice-cli | ~4 |
| lattice-tui | 0 |
| lattice-python | 4（需要 Python 运行时，CI 中 ignored） |

注：`lattice-memory` 和 `lattice-token-pool` 已合并到 `lattice-agent` 和 `lattice-harness`。

## 不用 Python 运行

core + agent + plugin + harness + cli 测试全部不需要 Python 运行时。

```bash
cargo test -p lattice-core -p lattice-agent -p lattice-plugin -p lattice-harness -p lattice-cli
```
