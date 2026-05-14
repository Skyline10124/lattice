# 开发环境搭建

## 依赖

- Rust 1.85+（workspace `Cargo.toml` 定义）
- Linux: Landlock sandbox 支持（kernel ≥ 5.13）

## 构建

```bash
# 全量构建
cargo build

# Release 构建
cargo build --release

# 单独 crate
cargo build -p lattice-core
cargo build -p lattice-agent
```

## 测试

```bash
# 全量测试
cargo test

# 特定 crate 测试
cargo test -p lattice-core
cargo test -p lattice-agent

# 特定测试函数
cargo test -p lattice-core test_streaming

# 基准测试
cargo bench -p lattice-core

```

## Lint

```bash
cargo clippy -- -D warnings
cargo fmt --check --all
cargo fmt --all  # 修复格式
```

## 环境变量

开发时需要设置至少一个 LLM provider 的 API key：

```bash
export ANTHROPIC_API_KEY="sk-ant-..."  # Anthropic/Claude
export OPENAI_API_KEY="sk-..."          # OpenAI/GPT
export GEMINI_API_KEY="..."             # Google Gemini
export DEEPSEEK_API_KEY="..."           # DeepSeek
```

没有 API key 时，resolve 会返回 `CredentialStatus::Missing`，非推理测试仍可通过。

## 项目结构

```
lattice/                    # Cargo workspace root
├── crates/
│   ├── lattice-core/          # 模型路由 + 流式推理 + 重试
│   ├── lattice-prompt/        # prompt 编译和 provider
│   ├── lattice-sandbox/       # 路径/命令/URL sandbox
│   ├── lattice-tools/         # 工具 schema + executor + capabilities
│   ├── lattice-agent/         # Agent 对话 + 内存 + hooks + audit
│   ├── lattice-plugin/        # Plugin trait + DAG
│   ├── lattice-orchestrator/  # Pipeline + EventBus + MicroAgent
│   ├── lattice-runtime/       # 顶层 runtime facade
│   ├── lattice-session/       # session graph + CAS store
│   ├── lattice-plugins/       # 官方插件
│   └── lattice-cli/           # CLI + TUI (ratatui)
├── wiki/                   # 项目 wiki 文档
└── CLAUDE.md               # AI 助手指南
```

## 常见问题

- **RwLock poisoned**: registry/session/memory 相关代码可能使用 `unwrap_or_else(|e| e.into_inner())` 恢复 poisoned lock；遇到 panic 相关问题时先确认是否为有意恢复路径。
- **Landlock 不支持**: 非 Linux 平台 sandbox 会 fallback 到 metacharacter + env override。
- **No reactor running**: VetHook 测试需要 `#[tokio::test]`，不能用 `#[test]`。
