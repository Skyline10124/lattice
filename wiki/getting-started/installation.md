# 安装

## Rust

LATTICE 是 Cargo workspace，直接 `cargo build`：

```bash
git clone https://github.com/Skyline10124/LATTICE
cd LATTICE
cargo build --release
```

依赖全在 Cargo.toml 里，不需要系统级安装。

## Python

当前 workspace 不包含 `lattice-python` crate。旧的 PyO3/maturin 安装说明只适用于历史版本。

## 凭证

所有 provider 通过环境变量认证。常用：

| Provider | 环境变量 |
|----------|---------|
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| DeepSeek | `DEEPSEEK_API_KEY` |
| MiniMax | `MINIMAX_API_KEY` |
| GitHub Copilot | `GITHUB_TOKEN` |
| OpenCode Go | `OPENCODE_GO_API_KEY` |
| OpenCode Zen | `OPENCODE_ZEN_API_KEY` |
| Google Gemini | `GEMINI_API_KEY` |
| 阿里云百炼 | `DASHSCOPE_API_KEY` |
| 月之暗面 | `MOONSHOT_API_KEY` |

完整列表见 [provider-matrix](../reference/provider-matrix.md)。
