# 安装

## Rust

artemis 是 Cargo workspace，直接 `cargo build`：

```bash
git clone https://github.com/Skyline10124/artemis
cd artemis
cargo build --release
```

依赖全在 Cargo.toml 里，不需要系统级安装。

## Python

```bash
cd artemis-python
pip install maturin
maturin develop
```

当前 Python 包只暴露 resolver。Chat 和 streaming 走 Rust。

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
