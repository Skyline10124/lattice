# LATTICE

LATTICE 是用 Rust 编写、围绕会话组织的大模型运行时，提供命令行和终端交互界面。

> **警告**：项目处于快速迭代阶段，接口、配置和实现仍会有破坏性调整，请勿用于生产环境。
>
> 本项目主要由 Code Agents 驱动，个人开发者实力有限，使用前风险自斟。

## 当前能力

- **会话管理**：在全局会话库中保存树形对话与步骤记录，支持本地后台持续执行、重连观察、历史查看和退回任意节点后分叉继续。
- **多协议模型接入**：支持内置与自定义 provider 配置、API key 及登录凭据管理、推理档位选择，以及逐轮 token 用量与费用估算。
- **工具执行接缝**：协调层经可注入的 `ToolExecutor` 驱动调用槽位；当前版本生产环境默认注入空目录的 `NoTools`，会话以纯对话运行，不向模型暴露工具定义或任意 Shell 命令执行能力。

## 安装

目前仅支持 Linux 主机。Rust 工具链版本由
[rust-toolchain.toml](rust-toolchain.toml) 指定。

拉取源码后，在 LATTICE 仓库根目录将前端 `lattice` 与后台 `lattice-daemon` 安装到同一目录（`cargo install` 默认写入同一个 `$CARGO_HOME/bin`）：

```sh
git clone https://github.com/Skyline10124/lattice.git
cd lattice
cargo install --locked --path crates/lattice-cli
cargo install --locked --path crates/lattice-daemon
```

确保 Cargo 的可执行文件安装目录已加入 `PATH`，然后检查入口：

```sh
lattice --help
lattice-daemon --help
```

## 快速开始

以下示例创建一个纯对话 Agent，使用模型引用 `anthropic/claude-sonnet-4-6`。

### 1. 准备配置

进入项目根目录，创建配置目录：

```sh
mkdir -p .lattice/agents/reader
```

将以下内容保存为 `.lattice/agents/reader/agent.toml`。

> 文件已经存在时，先检查现有配置，不要直接覆盖。

```toml
schema_version = 1

[agent]
name = "reader"
model = "anthropic/claude-sonnet-4-6"

[prompt]
system = "请用中文回答，表达简洁准确。"
```

配置目录名称与 `[agent].name` 保持一致。
后台在新会话首次提交或显式选择 agent 时读取 profile，先查项目内的 `.lattice/agents`，再查后台启动环境中 `$HOME/.lattice/agents`。全局目录与其中的 profile 须属于当前用户、不可被同组或其他用户写入（默认的 0755/0644 即可），路径最后一段不能是符号链接，否则提交时按配置错误拒绝。连接同一后台的前端共用这一全局目录；固定 `XDG_STATE_HOME` 时，更换前端 HOME 不会改变它。接受事务会保存 profile 的有效副本和提示引用；后续未显式选择 agent 时使用副本，即使源文件修改或删除也不受影响。退回历史节点或根节点不会改变副本；显式选择同名 agent 可重新加载源配置。
当前版本生产环境默认以纯对话运行，不向模型暴露工具。`agent.toml` 仅允许 `schema_version`、`[agent]`、`[prompt]` 与可选 `[llm]`；出现已移除的 `[lancet]` 或 `[tools]` 时，提交按配置错误拒绝（`bad_request`，CLI 退出 `2`，诊断指出 `forbidden top-level section` 与文件路径），删除对应节后重试（已有会话需更新副本时，提交显式带上 `--agent <name>`）。

### 2. 启动后台并配置模型凭据

首次使用前执行 `lattice daemon install`，写出用户级 systemd 服务单元（`${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/lattice.service`）并立即启用与启动后台：

```sh
lattice daemon install
```

日常通过 `systemctl --user start|stop|restart lattice` 或 `lattice daemon stop` 管理后台。需要后台在当前用户没有任何登录会话时也保持运行的，执行：

```sh
loginctl enable-linger
```

没有 systemd user session 的环境（如容器或精简环境）中，改为在独立终端或外部监督器里手动运行 `lattice-daemon`：

```sh
lattice-daemon --state-root "${XDG_STATE_HOME:-$HOME/.local/state}"
```

通过终端录入服务商密钥（输入不回显），后台保存到用户级 `~/.lattice/auth.json`。配置目录 `~/.lattice` 属于当前用户，新建时为 0700，不能向同组或其他用户开放；`auth.json` 为 0600。权限更宽、属主不符或是符号链接时，`lattice auth set` 与需要 key 的提交会被拒绝，后台不自动修改权限。

```sh
lattice auth set anthropic
lattice models --provider anthropic
```

provider 来自内置表与用户级 `~/.lattice/providers.toml`；环境变量、工作区 `.env` 和项目中的 `providers.toml` 不提供凭据或连接配置。

后台在 `~/.lattice/cache/models.json` 新于 24 小时时不重复拉取；缓存缺失或更旧时在启动时拉取，成功后每 24 小时再拉取。拉取失败从 1 分钟起倍增重试，上限 1 小时。在 `~/.lattice/providers.toml` 中写入以下设置可关闭自动刷新，修改后无需重启；文件无效时该次自动刷新算作失败，按 1 分钟起倍增、上限 1 小时的退避重试，修正文件后在下一次重试时生效。手动执行 `lattice models refresh` 仍会刷新。

```toml
[metadata]
auto_refresh = false
```

> 密钥不要写入提交到仓库的文件。对话需要调用模型服务。

### 3. 启动终端界面

保持在目标项目根目录，进入 TUI：

```sh
lattice tui --agent reader
```

TUI 的 [statusline](docs/architecture/tui/layout.md#statusline) 显示本轮用量、缓存命中率和费用估算，并单独显示上下文用量。

TUI 在首次提交被接受时创建 Session，启动界面本身不创建。打开已有会话时使用会话 ID：

```sh
SESSION_ID="替换为已保存的会话 ID"
lattice tui "$SESSION_ID"
```

进入界面后，可以输入：

```
----PROMPT----------------------------
> 概括这个项目的用途，并指出主要入口文件和目录。
```

## 后台执行与重连

Session 命令、TUI、`auth` 与 `models` 只连接已经运行的本地后台（`lattice-daemon`），不会自动拉起后台。请求被接受后，即使关闭终端，任务也会在后台继续执行。

`lattice-daemon` 使用启动它的宿主进程环境（systemd user manager 环境或手动启动时的进程环境），在启动时确定配置目录的位置（默认 `$HOME/.lattice`，其中有 `providers.toml`、凭据与全局 agents 目录），并载入 TLS 根证书。目录位置与 TLS 根在运行期间保持不变。换了决定目录位置或 TLS 根的环境后，执行 `systemctl --user restart lattice`（手动运行时先执行 `lattice daemon stop` 再重新启动 `lattice-daemon`）。

配置目录里的 `providers.toml` 与凭据在每次模型请求时重读，其中用于请求发送的配置与凭据修改从下一次请求起生效。凭据也可通过 `lattice auth set` 更新。价格的生效时机见[费用估算](docs/architecture/llm/usage.md#费用估算)。TLS 根只在后台启动时载入；修改 TLS 信任后需要重启后台。

TUI 中，输入框为空时按 Ctrl+D 或输入 `/quit` 只退出界面；任务运行时按 Esc 或 Ctrl+C 请求取消。之后用 `lattice tui <session_id>` 重新打开会话，在界面内输入 `/reconnect` 重新连接。

命令行提交被接受后会输出会话 ID 与本次任务（Turn）的 ID。用它们查询、继续观察或取消任务，或查看、停止后台：

```text
lattice session turn <turn_id>
lattice session turn <turn_id> --follow
lattice session cancel <session_id> <turn_id>
lattice daemon install
lattice daemon status
lattice daemon stop
```

接受回执未知时的找回与退出码见[判读结果](docs/development/validation-guide.md#判读结果)；取消与崩溃遗留任务的处理见[取消与执行者消失后继续](docs/development/scenarios/manual/cancel-and-continue.md)。

## 命令行入口

首次发送不带 `--session`，必须用 `--agent` 指定 Agent：

```sh
lattice session send "概括项目用途。" --agent reader
```

后续发送改用 `--session <session_id>`。顶层 `lattice run` 与 `session send` 相同。

`--model <provider>/<model>` 与 `--effort` 可覆盖本次提交的模型和推理档位；模型名可含后续 `/`。先用 `lattice models` 查看候选及其 `EFFORTS` 列，再选择模型支持的档位，例如：

```sh
lattice run "概括项目用途。" --agent reader \
  --model anthropic/claude-sonnet-4-6 --effort high
SESSION_ID="替换为提交回执中的会话 ID"
lattice run "继续检查测试入口。" --session "$SESSION_ID"
```

两项都省略时继承会话副本；`--effort off` 表示明确关闭推理，需要模型支持。不选档位时产品不发档位参数，由 provider 用默认推理强度。`gpt-6-luna` 这类模型在自定义的 Chat Completions provider 上带工具时，这个默认值会被拒绝（HTTP 400），要显式选 `--effort off`，或在 profile 里写 `effort = "off"`。档位名只有 `off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`。新会话的 profile 未写模型时可由 `--model` 补足。接受后的选择随会话保存，拒绝不改变副本，也不回写源 profile。已有会话再次显式指定 `--agent reader` 会重新读取配置并重建副本，同名 agent 也一样；选择规则及拒绝示例见[提交与找回](docs/development/scenarios/manual/submit-and-recover.md)。

`lattice session rewind <session_id> <节点序号>` 把当前位置移到会话树中的某个节点，之后的提交从那里分叉继续；原分支保留，文件不回退。节点序号用 `lattice session log <session_id>` 查看，用法见[会话退回与分叉](docs/development/scenarios/manual/rewind.md)。

当前路径的逐轮用量与费用估算可直接查询，字段与输出说明见[查询当前路径的用量](docs/development/scenarios/manual/observe-and-usage.md)：

```text
lattice session usage <session_id>
lattice --json session usage <session_id>
```

由 systemd 托管时，升级先执行 `systemctl --user stop lattice`（或 `lattice daemon stop`），覆盖同目录下的 `lattice` 与 `lattice-daemon` 后执行 `systemctl --user start lattice`。从自动拉起后台的旧版本升级时，若执行 `lattice daemon install` 报告旧后台仍占用状态根，先执行 `lattice daemon stop`（协议不兼容时用旧二进制执行），再执行 `systemctl --user start lattice`。协议兼容、输出字段与旧状态处理见[升级与输出字段适配](docs/development/scenarios/manual/daemon-and-upgrade.md#升级与输出字段适配)。

具体参数以各命令帮助为准：

```sh
# 会话管理
lattice session --help

# 查看已配置凭据的模型
lattice models --auth

# 立即刷新模型元数据
lattice models refresh
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
| 工具执行接缝 | [Step 执行与结果保存](docs/architecture/session/steps.md#调用槽位)与 [ADR 0009](docs/adr/0009-remove-legacy-lancet-tool-executor.md) |
| 开发测试与验证方式 | [测试工程合同](docs/development/testing.md)与[验证操作指南](docs/development/validation-guide.md) |

## 许可证

本项目采用 [Apache-2.0](LICENSE) 许可证。
