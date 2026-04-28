# artemis-core 设计构想与方向

持续记录设计思路、架构构想和开发方向，指导后续开发。

**最后更新**: 2026-04-29

---

## 一、定位：速度 + 最小化 + 插件 + 垂类

不做一个全能型 agent 框架。聚焦窄而深的场景。

| 维度 | 目标 |
|------|------|
| **速度** | Rust 核心，零开销抽象，不做重架构 |
| **最小化部署** | 单二进制 + catalog.json，不绑 Python 运行时，不拖数据库 |
| **插件自定义** | Overlay 模式注入 provider / tool / 路由规则 |
| **垂类精通** | artemis-core 只做一件事：给定 model + messages → 高效返回 response |

---

## 二、微 Agent 架构

### 2.1 分层

```
┌──────────────────────────────────────────┐
│          Agent 通信层（协议 + 路由）        │  ← 独立 crate
├──────────────────────────────────────────┤
│        插件组成层（overlay compose）        │  ← 独立 crate
├──────────────────────────────────────────┤
│     插件：code-review │ refactor │ test   │  ← 社区扩展
├──────────────────────────────────────────┤
│          artemis-core（最小内核）           │  ← 只做模型路由 + 推理
└──────────────────────────────────────────┘
```

### 2.2 插件 = 微型垂类 Agent

```rust
struct AgentPlugin {
    name: String,
    system_prompt: String,
    tools: Vec<ToolDefinition>,
    preferred_model: String,      // "sonnet" / "deepseek-v4-pro"
    handoff_targets: Vec<String>, // 可以移交给哪些 agent
}

// 示例
let code_review = AgentPlugin {
    name: "code-review",
    system_prompt: "你是资深代码审查工程师。审查代码正确性、安全和设计。",
    tools: vec![read_file, run_test, list_directory],
    preferred_model: "sonnet",
    handoff_targets: vec!["refactor", "security-audit"],
};
```

### 2.3 组合 = 不同插件拼出不同垂类

```
code-review + security-audit     → 安全审查 agent
refactor + test-gen              → TDD agent
code-review + refactor + test    → 全栈开发 agent
```

每个组合就是一组 AgentPlugin 的 overlay merge，不改核心代码。

### 2.4 通信层：Agent 间握手

```
Agent A                     Agent B
  │                            │
  │  handoff {                 │
  │    target: "security",     │
  │    payload: { files: [...]},│
  │    context_summary: "..."  │
  │  }                         │
  │ ──────────────────────────→│
  │                            │ process...
  │  result ←──────────────────│
  │                            │
```

- 每个 agent 保持独立 context window，不共享上下文
- 只通过结构化 handoff 传递结果
- 类似 Anthropic tool use 协议，但 agent 级别

### 2.5 artemis-core 的边界

artemis-core 不碰：
- Agent loop
- 工具执行
- Agent 间通信
- 插件加载

artemis-core 只做：
- 模型名 → 解析 provider / 凭证 / 协议
- 消息 → HTTP 请求 → 流式响应
- 重试、错误分类
- Token 估算

Agent loop、插件系统、通信层都是上层独立 crate，通过 overlay 注入。

---

## 三、Nix 范式

### 3.1 声明式配置 + lockfile

```
artemis.toml          # 声明需求：model = "sonnet", budget = "$50"
artemis.lock          # 解析锁定：provider=anthropic, model=claude-sonnet-4-6-20250514
```

- 模型解析从"运行时动态选最优"变为"提前锁定，可审计，可复现"
- 类似 `flake.nix` + `flake.lock`

### 3.2 内容寻址缓存

```
/content-cache/
  sha256(prompt + model + params) → response
```

- 同样 prompt + model + 参数 → 同样 hash → 直接返回缓存
- Nix store 思路，但存的是 LLM 响应

### 3.3 派生式任务描述 (Derivation)

```rust
InferenceTask {
    model: "sonnet",
    messages: [...],
    budget: 5000,  // tokens
}
// → 构建 → Response
// 失败 → 查看构建日志
```

### 3.4 Overlay 模式

```rust
// 替代 register_model()，改为声明式叠加
let overlay = CatalogOverlay::new()
    .add_model("my-fine-tune", ...)
    .patch_provider("anthropic", |p| p.timeout = 60);

let catalog = Catalog::default().with_overlay(overlay);
```

---

## 四、核心策略：Dogfooding

用 artemis-core 开发 artemis-core 本身。

- 写代码 → 自己跑推理 → 发现 bug → 修
- 处理自己的 codebase 就是最真实 benchmark
- catalog 98+ 模型、路由逻辑，每天在用就是在测

**当前卡脖子的先修**：

| 优先级 | 问题 | 描述 |
|--------|------|------|
| C2 | AgentLoop 无 tokio runtime | `futures::executor::block_on` 无法运行真实 provider |
| H2 | 对话历史丢失 | `submit_tool_result` 丢弃完整历史，多轮对话断裂 |
| H3 | Tool result 重入缺失 | 硬编码 "mock tool result" |
| C1 | 双 Transport trait | 同名冲突，接口不一致 |
| H4 | 双 ErrorClassifier | 两套实现分歧 |

---

## 五、修复路线图

### 第一阶段：内核瘦身（砍掉不该在内核里的）

- [ ] 从内核移除 agent_loop → 独立 crate
- [ ] 从内核移除 tool_boundary → 上层负责
- [ ] 从内核移除 streaming_bridge（Python 相关）→ 独立 crate
- [ ] 删除 `rig-core` 依赖（H11）
- [ ] 删除 `ProviderConfig`、`TransportType` 死代码（M12, M13）

### 第二阶段：内核收敛

- [ ] C1: 合并双 Transport trait
- [ ] H4: 统一 ErrorClassifier
- [ ] H9: 提取公共 provider 逻辑
- [ ] H6: Regex `LazyLock`
- [ ] H7: 共享 `reqwest::Client`
- [ ] H13: HTTP 超时
- [ ] H14: ResolvedModel Debug 脱敏

### 第三阶段：上层 crate

- [ ] `artemis-agent-compose` — 插件定义 + 组合 + overlay
- [ ] `artemis-agent-protocol` — handoff 协议 + 通信路由
- [ ] artemis-core dogfooding 就绪
- [ ] 声明式配置 + lockfile 原型

### 第四阶段：Nix 范式 + 安全

- [ ] 内容寻址缓存
- [ ] Derivation 式任务模型
- [ ] 沙箱工具执行
- [ ] H12: base_url HTTPS 校验
- [ ] M14-M16: 热路径优化

---

## 六、相关文档

- `code-review-report.md` — 完整审查报告，44 个发现问题
- `architecture.md` — 当前架构文档
- `CLAUDE.md` — 项目开发指南

---

*此文档随开发推进持续更新。新想法、设计决策、方向变更均记录于此。*
