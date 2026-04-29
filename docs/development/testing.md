# 测试

## 运行

```bash
cargo test                           # 全量
cargo test -p artemis-core           # 单 crate
cargo test -p artemis-core <name>    # 单测试
```

## 测试分布

总测试数 ~440+，覆盖 9 个 crate。

| Crate | 测试数 |
|-------|--------|
| artemis-core | 414（lib: 173 + e2e: 170 + transport_char: 29 + transport_integration: 42） |
| artemis-agent | 0 |
| artemis-memory | 3 |
| artemis-token-pool | 3 |
| artemis-plugin | 6 |
| artemis-harness | 12 |
| artemis-cli | 0 |
| artemis-tui | 0 |
| artemis-python | 4（需要 Python 运行时，CI 中 ignored） |

## 不用 Python 运行

core + agent + memory + token-pool + plugin + harness 测试全部不需要 Python 运行时。只有 artemis-python 的 PyErr roundtrip 测试需要。

```bash
# 跳过 python crate
cargo test -p artemis-core -p artemis-agent -p artemis-memory -p artemis-token-pool -p artemis-plugin -p artemis-harness
```
