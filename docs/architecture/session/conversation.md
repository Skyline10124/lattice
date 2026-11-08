---
status: active
---

# 对话数据与模型投影

> 所属领域：[会话与 Turn](README.md)

## 有序消息与 ToolCall

`NativeMessage<ToolCallState>` 用有序 `MessagePart` 保存消息。`UnboundToolCall` 表示模型生成、待登记的调用，`BoundToolCall` 表示协调层已经分配 InvocationId 的调用；登记保留原片段顺序。

助手消息的 parts 按到达顺序保存正文、reasoning 与工具调用的交错：相邻同类内容合并为一段，工具调用排在它首次出现的位置。各协议解析器把部件增量交给同一个回复累积器，由它按此生成 parts 并投影显示增量。Chat Completions 这类只有一个正文字段和一个 reasoning 字段的请求格式，由 provider 投影在发请求时合并同类内容，同一 provider 的回放不受影响。

Tool 参数通过协议 canonical JSON 生成规范对象和摘要。有效调用要求参数为一个 JSON 对象，ToolCall 完整且无重复；单次完整响应最多 32 个 ToolCall。任一个调用无效或数量超限时拒绝整个尝试。完整 reasoning 进入持久对话；停止的尝试只记原因与诊断（provider 错误或批次校验结果），不保存已收到的部分文本、reasoning 和调用数据。

### 对话与投影边界

空串、数组、标量、畸形 JSON 不能作为 Tool 参数对象。拒绝尝试不产生可执行调用或 InvocationId，也不把该尝试的助手正文作为成功消息；停止尝试的部分内容不保存，不成为回复，也不发给 provider。超限批次不截断为可执行批次。这些限制让每个调用槽位始终对应一条完整有效回复。

## 回放与跨 provider 投影
每次尝试记录来源：协议与 provider `id`。适配层按[回放来源](../llm/protocols.md#回放来源)的规则生成投影：协议与 provider `id` 都相同时保留 reasoning 和回放状态，否则去除。来源不含 endpoint：同一 provider `id` 在 `providers.toml` 中改了 `base_url` 之后仍回放此前的签名，新 endpoint 可能拒绝这些签名；要换 endpoint 时使用新的 provider `id`。provider call ID 在工具调用与工具结果两侧原样保留，切换协议或 provider 都不去除；旧历史缺少 ID 时省略对应字段，不合成 ID，由目标 API 判断是否接受。切回原来源时使用树中保存的原消息。不透明回放材料逐片段原样保留，单片段上限 64 KiB；即使片段正文为空，也保留其回放状态。连续两个 UserMessage 由投影按适配器要求合并或原样传递。

类型和编码由 [`lattice-conversation`](../../../crates/lattice-conversation/src/lib.rs) 维护。provider Tool Call ID 仅是 provider 协议材料，不参与会话登记或执行身份分配；跨 provider 投影不伪造目标 provider 身份材料。发给模型的对话是当前位置从根到该节点的路径，provider 投影是纯函数，不在请求时扫描或补齐缺失结果。
