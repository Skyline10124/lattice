# 为什么这样设计

## 问题

LLM 应用的主要痛点不是模型能力不足，而是**行为不可控**。

- Prompt 工程靠祈祷——写好 prompt 丢给 LLM，期待它做对
- Agent 框架把决策权全交给 LLM——什么时候停、什么时候调工具、什么时候移交
- 每个模型、每个 provider 的 API 都不一样——开发者被迫绑定一家

## 解决方案

**模型为中心的解析**：用户说 `"sonnet"`，引擎查 catalog，自动选 provider、匹配 API key、构造请求。不绑任何一家。

**LLM 是函数，不是大脑**：每个任务是 `Input → to_prompt() → LLM.invoke() → from_output() → Output`。代码控制输入格式化、输出验证、handoff 路由。LLM 只做推理。

**约束比说服可靠**：工具白名单（LLM 只能在声明范围内选择）、类型化的输入输出（解析失败 = 重试，不把垃圾传给下游）、代码控制的 behavior（不是 prompt 里写"请再试一次"）。

## 不是什么

- 不是全能 agent 框架
- 不是可视化工作流引擎
- 不是 SaaS
- 不绑定任何模型供应商

## 参考

- [LLM 作为函数](llm-as-function.md)
- [Nix 范式](nix-paradigm.md)
- [竞品分析](competitors.md)
