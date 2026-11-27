---
status: active
---

# 用户图片输入

| 项 | 内容 |
| --- | --- |
| 目标 | CLI `--image` 经 IPC 69 提交保序图文，五种协议按[协议适配合同](../../../architecture/llm/protocols.md#适配器的职责)投影经 `<lattice-context channel="cli">` 包裹的含图请求并在后台重启后从会话库重放历史图片；本地预检拒绝不留草稿会话；跨轮累计超过 32 MiB 在接受前拒绝、`rewind` 后恢复；远端 `HTTP 413` 不重试并给出恢复提示 |
| 前提 | 准备两张固定魔数小图（PNG 魔数 8 字节写入错扩展名文件 `shot.jpg`、JPEG 魔数 4 字节写入 `photo.jpeg`）、一张 3 MiB（`3_145_728` 字节）PNG 大图 `big.png` 与一个非图片文件 `bad.png`；五种协议使用自定义 provider 接入（无 `models.dev` 元数据，不需要手写 `input`）；413 用例在 `agent.toml` 开启 `[llm.retry]`（`max_retries = 2`），假模型脚本先返回带 `x-should-retry: true` 与错误 `message` 的 `413` JSON 响应再附带未达文本回复 |
| 相关测试 | `direct_user_images`：`user_images_replay_after_daemon_restart`、`user_image_budget_recovers_after_rewind`、`user_image_http_413_is_not_retried` |

## 操作

1. 对五种协议（`chat-completions`、`responses`、`interactions`、`anthropic-messages`、`generate-content`）分别在独立世界中带 `--image shot.jpg --image photo.jpeg` 发送首轮图文消息，随后删除两张本地图片并重启后台，再在同一会话发送纯文本续接消息。
2. 在 Chat Completions 会话中连续两轮各发送一段短文本与三张 `big.png`，中间以新会话发送 `--image bad.png` 验证本地预检，再发送第三轮三张 `big.png` 触发跨轮累计超预算，最后执行 `session rewind <session_id> 0` 并重新发送三张 `big.png`。
3. 在开启重试的 Chat Completions 会话中发送带小 PNG 的请求命中假模型返回的 `HTTP 413` 响应。

## 断言

- 五种协议首轮均退出 `0` 并完成；会话视图（`watch`）中本轮用户输入条目的片段依次为提示文本与两张结构化图片引用（`media_type` 按魔数识别为 `png` 与 `jpeg`、`byte_length` 为 `8` 与 `4`、`sha256` 为 64 位十六进制）；首次模型请求中用户消息内容数组恰为 `[带开标签的文本块, PNG 图片块, JPEG 图片块, 闭标签文本块]` 且图片块与线格式合同逐字段相等；删图重启后，第二次请求按原顺序重放首轮用户输入（含两张图片），其后为首轮回复与本轮文本。
- 3 MiB 大图前两轮均退出 `0` 并完成，第二次请求正文长度约为第一次的两倍；`bad.png` 本地预检以退出码 `2`、`submit` 回执 `rejected`（`bad_request`）拒绝，不触发 `configure`、不增加会话列表数量且不发模型请求；第三轮跨轮累计超过 32 MiB 在接受前以退出码 `2`、`bad_request` 及[本地请求体预算固定说明](../../../architecture/llm/protocols.md#发送前检查与大小限制)拒绝，模型请求数保持为 2；`rewind 0` 后重发退出 `0` 并完成，第 3 次请求只含系统提示与本轮用户消息且正文长度与第一次同一量级。
- `HTTP 413` 用例以退出码 `1`、`failed` 及停止原因 `provider_error` 结束，开启重试且响应带 `x-should-retry: true` 时模型服务仍只收到 1 次请求，错误说明同时包含[远端 413 固定恢复提示](../../../architecture/llm/protocols.md#发送前检查与大小限制)与远端错误正文中的 `message`；大图请求的证据只记录请求数与各次正文长度。
