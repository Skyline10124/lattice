---
status: active
---

# 会话设置与草稿会话

| 项 | 内容 |
| --- | --- |
| 目标 | 后台独占持久化的会话设置、草稿会话与项目绑定字节；模型与档位选择经 `configure` 立即保存并跨后续提交与重启沿用；`configure_committed` 与先 `configure` 后 `turn_accepted` 崩溃重开状态一致；旧 `SettingsVersion`、同会话忙时改设置、不支持的档位与跨项目发送均在写入前被拒绝，模型切换的档位调整在 TUI 中可见 |
| 前提 | 显式启动自有后台后，在场景 `HOME` 下配置指向本地假 provider 的 `providers.toml` 与凭据（配置目录、配置快照刷新与根证书读取时点见[配置来源与运行时重读](configuration-sources.md)）；无模型草稿会话夹具准备既未写 `[agent].model` 也未写默认 `[agent].effort` 的 `agent`；多步工具与终端场景启用 `fake-tools` feature 与带门控的假 provider，准备支持不同推理档位集合的模型；崩溃场景启用 `crash-barriers` feature 与带 `lattice-server/test-crash-barriers` 的 `lattice-daemon` 二进制，分别以带 `configure_committed` 或 `turn_accepted` 屏障的环境显式启动或重启后台；非 UTF-8 路径场景创建名字含 `0xff` 字节的项目目录 |
| 相关测试 | `direct_background`：`draft_without_model_survives_restart_and_enforces_settings_version`、`public_choices_persist_and_model_changes_use_supported_fallbacks`、`cli_off_survives_tool_retry_restart_and_provider_failure`；`direct_crash_barriers`：`configure_committed_crash_leaves_single_draft_and_reports_unknown`、`accepted_session_settings_and_frozen_turn_reopen_before_receipt`；`fast_session_cli`：`non_utf8_project_path_round_trips_bytes_and_enforces_binding`；`pty_tui_smoke`：`pty_model_effort_selection_survives_retry_tools_and_reopening` |

## 操作

1. 对未写 `[agent].model` 且未设默认档位的 `agent` 经公开 Local IPC 执行 `configure(New)`，显式重启后台后查询 `session list` 与 `head`，直接提交一轮；再执行 `configure(Existing, model=M)` 后提交，随后分别携带旧 `SettingsVersion` 与新 `SettingsVersion` 各提交一次。
2. 用 `--model`、`--effort`（含 `--effort off`）配置并提交，之后不带选择继续提交（含工具重试、显式重启后台与提供方失败场景）；请求目标模型不支持的档位；切换到另一提供方或不同档位集合的模型。
3. 改写 `agent` 配置（换模型、档位与系统提示）及 `providers.toml` 中该模型的档位集合，以带 `turn_accepted` 崩溃屏障的环境显式重启后台；分别对新会话执行 `configure(New)`、对已完成一轮的会话执行 `configure(Existing)`，两者都显式给出该 `agent`；再经不读回执的连接提交一轮，接受后以 `SIGKILL` 中断后台。显式启动新后台后查询 `head` 与视图，按 `TurnId` 查询这一轮并以同一 `TurnId` 重发；删除配置目录后，向同一会话不带选择继续发送；另以带 `configure_committed` 崩溃屏障的环境显式重启后台，用 CLI `--json session send --agent A` 创建新会话触发 `SIGKILL`，等待前端退出并显式启动新后台后查询会话状态、在同一会话继续发送。
4. 在名字含 `0xff` 字节的项目目录中运行 `lattice session send --agent A`，分别查看 `session list` 文本与 `--json` 输出，再在同一目录与另一目录各自运行 `--session S` 发送。
5. 在 TUI 中用 `/model`、`/effort` 选定后提交并经历重试、工具及显式重启后台后重新打开会话；在一轮执行中通过 `/model` 选定另一模型；待该轮结束后切换到不支持当前档位的模型。

## 断言

- 该夹具的 agent 未写 `[agent].model` 且未设默认档位，`configure(New)` 生成模型、provider 与档位均为 `null` 的草稿会话（没有模型时档位也可以不为空，取决于 profile 或已有设置，见[档位规则](../../../architecture/session/settings.md#档位规则)）；重启后台后 `session list` 仍能读到该草稿会话（节点数为 1），`head` 保持一致。
- 未选择模型时提交以配置拒绝返回，提示先通过 `/model` 或 `--model` 选择模型，不写入对话节点，会话头与 `SettingsVersion` 不变；选定模型 M 后提交完成，`UserMessage` 记录模型 M。
- 携带过期 `SettingsVersion` 提交返回 `settings_changed` 冲突且不写节点；携带当前 `SettingsVersion` 提交被接受并完成。
- 模型与档位选择（含 `off`）经 `configure` 立即保存到会话设置，之后不带选择的提交、重试与重启均沿用；不支持的档位在 `configure` 时被拒绝，会话设置不变；换模型时按[档位规则](../../../architecture/session/settings.md#档位规则)调整档位，换到另一 provider 后会话设置记录新的 provider `id`。
- `turn_accepted` 崩溃并重启后，`head` 给出崩溃前 `configure` 保存的模型与档位，并带有 `settings_version`；视图中这一轮的 `UserMessage` 冻结了同一模型与档位，按 TurnId 查询得到该节点，以同一 TurnId 重发返回 `turn_id_used`。删除 profile 目录后，不带选择的发送照常完成，请求使用会话设置中保存的模型、档位与系统提示。新会话与已有会话两种情形结果相同。崩溃后未关闭的那一轮如何读出，见[后台崩溃与执行者消失](crash-and-vanished-executor.md)。
- `configure_committed` 崩溃后，CLI 第一条 JSONL 为写明身份的 `submit(sending)`；CLI 读不到回复，依次输出 `configure(unknown)`（错误种类为 `transport`）与 `submit(not_accepted)`，退出码 1；之后重启后台，状态根中只有该草稿会话 S（节点数为 1），后续 `--session S` 发送正常完成，不出现第二个会话或重复轮次。
- 非 UTF-8 项目目录下创建的会话在 `session list` 文本输出中以替换字符显示路径，在 `--json` 输出中 `project_path` 与规范路径的原始字节数组逐字节一致；同目录续发成功，跨目录以 `--session S` 发送返回 `binding_mismatch` 冲突并以退出码 2 退出。
- TUI 中 `/model`、`/effort` 选择经 `configure` 保存后在重试、工具与重新打开会话后保持；在一轮执行中选择模型时，Dock 显示说明执行中不能修改设置的 `WARN`，后台 `head` 与 `SettingsVersion` 不变；该轮结束后切换到不支持当前档位的模型时，对话记录出现说明档位调整的 `Info`，statusline 显示新的模型与档位。

## 相关局部验证

草稿会话的逾期清理属于后台内部并发与存储时机，由 [`server_configure_tests.rs`](../../../../crates/lattice-server/src/local/server_configure_tests.rs) 中的后台局部测试（非 harness 场景）验证：

- `cleanup_drafts_after_configure_new_respects_expiry_update_and_outcome`：期限判定、`updated_at` 刷新，以及 `configure(New)` 成功与被拒都会触发清理；
- `cleanup_drafts_skips_watched_draft_until_watch_disconnects`：有观察者时保护草稿会话，观察断开后才可清理；
- `cleanup_drafts_skips_when_session_slot_or_execution_lock_is_busy`：目标会话槽或执行锁正忙时跳过清理；
- `cleanup_drafts_and_watch_registration_serialize_in_both_orders`：`watch` 登记与清理按两种先后顺序串行化；
- `cleanup_drafts_keeps_session_accepted_between_scan_and_delete`：扫描之后接受了一轮的会话，经删除事务复核后保留。
