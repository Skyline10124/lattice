# 测试

## 运行

```bash
cargo test                           # 全量
cargo test -p lattice-core           # 单 crate
cargo test -p lattice-core <name>    # 单测试
```

## 测试分布

总测试数 ~440+，覆盖 9 个 crate。

| Crate | 测试数 |
|-------|--------|
| lattice-core | 414（lib: 173 + e2e: 170 + transport_char: 29 + transport_integration: 42） |
| lattice-agent | 0 |
| lattice-memory | 3 |
| lattice-token-pool | 3 |
| lattice-plugin | 6 |
| lattice-harness | 12 |
| lattice-cli | 0 |
| lattice-tui | 0 |
| lattice-python | 4（需要 Python 运行时，CI 中 ignored） |

## 不用 Python 运行

core + agent + memory + token-pool + plugin + harness 测试全部不需要 Python 运行时。只有 lattice-python 的 PyErr roundtrip 测试需要。

```bash
# 跳过 python crate
cargo test -p lattice-core -p lattice-agent -p lattice-memory -p lattice-token-pool -p lattice-plugin -p lattice-harness
```
