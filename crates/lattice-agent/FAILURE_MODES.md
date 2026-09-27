# 单次模型尝试失败方式

S2 WP1 实施前清单；seam 为 `ModelStepDriver::run`、正式 OpenAI parser 与可注入 LlmClient。Coordinator 重试策略与持久关闭由 WP2 验证。

| ID | 失败前提与断言 | 入口 |
| --- | --- | --- |
| W1-08c | delta 后的 typed retryable 错误被压成不可重试，或被误报 Complete；开流/流内保留同一 retryable，Stopped 不携完整消息 | model_step、streaming_seam |
| W1-08d | 取消/deadline/OutputLimit/InvalidBatch 原因混淆，流在无 Done/无权威完整消息/重复完整消息时假成功 | 保留现有 model_step 对应失败场景，改枚举断言 |
| W1-08e | S3 按 index 参数计量、同步观察回调、完整 parts 顺序退化 | 原 `openai_stream_limits` 与 model_step 回归 |
| W1-02d | 请求历史把 Step 变回 Tool 字符串，或 system prompt 重复添加；完整配对直接传入 ChatRequest，不伪造缺 provider ID | 原 projection opening 错误与请求夹具 |

诊断只存已脱敏且最多2048字符的 provider_error；最小实现不保存 partial。不以局部 fake 证明真实重试 E2E。

- F12：已取消或 deadline 已到的请求不得首次调用 `stream_chat`；用调用计数及类型化 Stopped(Cancelled/Deadline) 同时断言，不把随后停止误作未打开。
