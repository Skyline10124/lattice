# artemis 设计构想与方向

持续记录设计思路、架构构想和开发方向。

**最后更新**: 2026-04-29

---

## 一、定位

> 一个快的、单二进制的 LLM 调用库，开发者引用进自己的项目，按需叠加 agent 插件。

**不是什么**：不做全能 agent 框架、不做可视化平台、不做工作流引擎。

**四个维度**：

| 维度 | 含义 |
|------|------|
| **速度** | Rust 核心，零开销抽象 |
| **最小化部署** | 单二进制 + catalog.json |
| **插件自定义** | 类型化插件（Input → to_prompt → LLM → from_output → Output） |
| **垂类精通** | artemis-core 只做模型路由 + 推理，不碰 agent logic |

**与其他项目的区别**：

| 项目 | 是什么 | 和 artemis 的关系 |
|------|--------|------------------|
| **OpenRouter** | 模型路由 SaaS | 竞品，但 artemis 是库不是 SaaS |
| **LiteLLM** | 模型网关 | 竞品，但 artemis 是 Rust 不是 Python |
| **LangGraph / CrewAI** | 多 agent 编排框架 | 不同层，重框架 vs 轻库 |
| **n8n / Dify** | 可视化工作流 | 不同赛道，业务自动化 vs 开发者工具 |
| **Google A2A** | Agent 间通信协议 | 参考，artemis 的 handoff 层可以参考 |
| **Anthropic MCP** | 模型→工具协议 | 互补，插件内的工具用 MCP 连 |
| **AgentUnion ACP** | Agent 通信协议（中国） | 参考 |

---

## 二、核心理念：LLM 是函数，不是大脑

### 2.1 控制反转

```
传统 agent（LLM 是大脑）：
  给 prompt + tools → LLM 决定一切 → 代码围观

artemis（LLM 是工具）：
  代码决定什么时候调 LLM → LLM 执行推理 → 代码验证结果 → 代码决定下一步
```

LLM 不参与控制流。它是一个非确定性的 `fn(String) -> String`，被封装在类型安全的边界里。

### 2.2 类型化的 LLM 调用 (Derivation)

```rust
// 每个插件定义一组输入/输出类型
struct ReviewInput {
    diff: String,
    file_path: String,
    context_rules: Vec<String>,
}

struct ReviewOutput {
    issues: Vec<Issue>,
    suggested_handoff: Option<AgentId>,
    confidence: f64,
}

// LLM 被包裹在类型边界里
fn code_review(diff: &str) -> Result<ReviewOutput, ReviewError> {
    let input = ReviewInput::new(diff);
    let prompt = input.to_prompt()?;         // 代码控制输入格式
    let raw = llm.invoke("sonnet", prompt)?;  // LLM 只做推理
    let output = ReviewOutput::from_raw(raw)?; // 代码验证输出
    output.validate()?;                       // 代码再验证
    Ok(output)
}
```

对比 Nix：LLM = builder，代码 = derivation graph。代码决定构建什么、怎么构建、输出怎么验证，builder 只是执行器。

### 2.3 输入格式化的工程价值

输出格式化简单（LLM 端已支持 `response_format: json_schema`）。输入格式化才是核心工程工作：

- 这个任务该喂什么上下文？diff、文件、CLAUDE.md？
- 怎么拼 prompt？顺序、粒度、示例、否定约束
- token 预算怎么分？

每个插件作者的主要工作量在 `to_prompt()`。很麻烦，但做好后有三大收益：

**可测试**：
```python
def test_review_input():
    prompt = CodeReview().to_prompt(ReviewInput(diff="...", file="auth.rs"))
    assert "auth.rs" in prompt
    assert len(tokens(prompt)) < 4000

def test_review_output():
    result = CodeReview().from_output(raw_json)
    assert result.issues[0].file == "auth.rs"
    assert result.confidence >= 0.8
```

**可组合**：
```
CodeReview 产出 ReviewOutput
  → 直接传给 Refactor 的 RefactorInput
    → 直接传给 TestGen 的 TestGenInput
```
类型保证链路上每一段的输入合法，不需要祈祷下家能看懂。

**可迭代**：改 `to_prompt` 不影响 `from_output`，下游输入契约不变，A/B test prompt 变安全。

### 2.4 对比

| | Prompt 工程 | artemis 插件 | n8n / Dify |
|------|-----------|-------------|-----------|
| 控制权 | LLM | 代码 | 可视化拖拽 |
| 可测试性 | 跑一遍看 | 输入/输出可单测 | 跑一遍看 |
| 组合方式 | prompt 里写"请交给..." | 类型安全的函数组合 | 可视化连线 |
| 失败处理 | prompt 说"再试一次" | 类型系统兜底 + retry | 手动配错误分支 |
| 目标用户 | 所有人 | 开发者 | 非开发者 |
| LLM 角色 | 大脑 | 函数内部实现 | 流程节点 |

---

## 三、架构

### 3.1 整体结构

```
┌───────────────────────────────────────────┐
│    Python 胶水层：插件加载 + 组合 + handoff   │
│    pip install artemis-code-review-plugin   │
├───────────────────────────────────────────┤
│    artemis-core（Rust，最小内核）            │
│    模型路由 + HTTP 推理 + SSE + retry       │
│    PyO3 暴露给 Python                       │
└───────────────────────────────────────────┘
```

**为什么 Python 做胶水**：
- AI 生态全在 Python，插件作者 python 比 rust 多 100 倍
- `importlib` 动态加载，`pip` 分发 —— 这两件事 Rust 要花大力气
- 胶水工作全在冷路径（加载、组装、路由），不在推理热路径
- 推理延迟 99.9% 是网络 I/O + token 生成，Python 不影响

**为什么 Rust 做核**：
- 热路径：模型解析、HTTP、SSE 解析、retry
- 信任边界：持有凭证，不可开放给插件

### 3.2 核心边界

| 层 | 职责 | 信任 |
|----|------|------|
| **artemis-core** | 模型路由、HTTP、SSE、retry、token 估算 | 持凭证 |
| **Python 胶水** | 加载插件、组合 agent、handoff 路由 | 不碰凭证 |
| **插件** | 定义 Input/Output 类型、to_prompt、from_output、handoff 目标 | 不碰凭证、不碰 HTTP |

插件不该做的事：注册 HTTP handler、拦截 retry、直接读 env var、创建 reqwest client。这些进核。

### 3.3 插件结构

```
artemis-code-review/
  setup.py
  args.toml              # 声明式：Input 类型字段声明
  prompts/
    review.md            # 自然语言：agent 身份 + 推理策略
  src/
    input.py             # ReviewInput 定义 + to_prompt()
    output.py            # ReviewOutput 定义 + from_output() + validate()
    behavior.py          # 编程语言：handoff 路由、超时、重试
```

```python
# behavior.py —— 代码控制，确定性的
class CodeReviewBehavior:
    def should_handoff(self, output: ReviewOutput) -> Optional[AgentId]:
        if output.confidence < 0.7:
            return None  # 不够自信，让人工介入
        if any(i.severity == Severity.CRITICAL for i in output.issues):
            return "refactor"  # 有严重问题，交给重构 agent
        return None  # 无问题，结束

    def on_parse_error(self, raw: str, retry_count: int) -> Action:
        if retry_count < 3:
            return Action.RETRY
        return Action.FALLBACK_HUMAN
```

### 3.4 插件 vs MCP vs CLI

```
artemis 插件：agent 的完整定义（Input/Output + prompt + tools + handoff + behavior）
MCP：        工具的协议（如何暴露、调用、返回），插件内部通过 MCP 引用工具
CLI：        无 AI 的纯代码工具
```

三层各管各的：artemis 管 agent 组装和行为，MCP 管工具协议，CLI 不涉及 AI。

---

## 五、
---

## 五、Dogfooding

用 artemis 开发 artemis 本身。

- 写代码 → 自己跑推理 → 发现 bug → 修
- 处理自己的 codebase 就是最真实 benchmark
- 不做全能框架的定位 → 不需要"什么都行" → 只需要"开发自己够用"

---

## 六、路线图

### 第一阶段：内核瘦身 ✅

- [x] 从内核移除 agent_loop → 上层负责
- [x] 从内核移除 tool_boundary → 上层负责
- [x] 从内核移除 streaming_bridge → 独立 crate
- [x] 删除 `rig-core` 依赖（H11）
- [x] 删除 `ProviderConfig`、`TransportType` 死代码（M12, M13）

### 第二阶段：内核收敛 + 安全 ✅

- [x] C1: 合并双 Transport trait
- [x] H4: 统一 ErrorClassifier
- [x] H9: 提取公共 provider 逻辑（移除 providers/ 目录，核心逻辑收敛至 transport/）
- [x] H6: Regex `LazyLock`
- [x] H7: 共享 `reqwest::Client`
- [x] H13: HTTP 超时
- [x] H14: ResolvedModel Debug 脱敏

### 第三阶段：Dogfooding 就绪 ✅

- [x] C2: Agent 独立 crate（artemis-agent）
- [x] H2: 对话历史维护（Agent state）
- [x] H3: tool result 重入机制（submit_tools）

### 第四阶段：类型化插件系统（下一阶段）

- [ ] 插件 Input/Output 类型接口定义
- [ ] `to_prompt()` / `from_output()` trait
- [ ] 输出解析 + 校验 + 重试框架
- [ ] Python 胶水层：插件加载 + 组合
- [ ] artemis-agent-protocol：handoff 路由


## 七、
## 七、竞品与参考

| 项目 | 类型 | 参考价值 |
|------|------|---------|
| [Google A2A](https://github.com/google/A2A) | Agent 通信协议 | handoff 层设计参考 |
| [Anthropic MCP](https://modelcontextprotocol.io) | 模型→工具协议 | 插件内工具引用 |
| [AgentUnion ACP](https://acp.agentunion.cn) | Agent 通信协议（中国） | AID 身份体系参考 |
| [OpenRouter](https://openrouter.ai) | 模型路由 SaaS | artemis-core 对标 |
| [LiteLLM](https://github.com/BerriAI/litellm) | 模型网关 | 模型管理参考 |
| [Lagant](https://github.com/InternLM/Lagent) | 轻量 Agent 框架 | 轻盈理念相似 |
| ICML 2024 "Multiagent Debate" | 论文 | 多 agent 协作理论 |

---

## 八、相关文档

- `code-review-report.md` — 完整审查报告，44 个发现问题
- `architecture.md` — 当前架构文档
- `CLAUDE.md` — 项目开发指南

---

*此文档随开发推进持续更新。*
