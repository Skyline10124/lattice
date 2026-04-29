# 测试

## 运行

```bash
cargo test                           # 全量
cargo test -p artemis-core           # 单 crate
cargo test -p artemis-core <name>    # 单测试
```

## 测试分布

| Crate | 测试数 |
|-------|--------|
| artemis-core | 411（lib: 170 + e2e: 170 + transport_char: 29 + transport_integration: 42） |
| artemis-agent | 0 |
| artemis-memory | 3 |
| artemis-token-pool | 3 |
| artemis-python | 0（Python binding 无 Rust 测试） |

## 不用 Python 运行

core + agent + memory + token-pool 测试全部不需要 Python 运行时。只有 artemis-python 的 PyErr roundtrip 测试需要。

```bash
# 跳过 python crate
cargo test -p artemis-core -p artemis-agent -p artemis-memory -p artemis-token-pool
```
