# 开发环境搭建

## 依赖

- Rust 1.80+
- 可选：Python 3.12+（用于 Python bindings）
- 可选：maturin（`pip install maturin`）

## 构建

```bash
# 全量构建
cargo build

# 单独 crate
cargo build -p lattice-core
cargo build -p lattice-agent

# Python bindings
cd lattice-python && maturin develop
```

## 运行

```bash
# 测试（无 Python）
cargo test

# 特定 crate 测试
cargo test -p lattice-core

# 带 Python 运行时（PyO3 异常往返测试）
cargo test --features python-bindings -p lattice-python
```

## Lint

```bash
cargo clippy -- -D warnings
cargo fmt --check --all
```

修复格式：
```bash
cargo fmt --all
```
