---
status: active
---

# 对话数据与模型投影

> 所属领域：[会话与 Turn](README.md)

## 有序消息与 ToolCall

`NativeMessage<ToolCallState>` 用有序 `MessagePart` 保存消息。`UnboundToolCall` 表示模型生成、待登记的调用，`BoundToolCall` 表示协调层已经分配 InvocationId 的调用；登记保留原片段顺序。

助手消息的 parts 按到达顺序保存正文、reasoning 与工具调用的交错：相邻同类内容合并为一段，工具调用排在它首次出现的位置。各协议解析器把部件增量交给同一个回复累积器，由它按此生成 parts 并投影显示增量。Chat Completions 这类只有一个正文字段和一个 reasoning 字段的请求格式，由 provider 投影在发请求时合并同类内容，同一 provider 的回放不受影响。

Tool 参数经 [`lattice-conversation`](../../../crates/lattice-conversation/src/lib.rs) 的 `canonical_json` 生成规范对象，摘要由 `arguments_digest` 计算。有效调用要求参数为一个 JSON 对象，ToolCall 完整且无重复；单次完整响应最多 32 个 ToolCall。任一个调用无效或数量超限时拒绝整个尝试。完整 reasoning 进入持久对话；停止的尝试只记原因与诊断（provider 错误或批次校验结果），不保存已收到的部分文本、reasoning 和调用数据。

### 对话与投影边界

空串、数组、标量、畸形 JSON 不能作为 Tool 参数对象。拒绝尝试不产生可执行调用或 InvocationId，也不把该尝试的助手正文作为成功消息；停止尝试的部分内容不保存，不成为回复，也不发给 provider。超限批次不截断为可执行批次。这些限制让每个调用槽位始终对应一条完整有效回复。

工具结果 `StoredToolResult` 以 `ResultPayload` 按结局保存载荷：成功输出按 65,536 字节规范 JSON 上限保存正文或 `OutputTooLarge` 摘要；失败载荷为 `ToolError { code, message }`，保存字符串机器错误码 `code` 与说明文本 `message`（[ADR 0009](../../adr/0009-remove-legacy-lancet-tool-executor.md#4-会话库升版与存储收敛)）；不确定结果保存 `IndeterminateCause`，未执行结果不带载荷。provider 投影将 `Failed(ToolError)` 编码为 `{"error":{"code":<code 字符串>,"message":<message>}}`（`message` 按 UTF-8 字符边界裁剪，使其规范 JSON 字符串编码不超过 65,536 字节，即 `MAX_TOOL_CONTENT_BYTES`），见[调用槽位](steps.md#调用槽位)。

### 用户图片与单轮输入规范化

`MessageSemantic<ToolCallState>` 包含图片变体 `Image { image: ImageRef }`。`ImageRef` 由 64 位小写十六进制编码的 `sha256: [u8; 32]`、`media_type: ImageMediaType`（`png`、`jpeg`、`webp`、`gif`，仅由 `ImageMediaType::sniff` 读取前 12 字节文件头魔数识别，忽略文件扩展名与外部声明的 MIME）和原始字节长度 `byte_length: u64` 组成。

- **放置约束。** `MessageSemantic::Image` 只允许出现在 `MessageRole::User` 消息中，且所在 `MessagePart` 的 `replay_state` 必须为 `None`（由 `validate_image_placement` 核验）；助手、系统或工具消息包含图片，或图片片段携带回放状态时均拒绝。消息中每个 `ImageRef` 在会话库打开与全库校验时的核对规则见[会话库身份、版本与打开](../storage.md#身份版本与打开)。
- **单轮上限。** 由 [`lattice-conversation`](../../../crates/lattice-conversation/src/limits.rs) 统一定义：
  - `MAX_IMAGE_BYTES = 3_932_160`（3.75 MiB）：单张原始图片字节长度必须满足 `0 < byte_length <= MAX_IMAGE_BYTES`；
  - `MAX_USER_IMAGES_PER_TURN = 8`：单轮用户消息按出现次数计最多 8 张图片（相同摘要重复出现按出现次数累计）；
  - `MAX_USER_IMAGE_TOTAL_BYTES = 10_485_760`（10 MiB）：单轮原始图片字节总和上限；
  - `MAX_USER_TEXT_BYTES = 2_097_152`（2 MiB）：单轮全部 `Text` 片段 UTF-8 字节总和上限。
- **保序规范化。** `normalize_user_message` 只接受 `Text` 与 `Image` 片段，先核验上述四项单轮上限，再剔除空字符串 `Text` 片段（仅剔除 `text.is_empty()`，不裁剪空白字符）、合并剔除后相邻的 `Text` 片段（不跨 `Image` 合并，原样保留每个 `Image` 的顺序与重复），并要求规范化后的片段列表非空；生成的 `NativeMessage` 角色为 `User`、`provider_origin` 为 `None`、每个片段 `replay_state` 为 `None`。

## 回放与跨 provider 投影
每次尝试记录来源：协议与 provider `id`。适配层按[回放来源](../llm/protocols.md#回放来源)的规则生成投影：协议与 provider `id` 都相同时保留 reasoning 和回放状态，否则去除。来源不含 endpoint：同一 provider `id` 在 `providers.toml` 中改了 `base_url` 之后仍回放此前的签名，新 endpoint 可能拒绝这些签名；要换 endpoint 时使用新的 provider `id`。provider call ID 在工具调用与工具结果两侧原样保留，切换协议或 provider 都不去除；旧历史缺少 ID 时省略对应字段，不合成 ID，由目标 API 判断是否接受。用户消息中的 `ImageRef` 是厂商中立载荷，回放时按目标协议直接投影，不随来源裁剪。切回原来源时使用树中保存的原消息。不透明回放材料逐片段原样保留，单片段上限 64 KiB；即使片段正文为空，也保留其回放状态。连续两个 UserMessage 由投影按适配器要求合并或原样传递。

类型和编码由 [`lattice-conversation`](../../../crates/lattice-conversation/src/lib.rs) 维护。provider Tool Call ID 仅是 provider 协议材料，不参与会话登记或执行身份分配；跨 provider 投影不伪造目标 provider 身份材料。发给模型的对话是当前位置从根到该节点的路径，provider 投影是纯函数，不在请求时扫描或补齐缺失结果。
