# #142 Plan: InvocationId + TurnId + data-only `lattice-conversation`

- Issue: <https://github.com/Skyline10124/lattice/issues/142>
- Normative authority: ADR 0005 (`docs/adr/0005-durable-turn-invocation-and-private-git-stores.md`)
  §1, §7, §19, §20; handoff index `docs/plans/2026-08-13-issue141-accepted-decisions.md`.
- Superseded background: `feat/issue139-invocation-id` 分支的
  `docs/plans/2026-08-06-issue139-invocation-id-grill-decisions.md`（R7/R10/R17 仍提供
  数据模型细节；凡与 ADR 0005 冲突以 ADR 为准）。
- Branch: `feat/issue142-invocation-conversation`, base `main` @ f4a8c905.

## 0. Scope

#142 是 #139 的 additive foundation：**不切换** production JSON-RPC、numeric
`call_id`、evidence backend、operator Adapter、WorkspaceHistory、dispatch 或现有
provider codec 的 live path；不建立双 identity seam。只交付：

1. `lancet-protocol::{InvocationId, TurnId}` strict 类型 + 结构化地址；
2. 新 data-only crate `lattice-conversation`（含 conversation pure-data ownership
   迁移，使 `lattice-agent` 不再依赖 `lattice-session`）；
3. `lattice-llm` 新增 provider replay/projection seam 模块（纯数据、未接
   production codec）+ 三 provider official-wire fixtures。

## 1. Grill 决议（2026-08-13，已与提出者确认）

| # | 决策 | 结论 |
|---|------|------|
| G1 | provider fixture 所测 projection 代码位置 | **lattice-llm 新 seam 模块**（符合 grill §5 ownership：provider 边界归 lattice-llm）。fixtures 在 `crates/lattice-llm/tests/`。不接 production codec。 |
| G2 | 地址中的 `SessionId` 类型 | **lancet-protocol 严格 32-hex 小写 newtype**，匹配现有 `Uuid::new_v4().simple()` 生产格式；无第二身份 seam。 |
| G3 | GraphMessage 家族迁移方式 | **原样搬迁 + re-export**：名字与 serde 形状不变，`lattice-session` re-export 给 lattice-api/cli 现有消费点；production wire/存储格式零变化。 |
| G4 | usage 类型 | `lattice_conversation::TokenUsage { u64, u64, u64 }` 是 provider-neutral per-attempt contract；`lattice-session` 以 `pub use lattice_conversation::TokenUsage as TurnUsage;` 兼容 re-export；`lattice_llm::TokenUsage`(u32) 暂保留，Agent 边界继续 checked widening；`Option<TokenUsage>` 的 `None` = unknown，绝不归零。#139-B 再统一。 |

派生决议（由 ADR 0005 dependency graph 直接推出，不再单独 grill）：

- 结构化地址放 **lancet-protocol**：`lancet-audit`（#139-A InvocationStore）需要
  binding tuple 但不允许依赖 lattice-conversation；ADR §1 把 identities 与
  addresses 同列于 protocol 所有权下。
- `PlanKey`、`ModelAttemptKey` 与两个 Message 地址一并落在 protocol（ADR §1 列全四类）。

## 2. Acceptance 逐条映射

| Acceptance | 交付 |
|---|---|
| strict `InvocationId`/`TurnId` positive/negative/schema tests；无 String alias、第二 newtype、I/O、tokio、生成器 | `lancet-protocol/src/identity.rs`：`InvocationId`、`TurnId` 两个 newtype。hand-rolled 严格 parser（36 chars、连字符位置 8/13/18/23、全小写 hex、version nibble=`4`、variant nibble∈`8/9/a/b`）。只 parse/display/serde/schemars，无 `new_v4`。测试：合法 roundtrip、大写/无连字符/v1/非 variant/错长/非 hex 全拒绝、serde roundtrip、schemars schema fixture。 |
| `ModelMessageAddress { SessionId, TurnId, model_step_ordinal }`、`ToolCallAddress { message, part_ordinal }` 等 structured address contracts | `lancet-protocol/src/address.rs`：`SessionId`（G2）、`ModelMessageAddress`、`ToolCallAddress`、`ModelAttemptKey`、`PlanKey`。纯数据 + serde + schemars。 |
| `lattice-conversation` 只依赖 data-only protocol 与 neutral deps | 新 crate，deps = `lancet-protocol, serde, serde_json, sha2, thiserror`。无 gix/filesystem/store/runtime/provider HTTP/ID 生成器。 |
| OpenAI-compatible/Anthropic/Gemini official-wire fixtures | `lattice-llm/src/provider_projection/`（G1）+ `crates/lattice-llm/tests/provider_projection.rs` + `tests/fixtures/projection/*.json`：real/missing provider ID、part order、signature/thought state、same-provider exact replay、A→B 不泄漏、B→A 恢复。 |
| strict ToolCall batch validation 与 canonical argument digest fixtures；无 empty-to-`{}` | `lattice-conversation::validate_tool_call_batch`：`arguments` 必须恰为一个 JSON object；empty string/array/scalar/malformed/duplicate id/incomplete call 全部 typed error，整个 batch fail closed。`arguments_digest` = SHA-256(domain separator \|\| u64_be(len) \|\| canonical bytes)（ADR §7，收紧 grill R17）。golden digest fixtures。 |
| per-attempt `Option<TokenUsage>` 保留 unknown | `lattice_conversation::TokenUsage`（G4）+ `ModelAttemptRecord { key: ModelAttemptKey, usage: Option<TokenUsage>, … }`。测试：missing ≠ 0；retry attempt 分列。 |
| protocol hard limits：8 MiB arguments、2 MiB assistant、2 MiB reasoning、16 MiB attempt；plus-one → typed interruption/rejection | `lattice-conversation::limits`：`MAX_TOOL_CALL_ARGUMENTS_BYTES = 8 MiB`、`MAX_ASSISTANT_CONTENT_BYTES = 2 MiB`、`MAX_REASONING_CONTENT_BYTES = 2 MiB`、`MAX_MODEL_ATTEMPT_BYTES = 16 MiB`。`AttemptAssembly` 计量器：超限即 typed `AttemptInterruption::OutputLimitExceeded { limit, actual }`，不产生 partial conversation/InvocationId。exact-boundary success + plus-one fail 双测。 |
| 迁移 conversation pure-data ownership，`lattice-agent` 不依赖 `lattice-session`；persistence 仍属 SessionStore | G3：四类型迁入 conversation；session re-export；agent Cargo.toml 删 `lattice-session` 加 `lattice-conversation`；`cargo tree -p lattice-agent` 断言无 session 边。 |
| 不接 production JSON-RPC/transport/events/cancellation/SQLite/operator/WorkspaceHistory/real dispatch；无双 identity seam | 静态自查：`git diff` 不涉及 `lancet-dispatch/lancet-native/lancet-cep/lancet-audit/lancet-runtime` src；production `call_id` 路径零改动。 |
| 更新 graph/docs/AGENTS，`cargo xtask ci` 通过 | root AGENTS.md graph + crate roles、crates/{lattice-conversation,lattice-agent,lattice-llm,lattice-session,lancet-protocol}/AGENTS.md、docs/ARCHITECTURE.md（如引用 graph）、FIX.md。 |

## 3. 文件级工作分解

### 3.1 `lancet-protocol`（identity + address）

新增 `src/identity.rs`：

```rust
/// Strict lowercase hyphenated UUID v4. Parse/display/serde/schema only —
/// generation belongs to `lattice-api::TurnCoordinator` (ADR 0005 §1).
pub struct InvocationId(uuid::NoCrate); // 示意：内部存 [u8; 16]，不依赖 uuid crate
```

实现要点（贴近 `canonical.rs` 的现有风格：小函数 + `#[cfg(test)]` 同文件测试）：

- `fn parse(raw: &str) -> Result<Self, IdParseError>`：长度 36；位置 8/13/18/23 为 `-`；
  其余全为小写 hex；`raw[14] == '4'`；`raw[19] ∈ {'8','9','a','b'}`。任何偏差 typed error
  （`InvalidLength / InvalidSeparator / InvalidCharacter / InvalidVersion / InvalidVariant`）。
- `Display` 输出规范小写连字符形式；`FromStr` = `parse`；`Serialize`/`Deserialize` 走
  字符串且反序列化同样严格；`schemars::JsonSchema` 为正则 pattern 的 string。
- `TurnId` 同构宏或共享泛型内核（一个 `UuidV4` 内部 repr + 两个零成本 newtype，
  避免 String alias 与第二 public newtype 之间的含糊）。
- `SessionId`（G2）：严格 32 位小写 hex（`[0-9a-f]`），长度恰 32；同样
  parse/display/serde/schemars，无生成器。

新增 `src/address.rs`：

```rust
pub struct ModelMessageAddress { pub session_id: SessionId, pub turn_id: TurnId,
    pub model_step_ordinal: u32 }
pub struct ToolCallAddress { pub message: ModelMessageAddress, pub part_ordinal: u32 }
pub struct ModelAttemptKey { pub turn_id: TurnId, pub model_step_ordinal: u32,
    pub attempt_ordinal: u32 }
pub struct PlanKey { pub turn_id: TurnId, pub model_step_ordinal: u32 }
```

derive: `Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`。

`lib.rs` re-export 两类类型；`Cargo.toml` 不加任何新依赖（sha2 不需要：digest
helper 放 conversation，protocol 保持最薄；digest 的 typed 结果 `[u8;32]` 不需要
protocol 持有）。

### 3.2 新 crate `lattice-conversation`

```text
crates/lattice-conversation/
├── Cargo.toml            # lancet-protocol, serde, serde_json, sha2, thiserror
├── AGENTS.md
└── src/
    ├── lib.rs            # re-export + crate doc（ADR 0005 §7 所有权声明）
    ├── graph_types.rs    # G3 原样搬迁：GraphMessage / GraphToolCall / HeadTranscript
    ├── usage.rs          # TokenUsage { u64×3 }（G4）
    ├── message.rs        # NativeMessage<S> / MessagePart<S> / MessageSemantic<S> /
    │                     #   ProviderOrigin / OpaqueProviderState（bounded）
    ├── tool_call.rs      # UnboundToolCall / BoundToolCall typestates +
    │                     #   validate_tool_call_batch + arguments_digest
    ├── outcome.rs        # ToolResultOutcome closed sum（Succeeded/Failed/Rejected/
    │                     #   Cancelled/NotExecuted/Indeterminate）
    ├── attempt.rs        # ModelAttemptRecord + AttemptOutcome（Complete/Rejected/
    │                     #   Interrupted typed reasons）
    ├── digest.rs         # semantic_digest / replay_state_digest / arguments_digest
    │                     #   （SHA-256 + domain separator + u64_be len + canonical）
    ├── limits.rs         # 4 个 protocol hard limit + AttemptAssembly 计量器
    └── history.rs        # ConversationHistory（ordered NativeMessage<BoundToolCall> +
                          #   per-attempt records；数据容器，无 I/O）
```

关键 contract（逐字对应 ADR §7 与 grill R7）：

```rust
pub struct NativeMessage<S: ToolCallState> {
    pub role: MessageRole,                       // System | User | Assistant | Tool
    pub provider_origin: Option<ProviderOrigin>, // 仅 assistant/provider 产出携带
    pub parts: Vec<MessagePart<S>>,              // Vec 顺序 = part ordinal
}

pub struct MessagePart<S: ToolCallState> {
    pub semantic: MessageSemantic<S>,
    pub replay_state: Option<OpaqueProviderState>, // part-local，bounded
}

pub enum MessageSemantic<S: ToolCallState> {
    Text { text: String },
    Reasoning { text: String },
    ToolCall { call: S },
    ToolResult { invocation_id: InvocationId, name: String, outcome: ToolResultOutcome },
}

pub struct UnboundToolCall { pub provider_call_id: Option<String>,
    pub name: String, pub arguments: serde_json::Value /* 必为 Object */ }

pub struct BoundToolCall { pub invocation_id: InvocationId,
    pub provider_call_id: Option<String>, pub name: String,
    pub arguments: serde_json::Value, pub arguments_digest: [u8; 32],
    pub address: ToolCallAddress }
```

- `ToolCallState` 是 sealed marker trait（`Unbound`/`Bound` 两个 ZST 或直接以
  `UnboundToolCall`/`BoundToolCall` 为 state 参数——选后者，少一层间接）。
- `OpaqueProviderState`：`String`，上限 `MAX_OPAQUE_REPLAY_STATE_BYTES = 64 KiB`
  （bounded；超出 typed error）。signature/thought state 原样保存，包括携带
  replay state 的空 text part 也不得被过滤（grill R7）。
- digest 分离（ADR §7）：
  - `semantic_digest`：role + ordered parts 的 neutral semantic（text、完整
    reasoning、ToolCall 的 name+canonical arguments、ToolResult 的
    invocation_id/name/outcome），不含 provider origin / provider_call_id /
    replay_state；
  - `replay_state_digest`：provider origin + ordered (provider_call_id, replay_state)
    序列。两者各自 domain separator。
- `validate_tool_call_batch(calls: &[RawToolCall { id: Option<String>, name: String,
  arguments: String }]) -> Result<Vec<UnboundToolCall>, ToolCallValidation>`：
  - `arguments` 解析失败 / 非 object（含 empty string、array、scalar）→
    `InvalidArguments`；
  - duplicate provider_call_id（均非 None 且相等）→ `DuplicateToolCallId`；
  - name 为空 / id 缺失（incomplete）→ `IncompleteToolCall`；
  - arguments 超过 8 MiB → `ArgumentsTooLarge`；
  - **不做** empty→`{}` normalization：empty string 是 InvalidArguments。
- `AttemptAssembly`：流式累加 assistant text / reasoning / tool arguments 字节数
  （UTF-8 bytes），任一 limit plus-one → `AttemptInterruption::OutputLimitExceeded
  { limit, actual }`；attempt 总量 > 16 MiB 同。不产生 partial conversation。
- `ModelAttemptRecord { key: ModelAttemptKey, origin: ProviderOrigin,
  usage: Option<TokenUsage>, outcome: AttemptOutcome }`；`AttemptOutcome =
  Complete | Rejected(AttemptRejection) | Interrupted(AttemptInterruption)`；
  `AttemptRejection::InvalidToolCall { diagnostics }`；`AttemptInterruption` 覆盖
  StreamError/Timeout/Cancelled/OutputLimitExceeded/ProcessInterrupted（typed）。

### 3.3 `lattice-llm` provider replay/projection seam（G1）

新模块 `src/provider_projection/`（纯数据，只用 serde/serde_json +
lattice-conversation 类型；无 reqwest/tokio）：

```text
provider_projection/
├── mod.rs        # pub fn project_in / replay_out / project_neutral
├── openai.rs     # chat.completions assistant message: content, reasoning_content?
├── anthropic.rs  # content blocks: text / thinking+signature / tool_use(id,input)
└── gemini.rs     # candidates[].content.parts: text / thought=true+thoughtSignature /
                  #   functionCall(name,args,id?)
```

每个 provider 三个纯函数：

- `wire_to_native(origin, wire: &serde_json::Value) -> Result<NativeMessage<UnboundToolCall>,
  ProjectionError>`：抽取 ordered parts；signature/thought state 进
  `OpaqueProviderState` 原位保存；Gemini `functionCall.id` present/absent 都合法，
  绝不合成 `tc_*`；Anthropic `tool_use.id`、OpenAI `tool_calls[].id` 缺失时
  `provider_call_id = None`（不伪造）。
- `native_to_wire(msg: &NativeMessage<UnboundToolCall>) -> Result<Value,
  ProjectionError>`：same-provider exact replay——要求 `msg.provider_origin ==
  Some(该 provider)`，带 replay_state 的 part 原样回写；否则 `WrongOrigin`。
- `project_cross_provider(msg, target) -> NativeMessage<UnboundToolCall>`：只读
  semantic；`provider_origin = Some(target)` 但所有 `replay_state = None`、
  `provider_call_id = None`；partial reasoning 不属于 completed conversation，
  跨 provider 不重放 source reasoning（reasoning 作为 neutral semantic 保留在
  semantic digest，但 wire 输出由 target provider 的自然表达决定——#142 只保证
  A→B 不泄漏 opaque state，B→A 由保存的原 A message 恢复）。

Fixtures（`crates/lattice-llm/tests/fixtures/projection/`，official wire JSON，
逐字节固定、可审查、不访问网络）：

```text
openai_assistant_tool_calls.json      # 两个 tool_calls 有序、真实 call_ id
openai_assistant_no_id.json           # 缺失 id 的负例
anthropic_thinking_tool_use.json      # thinking + signature + tool_use 有序
anthropic_text_only.json
gemini_thought_signature.json         # thought part + thoughtSignature + functionCall
gemini_function_call_no_id.json       # 无 id，禁止合成 tc_*
```

测试断言（对应 acceptance）：part order 保持；real/missing provider ID；
signature/thought 状态 roundtrip；same-provider replay wire-equal；A→B 后
`replay_state_digest` 变且不含 A 的任何 opaque 字节；B→A 恢复原 A native
message（`replay_state_digest` 相等）。

`lattice-llm/Cargo.toml` 增加 `lattice-conversation = { workspace = true }`
（真实边，AGENTS graph delta 落地）。production codec（`providers/*.rs`）零改动。

### 3.4 ownership 迁移（G3）

- `lattice-session/src/graph.rs`：`GraphMessage`/`GraphToolCall`/`HeadTranscript`
  struct 定义删除，改为 `pub use lattice_conversation::{GraphMessage, GraphToolCall,
  HeadTranscript};`；`TurnUsage` → `pub use lattice_conversation::TokenUsage as
  TurnUsage;`。graph.rs 内其余 store 语义（TurnCommit 等）不动。serde 形状不变
  → 既有 commit payload 兼容。
- `lattice-session/Cargo.toml`：`+ lattice-conversation`。
- `lattice-agent`：`use lattice_session::{GraphMessage, …}` →
  `use lattice_conversation::{…}`；Cargo.toml `- lattice-session /
  + lattice-conversation`。`TurnUsage` 引用处改 `TokenUsage`（或
  `use lattice_conversation::TokenUsage as TurnUsage;` 局部 alias 保持最小 diff）。
- `lattice-agent` 的 widened conversion：`lattice_llm::TokenUsage`(u32) →
  conversation `TokenUsage`(u64) 的 checked widening 保持原位（agent_loop/mod.rs
  现有累加点），不新增逻辑。
- `lattice-api`/`lattice-cli` 继续经 `lattice_session::` re-export 消费，零改动
  （re-export 就是为了这条；若 clippy 报 unused 再补）。
- workspace `Cargo.toml`：`members`/`workspace.dependencies` 注册
  `lattice-conversation`。

### 3.5 静态边界自查（ acceptance “不接 production” ）

- `git diff main --stat` 不得出现 `crates/lancet-{dispatch,native,cep,audit,runtime}/src/`；
- `grep -rn "lattice_session" crates/lattice-agent/src crates/lattice-agent/tests`
  必须为零（tests 也改）；
- `cargo tree -p lattice-agent --prefix none | grep lattice-session` 为空。

### 3.6 文档

- root `AGENTS.md`：dependency graph 执行 ADR 0005 delta 中 #142 部分
  （`ADD lattice-llm -> lattice-conversation`、`ADD lattice-agent ->
  lattice-conversation`、`DEL lattice-agent -> lattice-session`、`ADD
  lattice-session -> lattice-conversation`；`lattice-session -> lancet-protocol`
  与 `lattice-store` 边属 #143/#144，不写）；crate roles 表加
  `lattice-conversation`；并删去"两个 ADR 0005 crate 缺席"段落中已落地的
  `lattice-conversation` 一句。
- `crates/lattice-conversation/AGENTS.md`（新建：所有权、不变量、focused
  verification）；`crates/{lancet-protocol,lattice-llm,lattice-agent,lattice-session}/AGENTS.md`
  各自补一段。
- `docs/ARCHITECTURE.md` 若有 crate 清单则同步。
- `FIX.md` 落地后追加 #142 条目（issue id、问题、修复摘要、验证命令、changed files）。

## 4. 测试计划（focused → ci）

1. `cargo test -p lancet-protocol`（identity/address/digest 单测随源文件）。
2. `cargo test -p lattice-conversation`（batch validation、digests、limits、usage）。
3. `cargo test -p lattice-llm provider_projection`（fixtures）。
4. `cargo test -p lattice-agent -p lattice-session -p lattice-api`（迁移回归）。
5. `cargo build --workspace --exclude noop-echo`（memory #16 的已知排除）。
6. `cargo xtask ci`（全绿才算完成；含 fmt/clippy/boundary gates）。

## 5. 明确不做（#142 边界）

- 不生成 `InvocationId`/`TurnId`（TurnCoordinator 属 #139-B）；不提供任何
  `new_v4`/random 路径。
- 不改 production provider codec、JSON-RPC、transport、events、cancellation、
  SQLite、operator Adapter、WorkspaceHistory、dispatch；不删 numeric `call_id`。
- 不实现 PendingTurn/ResultDraft/InvocationStore/reserve_batch（#144/#139-A）。
- 不统一 `lattice_llm::TokenUsage`（#139-B）。
- 不动 `.gitignore`；新源文件带 workspace copyright/SPDX header。
