# artemis wiki

模型为中心的 LLM 推理引擎。Rust 内核，Python 绑定。

## 快速导航

| 想做什么 | 看这里 |
|----------|--------|
| 快速开始 | [getting-started/quickstart](getting-started/quickstart.md) |
| 了解架构 | [architecture/overview](architecture/overview.md) |
| 添加模型 | [reference/catalog](reference/catalog.md) |
| 开发指南 | [development/setup](development/setup.md) |
| 设计理念 | [design/why](design/why.md) |

## 项目状态

alpha / dogfooding。Rust 侧 resolve + chat + chat_complete 可用，Python 绑定仅 resolver。

## 目录

### 入门
- [快速开始](getting-started/quickstart.md)
- [安装](getting-started/installation.md)
- [第一个调用](getting-started/first-call.md)

### 架构
- [总览](architecture/overview.md)
- [模型解析](architecture/model-resolution.md)
- [推理链路](architecture/inference-pipeline.md)
- [Crate 地图](architecture/crates.md)
- [安全边界](architecture/security.md)

### 开发
- [环境搭建](development/setup.md)
- [运行测试](development/testing.md)
- [代码审查历史](development/code-review.md)
- [路线图](development/roadmap.md)

### 参考
- [API 参考](reference/api.md)
- [Catalog 与 Provider](reference/catalog.md)
- [错误处理](reference/errors.md)
- [Streaming 协议](reference/streaming.md)
- [Provider 测试矩阵](reference/provider-matrix.md)

### 设计
- [为什么这样设计](design/why.md)
- [LLM 作为函数](design/llm-as-function.md)
- [插件系统设想](design/plugin-system.md)
- [Nix 范式](design/nix-paradigm.md)
- [竞品分析](design/competitors.md)

## 外部链接

- [GitHub](https://github.com/Skyline10124/artemis)
- [CLAUDE.md](../CLAUDE.md) — AI 助手指南
- [README](../README.md) — 项目首页
