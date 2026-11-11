---
status: active
---

# 会话设置与草稿

| 项 | 内容 |
| --- | --- |
| 目标 | 后台独占持久化会话设置、无默认模型草稿与项目绑定字节；`configure(New)` 提交后崩溃不产生重复会话或重复轮次；旧 `SettingsVersion`、同会话忙时改设置与跨项目发送均在写入前被拒绝，模型切换的档位调整在 TUI 中可见 |
| 前提 | 场景 HOME 下配置指向本地 fake provider 的 `providers.toml` 与凭据；草稿场景准备不含 `[agent].model` 的 agent；崩溃场景启用 `crash-barriers` feature 与带 `test-crash-barriers` 的产品二进制，设置 `configure_committed` 屏障；非 UTF-8 路径场景创建名字含 `0xff` 字节的项目目录；TUI 场景启用 `fake-tools` feature 与带门控的 fake provider，准备支持不同推理档位集合的两个模型 |
| 相关测试 | `direct_background`：`draft_without_model_survives_restart_and_enforces_settings_version`；`direct_crash_barriers`：`configure_committed_crash_leaves_single_draft_and_reports_unknown`；`fast_session_cli`：`non_utf8_project_path_round_trips_bytes_and_enforces_binding`；`pty_tui_smoke`：`pty_model_effort_selection_survives_retry_tools_and_reopening` |

## 操作

1. 对未写 `[agent].model` 的 agent 经原始 IPC 执行 `configure(New)`，停止并重启后台后查询 `session list` 与 `head`，直接提交一轮，再执行 `configure(Existing, model=M)` 后提交，随后分别携带旧 `SettingsVersion` 与新 `SettingsVersion` 各提交一次。
2. 在后台设置 `configure_committed` 崩溃屏障，用 CLI `--json session send --agent A` 创建新会话触发屏障并以 SIGKILL 中断后台；等待前端退出后重启后台，查询会话状态并在同一会话继续发送。
3. 在名字含 `0xff` 字节的项目目录中运行 `lattice session send --agent A`，分别查看 `session list` 文本与 `--json` 输出，再在同一目录与另一目录各自运行 `--session S` 发送。
4. 在 TUI 中启动一轮并用门控挂住流，执行中通过 `/model` 选定另一模型；待这一轮结束后，通过 `/model` 切换到不支持当前档位的模型。

## 断言

- 无默认模型的 `configure(New)` 生成模型、provider 与档位均为 `null` 的草稿会话；重启后台后 `session list` 仍能读到该会话（节点数为 1），`head` 保持一致。
- 未选择模型时提交以配置拒绝返回，提示先通过 `/model` 或 `--model` 选择模型，不写入对话节点，会话头与 `SettingsVersion` 不变；选定模型 M 后提交完成，`UserMessage` 记录模型 M。
- 携带过期 `SettingsVersion` 提交返回 `settings_changed` 冲突且不写节点；携带当前 `SettingsVersion` 提交被接受并完成。
- `configure_committed` 崩溃后，CLI 第一条 JSONL 为写明身份的 `submit(sending)`；因 `configure_once` 直接连接 socket 而不自动拉起后台，重连得到 `not_running`，后续依次输出 `configure(unknown)` 与 `submit(not_accepted)` 并以退出码 1 退出；重启后台后状态根中只有该草稿会话 S（节点数为 1），后续 `--session S` 发送正常完成，不出现第二个会话或重复轮次。
- 非 UTF-8 项目目录下创建的会话在 `session list` 文本输出中以替换字符显示路径，在 `--json` 输出中 `project_path` 与规范路径的原始字节数组逐字节一致；同目录续发成功，跨目录以 `--session S` 发送返回 `binding_mismatch` 冲突并以退出码 2 退出。
- TUI 在一轮执行中选择模型时，Dock 显示说明执行中不能修改设置的 `WARN`，后台 `head` 与 `SettingsVersion` 不变；该轮结束后切换到不支持当前档位的模型时，对话区出现说明档位调整的 `Info`，statusline 显示新的模型与档位。
