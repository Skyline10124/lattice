# LLM 作为函数

## 核心理念

LLM 不参与控制流。它是一个非确定性的 `fn(String) -> String`，被封装在类型边界里。

```rust
fn code_review(diff: &str) -> Result<ReviewOutput, ReviewError> {
    let input = ReviewInput::new(diff);
    let prompt = input.to_prompt()?;          // 代码控制输入
    let raw = llm.invoke("sonnet", prompt)?;  // LLM 只做推理
    let output = ReviewOutput::from_raw(raw)?;// 代码验证输出
    output.validate()?;                        // 代码再验证
    Ok(output)
}
```

## 对比

| | 传统 Agent | artemis |
|---|-----------|---------|
| 决策者 | LLM | 代码 |
| 工具调用 | LLM 自由选择 | 白名单约束 |
| 停止条件 | LLM 说了算 | 代码定义 |
| 失败处理 | prompt 说"再试一次" | 类型系统 + retry |
| 可测试性 | 跑一遍看 | 输入输出可单测 |

## vs Prompt 工程

Prompt 工程是"告诉 LLM 怎么做"（说服）。artemis 插件是"限制 LLM 只能做什么"（约束）。说服不可靠，约束可靠。
