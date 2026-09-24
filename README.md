# LATTICE

LATTICE 是用 Rust 编写、围绕会话组织的大模型运行时，提供命令行和终端交互界面。
会话历史使用 Git 存储，模型通过 LANCET 调用结构化工具，在宿主策略控制下读取和修改本地项目。

> **警告**：项目处于快速迭代阶段，接口、配置和实现仍会有破坏性调整，请勿用于生产环境。
>
> 本项目主要由 Code Agents 驱动，个人开发者实力有限，使用前风险自斟。

## 当前能力

- **会话管理**：保存对话与工具调用记录，支持会话重开、历史查看和分支操作。
- **项目操作**：通过结构化工具读取、搜索、创建和编辑文件，也可按配置启用网页获取和待办维护。
- **调用审计**：记录工具调用的执行状态与证据，提供独立的操作员查询入口。

工具在 WASM Component 中执行，权限由宿主控制。运行中的工具目录固定，
不支持动态加载插件，也不向模型提供任意 Shell 命令执行能力。

## 安装

目前仅支持 Linux 主机。Rust 工具链版本由
[rust-toolchain.toml](rust-toolchain.toml) 指定。

拉取源码后，在 LATTICE 仓库根目录安装：

```sh
git clone https://github.com/Skyline10124/lattice.git
cd lattice
cargo install --locked --path crates/lattice-cli
```

确保 Cargo 的可执行文件安装目录已加入 `PATH`，然后检查入口：

```sh
lattice --help
```

## 快速开始

以下示例创建一个只允许读取项目文件的 Agent，使用 Anthropic 的 `sonnet` 模型别名。

### 1. 准备配置

进入希望读取的项目根目录，创建配置目录：

```sh
mkdir -p .lattice/agents/reader
```

将以下内容保存为 `.lattice/agents/reader/agent.toml`。

> 文件已经存在时，先检查现有配置，不要直接覆盖。

```toml
schema_version = 1

[agent]
name = "reader"
model = "sonnet"

[prompt]
system = "请用中文回答。先读取相关文件，再根据实际内容说明项目结构与实现。"

[tools]
enabled = ["observe.workspace"]
```

配置目录名称与 `[agent].name` 保持一致。
这里仅启用工作区读取工具，不授予创建、编辑文件或抓取网页的工具权限。

### 2. 配置模型凭据

在当前终端设置服务商密钥：

```sh
export ANTHROPIC_API_KEY="替换为你的密钥"
```

> 密钥不要写入提交到仓库的文件。只读工具配置不代表离线运行，
> 对话仍需要调用模型服务。

### 3. 启动终端界面

保持在目标项目根目录，创建会话并进入 TUI：

```sh
lattice tui --agent reader --project-branch main
```

将 `main` 替换为项目的目标分支名。首次创建 TUI 会话需要提供
`--project-branch`；打开已有会话时使用会话 ID：

```sh
lattice tui <session_id>
```

进入界面后，可以输入：

```
----PROMPT----------------------------
> 概括这个项目的用途，并指出主要入口文件和目录。
```

## 后台执行与重连

当前工作分支提供本地后台执行与重连入口，相关[合同修订待批准](docs/README.md#工作分支修订状态)。Session 命令与 TUI 首次使用时按需启动同用户、同状态根的后台。一次已接受任务称为 Turn；接受后关闭终端，后台继续执行。

提交时记下 SessionId、TurnId、原输入和工作区，重连用这些身份找回任务。TUI 中空缓冲的 Ctrl+D 或 `/quit` 只退出；运行时 Esc/Ctrl+C 请求取消。重新打开 `lattice tui <session_id>` 读取已保存对话，界面内 `/reconnect` 重新连接。

按保存的身份选择操作：

```text
lattice session turn <session_id> <turn_id>
lattice session turn <session_id> <turn_id> --follow
lattice session cancel <session_id> <turn_id>
lattice daemon status
lattice daemon stop
```

`<session_id>` 与 `<turn_id>` 替换为保存的身份。查询得到 `pending` 时，任务尚未关闭；后台仍运行可继续观察。后台崩溃遗留的任务，会在该会话下一次发送新消息，或执行 checkout、branch、reset、set-agent、rewind 之前自动关闭：关闭只依据已保存的进度，不再请求模型、不重跑工具；随后正常执行这次操作。观察缺口会提示并重新读取已保存对话，缺失的旧流式文字不逐 token 补播。

结果可读但接受或完成确认未知时，保留原 TurnId，按[操作指南](docs/development/validation-guide.md#后台与重连操作)查询或显式重试原请求。指南也说明取消、后台停止、退出码及当前重连限制；这些入口尚不代表本工作分支已通过产品验收。

## 命令行入口

不使用 TUI 时，可以通过 `session` 子命令创建会话、发送消息和查看历史。
具体参数以各命令的帮助为准：

```sh
# 会话管理
lattice session --help

# 查看已配置凭据的模型
lattice models --auth

# 工具调用审计
lattice audit --help

# LANCET 自检
lattice lancet doctor
```

## 开发与贡献

任务、问题与实现讨论在 GitHub Issues 和 PR 中进行。
提交贡献或使用编码 Agent 前，请阅读 [AGENTS.md](AGENTS.md)。

## 文档导航

| 内容 | 入口 |
| --- | --- |
| 文档索引 | [docs/README.md](docs/README.md) |
| Agent 工作指引 | [AGENTS.md](AGENTS.md) |
| 领域术语 | [docs/reference/glossary.md](docs/reference/glossary.md) |
| LANCET 工具执行 | [架构合同](docs/architecture/lancet.md)与[工具开发指南](docs/development/tool-development.md) |
| 开发测试与验证方式 | [测试工程合同](docs/development/testing.md)与[验证操作指南](docs/development/validation-guide.md) |

## 许可证

本项目采用 [Apache-2.0](LICENSE) 许可证。
