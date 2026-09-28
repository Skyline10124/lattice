# 结构化历史与流失败边界

S2 WP1 实施前清单。依据 session/lancet 合同及 `.pi/tmp/r2-s2-plan.md` §3。隔离 seam 为正式 provider projection 与 HTTP/SSE 客户端，不调用产品私有库。

| ID | 失败前提与必须保护的断言 | 测试入口 |
| --- | --- | --- |
| W1-02a | Step 结果缺失、多余、重复、乱序、ID/名称不符或助手角色不合法；拒绝整个请求，不靠历史扫描补齐 | `provider_projection` 的配对矩阵 |
| W1-02b | 六种终态和 OutputTooLarge 的文字从结构化载荷派生；成功规范 JSON、不降级 outcome，超大保留 digest/length；三 provider 原有成功 fixture 不改原义 | `provider_projection` 结果测试 |
| W1-02r | Failed/Rejected 丢失数值 code 或 message；message 在4096字节字符边界截断；其余停止终态意外携带 payload/原因 | 恢复 HEAD fixtures，扩展 `structured_result_outcomes_and_large_success` |
| W2-F09 | Anthropic Failed tool_result 仍期待旧裸字符串而非结构化 code/message；仅手工更新首个 content，其余 fixture 保持 HEAD。该改动由 Agent 自行决定，2026-09-28 获维护者事后批准 | 先复跑 `structured_result_fixtures_and_missing_ids` 的既有行为红，再运行 llm 全套 |
| W1-02c | Message 分支携带 Tool/Assistant 字符串桥接；拒绝。跨 provider 丢 reasoning/replay/ID；缺必要 provider ID 类型化拒绝，不合成 | `provider_projection` 请求测试和原 missing-ID 回归 |
| W1-08a | 流内真实网络错误丢 retryable；provider 结构化类别精确映射，未知/协议畸形不可重试，不从文案推断 | provider/parser 与 client 原有错误用例 |
| W1-08b | 开流/流中诊断泄露连接密钥或脱敏后超过2048 Unicode字符（包括省略号）；脱敏先于截断 | client 错误回归 |

真实重试/持久诊断/跨项目接线由 WP2–WP4 E2E 承接，局部通过不表示它们已经通过。

## OpenAI 兼容 reasoning 字段别名（#223 收尾）

| ID | 失败前提与必须保护的断言 | 测试入口 |
| --- | --- | --- |
| R-01 | 兼容服务用 `delta.reasoning`（而非 `reasoning_content`）发文本 reasoning 时被静默丢弃：实时无 Reasoning 事件、最终 parts 缺 R，R/T/R 顺序无从保留 | `openai` parser 的 reasoning 别名测试 |
| R-02 | 同一 delta 同时带 `reasoning_content` 与 `reasoning` 时重复累计同一段；以 `reasoning_content` 为准，只取一个 | 同上 |
