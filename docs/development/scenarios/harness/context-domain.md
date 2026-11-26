---
status: active
---

# 模型上下文域与多渠道前端操作

| 项 | 内容 |
| --- | --- |
| 目标 | 验证 CLI `session feed`、`session inspect`、`session context-op`、`session trigger`、`--channel` 与 `--expected-position` 并发保护、全部 7 种 `ContextEdit`（含 Anthropic 签名推理剥离、无 `rewind` 图片模态与 32 MiB 预算恢复，以及 `fake-tools` 下的多工具调用编辑），以及 TUI PTY 下的外部 `feed`/`context-op` 渲染与自包含双通道 `[tui/comment, tui]` 提交 |
| 前提 | 准备小 PNG/JPEG 与 3 MiB PNG 样本文件；Anthropic 推理剥离用例使用 `AnthropicMessages` 自定义 provider 并通过 `NativeEvents` 返回带 `signature_delta` 的 `thinking` 块；图片恢复用例在 `providers.toml` 中同时声明 `input = ["text", "image"]` 与 `input = ["text"]` 两个模型；工具调用编辑用例启用 `fake-tools` feature 与 `LATTICE_TEST_TOOLS="read_note,read_second"` |
| 相关测试 | `direct_context_domain`：`context_domain_feed_inspect_edits_trigger_and_expected_position`、`context_op_strip_reasoning_clears_anthropic_signature`、`context_op_omit_and_replace_image_recovers_text_only_model_and_32mib_budget`、`context_op_omit_invocation_and_replace_tool_result`（`fake-tools`）；`pty_tui_smoke`：`pty_tui_renders_external_feed_context_op_and_submits_self_contained_dual_channel_comment`；`fast_cli_help`：`fast_context_domain_cli_validation` |

## 操作

1. 在 Chat Completions 会话中带 `--channel cli/seed` 与 `--image` 发送首轮，随后用 `session feed` 追加 `cli/note`（含图）与 `cli/extra`（纯文本），经文本与 `--json` 执行 `session inspect`，再在单次 `session context-op` 中组合 `--omit-node`、`--replace-input`（含 `--image`）与 `--replace-step`，随后执行 `session trigger`；对 `send`、`feed`、`context-op`、`trigger` 分别传入过期 `--expected-position 0`，再用当前位点配合 `--model` 执行 `session trigger` 并查看 `session log`。
2. 在 Anthropic Messages 会话中完成带签名 `thinking` 的首轮并经对照续接验证同来源签名回放，随后执行 `session context-op --strip-reasoning <step_node>` 与 `session trigger`。
3. 在声明了多模态与纯文本两款模型的会话中，先带两张图片完成首轮并尝试切纯文本模型触发拒收，随后不执行 `rewind`，用 `--omit-image <node> <media>:<len>:<sha256>` 与 `--replace-image <node> @<PATH> <text>` 清理图片并执行 `session trigger`；另起会话连续两轮发送三张 3 MiB PNG 后用第三轮触发 32 MiB 本地请求体预算拒收，不执行 `rewind`，经 `context-op` 移出/替换前两轮大图后再重发第三轮。
4. 在 `fake-tools` 构建下经 `read_note`、`read_second` 完成双工具调用首轮，经 `session turn --json` 取得两个 `InvocationId`，执行 `session context-op --omit-invocation <id1> --replace-tool-result <id2> <text>` 后执行 `session trigger`。
5. 在真实 PTY 中启动 TUI 并完成首轮后，由外部 CLI 执行 `session feed --channel cli` 与 `session context-op --omit-node`，再在 TUI 内向上浏览到已关闭助手回复创建自包含评论草稿，回到输入区连同普通文本提交。

## 断言

- `session feed` 不触发模型请求，`session inspect` 准确呈现渠道计数、已操作节点状态与可复制的 `<media>:<len>:<sha256>` 图片规格；组合 `context-op` 后的 `session trigger` 请求体精确体现多渠道 `<lattice-context channel="...">` 与归约内容；四个写命令在 `--expected-position` 过期时均以退出码 `2` 与 `conflict`（`position_changed`）拒绝且不请求模型；匹配位点触发成功且 `session log` 展示 `input`、`context_op`、`step`。
- `--strip-reasoning` 前同来源续接包含原 `thinking` 与 `signature`，剥离后 `inspect` 的 `reasoning_bytes` 归零且后续 `trigger` 请求体保留文本并彻底清除 `thinking` 块与回放签名。
- 纯文本模型与 32 MiB 预算拒收消息均包含 `lattice session context-op` 恢复指引；经 `--omit-image` / `--replace-image` 清理后无需 `rewind` 即可在保留前轮文字历史的前提下完成纯文本模型 `trigger` 或重发第三轮大图。
- `fake-tools` 下 `--omit-invocation` 与 `--replace-tool-result` 使 `inspect` 的有效 `tool_call_count` 从 2 降为 1，后续 `trigger` 请求体中 `call_1` 的调用与结果成对消失，`call_2` 保留原 `tool_call_id` 且载荷替换为规范 JSON 字符串。
- PTY TUI 实时呈现 `USER · cli` 与 `CONTEXT · cli` 摘要；自包含评论与普通文本以 `[tui/comment, tui]` 双通道原子送达假模型且不含旧外键属性，画面原地渲染 `USER · tui/comment` 卡片且不出现 `[COMMENT n]` 或 `原文不匹配`。
