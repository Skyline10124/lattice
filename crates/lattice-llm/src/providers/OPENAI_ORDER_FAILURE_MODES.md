# OpenAI 有序 parts 失败方式

本文件是 #223 R2 S3 按维护者要求保留的测试设计与执行证据，不另立规范。
依据：ADR 0006 §10、会话合同「观察与重连」及本次任务授权。
基线：`feat/r2-redb-store`，`71c6850c`。上一位执行者记录其开始时工作区干净；续做开始时已有 parser/projection 三个文件的未提交改动及本文件，原样备份后核验并继续。
测试接缝：`OpenAiSseParser::parse_chunk` 的公开事件与完成消息，以及 provider projection 的请求/跨 provider 投影。只使用真实解析和投影，不 mock 内部协作者。

## 实施前清单（12 条）

| 编号 | 失败方式 | 独立断言 |
| --- | --- | --- |
| F01 | reasoning → text → reasoning 被归并重排 | parts 为 R/T/R，实时 delta 合并后相同 |
| F02 | 同 chunk 多种字段的最终顺序与事件发出顺序不符 | 同 chunk 按现有 reasoning、text、calls 发出顺序累计；相邻同类合并 |
| F03 | text → ToolCall → text 丢失工具位置 | 工具在两段正文之间，不跨工具合并 |
| F04 | 稀疏乱序 index 被数值排序 | 调用按首次出现次序，index 不作为 parts 下标 |
| F05 | 同 index 多片段、延迟 name/ID 使占位移动或错配 | 初次无 name 时也占位，后续原位补齐；参数与 ID 正确 |
| F06 | finish/usage/[DONE] 导致提前或重复完成、usage 丢失 | usage 与 finish 同 chunk、usage 尾 chunk、无 usage 的 DONE 三条路径均只完成一次 |
| F07 | 空片段生成空 part 或打断合并 | 空 reasoning/text 不产事件与 part、不构成分隔 |
| F08 | 分段正文/reasoning 绕过累计上限 | UTF-8 字节累计，恰好上限接受，多一字节整条拒绝 |
| F09 | 交错调用绕过单调用或总量上限 | 同 index 恢复片段仍累计；不同调用各有额度；总量 16 MiB 复核 |
| F10 | 有序投影绕过整批调用验证 | 非法 JSON/重复真实 ID/调用数量超限均 MessageRejected，无 MessageComplete |
| F11 | 请求侧改变 Chat Completions wire | 合并 reasoning/content，工具顺序与 ID 保留，参数规范编码 |
| F12 | 跨 provider 泄漏状态或重排 | 去除 reasoning/调用 ID，其余语义相对次序保留；原消息不变 |

现有 agent 使用 AttemptAssembly 持续计量，projection 在完成边界复核；本切片不重做 agent 的计量模型。F08/F09 在 parser 完成边界验证，持续计量由现有 agent 回归保护。

## 执行证据

红阶段命令：`CARGO_BUILD_JOBS=8 cargo test -p lattice-llm --lib order_f -- --nocapture`。原实现实际运行 12 项：6 失败、6 通过；未改 golden。

| 编号 | 原实现结果 / 受控变异失败信号 |
| --- | --- |
| F01 | 红：`R:想再想,T:答`，应为 `R:想,T:答,R:再想` |
| F02 | 红：`R:R,T:AB,C:t`，应为 `T:A,R:R,T:B,C:t` |
| F03 | 红：`T:AB,C:t`，应为 `T:A,C:t,T:B` |
| F04 | 红：`third,second,first`，应为 `first,second,third` |
| F05 | 红：`T:between,C:second,C:observe.workspace`，应在第一处占位 |
| F06 | 原绿；临时去掉 completed guard，晚到 delta 非空，断言失败 |
| F07 | 红：`R:RS,T:AB`，应为 `T:AB,R:RS` |
| F08 | 原绿；临时不计正文，`expected limit AssistantContent` 失败 |
| F09 | 原绿；临时不计参数，`expected limit ToolCallArguments` 失败 |
| F10 | 原绿；临时只验证首调用，非法尾调用未拒绝，case 0 失败 |
| F11 | 原绿；临时每段清空请求正文，输出 B 而不是 AB，精确 wire 断言失败 |
| F12 | 原绿；临时不清除跨 provider 调用 ID，is_none 断言失败 |

每个变异单独执行 `CARGO_BUILD_JOBS=8 cargo test -p lattice-llm --lib order_fNN -- --nocapture`，均为 1 项实际失败，随后立即撤销。全部撤销后再次运行 `order_f`，恢复同样的 6 红 / 6 绿；当时 tracked diff 仅有新增测试，无生产修改。日志分别在本机 `/tmp/lattice-s3-red.log`、`/tmp/lattice-s3-mut-fNN.log`、`/tmp/lattice-s3-red-restored.log`（临时路径，不是长期产物承诺）。

### 续做核验与补证

- 前述红阶段及 F06/F08–F12 变异日志实际存在，内容与本表一致；续做没有将其冒称为本次新执行。原日志本身未保存源码快照，不能单靠它证明执行时的生产代码身份。
- 为补足该证据，先保存遗留 diff 到 `/tmp/s3-inherited.diff`、清单到 `/tmp/s3-inherited-failure-modes.md`；保留全部新增测试，临时还原三处生产代码，并与 `git show HEAD:<path>` 逐字节比对一致。再次执行相同红阶段命令，仍为 6 红 / 6 绿，日志 `/tmp/s3-resume-red.log`。随后恢复遗留实现，完整 diff 与备份逐字节相同；未使用 stash、未修改索引。
- 保留已有有序 parts 方案，修正未完成的 `call.started` → `slot.started` 引用。`src/lib.rs` 的既有 codec 边界测试曾真实失败（`/tmp/s3-resume-first-green.log`）；仅将 OpenAI 的检查标记更新为 projection 的完整调用 `crate::provider_projection::openai_parts_to_native(`，不删除断言或放宽其他 provider 检查。
- F07 补充两次独立受控变异：去掉空片段 guard，空 delta 无事件断言失败；取消相邻合并，得到 `T:A,T:B,R:R,R:S` 而非 `T:AB,R:RS`。命令均为 `CARGO_BUILD_JOBS=8 cargo test -p lattice-llm --lib order_f07 -- --nocapture`，每次实际 1 项失败，随即撤销。日志 `/tmp/s3-resume-mut-f07-empty.log`、`/tmp/s3-resume-mut-f07-merge.log`。
- 撤销变异并格式化后，相同 `order_f` 命令实际 12 项全部通过，日志 `/tmp/s3-resume-green.log`。没有修改 digest golden、fixture 或请求 wire 形状。

### 最终验证

以下命令串行执行，均设置 `CARGO_BUILD_JOBS=8`：

| 命令 | 实际结果 | 日志 |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 退出 0 | `/tmp/s3-resume-fmt.log` |
| `cargo clippy --workspace --all-targets -- -D warnings` | 退出 0 | `/tmp/s3-resume-clippy.log` |
| `cargo test -p lattice-llm` | 92 单元测试 + 34 projection 集成测试通过；doc-tests 0 | `/tmp/s3-resume-llm.log` |
| `cargo test -p lattice-agent` | 退出 0，但实际 0 测试，不作为行为验证通过证据 | `/tmp/s3-resume-agent.log` |
| `cargo test -p lattice-agent --features test-utils` | 因默认入口零测试而补跑；12 单元测试 + 5 streaming seam 测试通过 | `/tmp/s3-resume-agent-test-utils.log` |

四个 Rust 修改文件的主动 LSP 检查无诊断；`git diff --check` 通过。未修改 agent、docs、AGENTS.md、其他 crate 或 golden。按本次授权不运行已说明的宿主 guest 链接 build，不做产品观察/重连 E2E，也不声称完成 S2/S6 接线验证。本次为单 Agent 核对，没有独立审查、提交、推送或 GitHub 写操作。

## M1 复审修正：实施前失败方式

原 F09 只证明 parser 完成投影的复核，不能证明生产 driver 对交错调用的持续计量；上文持续计量覆盖的表述不适用于此形状。本次新增接缝为真实 `OpenAiSseParser::parse_chunk` → `ModelStepDriver::run`，不经过会重组完成消息的 fake client。事件按需解析，每个参数片段不超过 128 KiB。

| 编号 | 失败方式 | 独立断言 |
| --- | --- | --- |
| F13 | 交错续传将不同 index 的参数相加，误判合法调用超限 | A/B 各 700,008 字节，A 首片 → B 完整参数 → A 余片；driver Complete 且两条参数完整。覆盖连续及稀疏反序 index、参数先于 metadata |
| F14 | 其他调用 Start 重置已有 index 的额度，延迟超限并误分类为 StreamError | A 先发 600,000 字节，插入 B 后继续 A；恰好 1 MiB 仍接受，多一字节的片段立即返回 ToolCallArguments 类型化超限，无完整消息且不读取后续片段；覆盖 name/ID 延迟到达 |

共享总量保护另在同一接缝核验：正文、reasoning 与交错调用合计恰好 16 MiB 时继续，多一字节立即返回 ModelAttemptTotal，不能因逐调用计量而分散总额度。

### M1 红阶段（本次实际执行）

命令：`CARGO_BUILD_JOBS=8 cargo test -p lattice-agent --features test-utils --test openai_stream_limits -- --nocapture`。在未修改 `model_step.rs` 的生产计量实现时，实际 3 项运行、3 项失败（退出 101）：

- F13：实际 `ToolCallArguments { limit: 1048576, actual: 1093224 }`，预期 `Complete`。
- F14：实际 `Interrupted(StreamError)`，预期越界片段立即返回 `ToolCallArguments { limit: 1048576, actual: 1048577 }`。
- 共享总量回归：实际提前返回 `ToolCallArguments { actual: 1179648 }`，预期 `ModelAttemptTotal { limit: 16777216, actual: 16777217 }`。

日志：`/tmp/r2-s3-m1/red-behavior.log`。首次测试编译曾因 parser 模块私有及误用参数访问方法失败（`/tmp/r2-s3-m1/red.log`），不把编译失败当作行为红灯。为真实跨 crate 接缝添加 `test-utils` 下的 parser 重导出及 agent feature 转接；parser 行为和事件结构未改。F13/F14 的后续 metadata 变体在首个断言失败后未执行，须在绿阶段核验。

### M1 修正与验证

driver 按事件 index 保留独立 `AttemptAssembly`，从首个参数 delta 开始计量，不依赖 name/ID 或 Start。原共享 assembly 仍累计正文、reasoning 和所有参数；只重置它的单片段参数计数，单调用累计限制由对应 index 的 assembly 执行。没有改变限额、完成投影或错误类型。

同一回归命令绿阶段实际 3 项通过，含 metadata 延迟变体；日志 `/tmp/r2-s3-m1/green.log`。以下指定验证实际按表序串行执行，均设置 `CARGO_BUILD_JOBS=8`：

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 退出 0；首次检查仅新测试的两处格式失败，修正后重跑通过 | `/tmp/r2-s3-m1/fmt.log`（首次为 `fmt-first.log`） |
| `cargo clippy --workspace --all-targets -- -D warnings` | 退出 0 | `/tmp/r2-s3-m1/clippy.log` |
| `cargo test -p lattice-llm` | 92 单元 + 34 集成测试通过；doc-tests 0 | `/tmp/r2-s3-m1/llm.log` |
| `cargo test -p lattice-agent --features test-utils` | 12 单元 + 3 新边界回归 + 5 既有 streaming seam 测试通过；doc-tests 0 | `/tmp/r2-s3-m1/agent.log` |

补充核验：`CARGO_BUILD_JOBS=8 cargo clippy -p lattice-agent --all-targets --features test-utils -- -D warnings` 退出 0，覆盖 feature 下的新测试（`/tmp/r2-s3-m1/clippy-test-utils.log`）；`git diff --check` 通过。主动 LSP 检查 3 个 Rust 文件时有 1 个超时未确认、1 个仅有 feature 未启用提示，不据此声称全部 LSP 干净；以上实际 Cargo 编译、lint 与测试结果独立成立。

仅修改 agent 计量/测试及两个 crate 的测试 feature 接线、LLM 测试用重导出和本清单；保留原 S3 parser/projection 改动，不涉及其他 crate、治理文件、Git 写操作或委派。未修复报告 M2，未执行产品 E2E 或远程 CI；结果不代表这些范围已验证。
