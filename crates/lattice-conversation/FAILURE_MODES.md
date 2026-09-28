# 对话纯数据删除边界

S5 删除旧 Git HeadTranscript、未使用的 ConversationHistory 和 NativeMessage 内的派生 ToolResult 字符串表示；生产结果仍由 StepContent/StoredToolResult 保存。

- S5-C01：删旧分支时误改 Text/Reasoning/ToolCall、provider origin 或 replay 的 serde/摘要。保留现有消息往返、绑定、摘要 golden 与 provider projection fixtures；不改黄金值。
- S5-C02：删除旧结果表示不能弱化 Step 的调用/结果数量、身份、名称和顺序配对。继续使用 llm 的 structured_steps_require_exact_pairing、六种结构化结果投影与大输出回归。
- 仅删除旧容器专属测试及旧 variant 的构造/穷举分支；原消息往返继续保护文字与 reasoning，不增加“旧 variant 已删除”测试。对应 E2E 使用多步工具、会话退回分叉和正常历史关联。新增/迁移内容及真实变异红、验证结果留在 S5 交接。
