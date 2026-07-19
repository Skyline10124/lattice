# #5 实现计划：WASM Component Tool 基础 + Cell effect 模型贯通

- 票：<https://codeberg.org/AstRin/LATTICE/issues/5>（`ready-for-agent`，minor-revision 遗留点见 §9）
- 事实源优先级：issue 正文（2026-07-16 按 ADR 0004 重写）> ADR 0004 > `docs/specs/2026-07-01-issue5-effect-model-toolctx-design.md`
  （spec 的逻辑模型继续有效：Cell 核、通道模型、ToolHost 面、fail-closed 供需校验；
  spec §6.7 "v1 = 进程内 trait" 的分代方案已被 ADR 0004 取代——本票直接落 WASM Component）。
- 前置依赖：#4 ✅、#11 ✅、#15 ✅、#40 ✅。
- 维护者既定顺序：#23（测试收口）应先于本票开工（roadmap #16 评论 7/4、7/5）。

## 0. 现状锚点（已核验 2026-07-19）

- catalog 恰为 5 个 agent 方法：`observe.workspace`/`mutate.create`/`mutate.edit`/`fetch.web`/`session.todo_write`。
- 旧 effect 模型仍在：`lancet-protocol/src/effect_types.rs:22,81,97,110,121`
  （`ImpactKind`/`EffectScopeKind`/`OverlayBehavior`/`FailureSemantics`/`EffectDecl`）。
- 注册面被 `CatalogMethod` 绑死：`lancet-dispatch/src/registry.rs:118`；
  agent adapter 硬编码 match：`lattice-api/src/turn/cep.rs:106-107`。
- workspace 无 `wasmtime`/`wit-bindgen`/`inventory` 依赖；无 `lancet-runtime`、无 `lattice-tool-prelude`。
- `xtask` 已有 `ci`/`harness-boundary`/`lancet-boundary` 子命令，codegen 可挂新子命令。
- 仓库无 `rust-toolchain.toml` —— 确定性构建前置条件，见 §9。

## 1. 交付物总览

| 新增 | 说明 |
| --- | --- |
| `crates/lancet-runtime` | Component engine（wasmtime）、embedded inventory、composition gate、ToolHost 宿主面 |
| `crates/lattice-tool-prelude` | guest 侧 generated bindings 转出口、descriptor/Cell helpers、Component export glue |
| `tools/noop/` | no-op Source Tool：`manifest.toml` + guest crate + generated/ 产物 |
| `xtask tool-codegen` | manifest → WIT/descriptor/LLM projection/hash/inventory registration 的确定性生成器 |
| `lancet-protocol` 新类型 | Cell effect 封闭核（add-only，不删旧类型） |

不删除 `lancet-native`/`lancet-dispatch`/`lancet-catalog`（#12 收口）；不迁移 5 个 production Tool（#7–#10）。

## 2. 切片 A：`lancet-protocol` Cell effect 核

新模块 `cell.rs`（与 `effect_types.rs` 并存，旧类型本票不动——production handler 仍在用，删除归 #6/#12）：

- `CtxOp { Alloc, Free, Mutate }`、`ExtImpact { None, Read, Write }`（封闭枚举）。
- `Trust { In, Ex }`：只能显式声明 `in`，缺省 = `ex`（fail-closed）。
- `ScopeDecl { name, trust, channel }`、`ChannelKind { Workspace, Network, Session }`。
- `Cell`（`ctx_op × ext_impact × scope`）+ cell-key 字符串形式 `"ctx:ext:scope:in|ex"`，parse/format 往返。
- `Capability { name, cell }`：工具侧定义、一 cell 可多 capability（alias）。
- `ToolDescriptor { name, version, description, params_schema, result_schema, scopes, capabilities, required_channels, artifact: ArtifactIdentity, abi_version }`
  —— Tool Descriptor 唯一 authority 的宿主表示；含 `description`+`version`（复审第 4 条）。
- `ToolOutput { result_cell, value }`。
- 校验：`capability.cell.scope` 必须引用已声明 scope；`required_channels` ⊆ scopes 派生通道集（无独立 channels 字段，spec §11.3）。

测试：fail-closed `in`/`ex`；cell-key 往返；descriptor 引用未声明 scope 报错；一次调用多 cell 表示。

## 3. 切片 B：manifest + 确定性 codegen + WIT + prelude + no-op tool

### 3.1 Source Tool manifest（`tools/noop/manifest.toml`）

```toml
[tool]        # name/version/description/abi-version
[params]      # structured JSON schema
[result]
[[scopes]]    # name + trust("in"|"ex") + channel
[[capabilities]] # name + cell = "ctx:ext:scope"
[artifact]    # sha256 + 构建工具链指纹
```

### 3.2 `xtask tool-codegen`

从 manifest 生成（全部带 `@generated` 头、可重复生成、byte-identical）：

1. WIT world `lattice:tool-host@0.1.0`：guest export `run(params-json) -> result-json`；
   host import 仅限声明通道 + `events` + `cancellation`。**通道句柄按 capability 绑定
   （handle-per-capability）**——#6 落 `ObservedEffect { capability, cell, target }` 精确归因时
   无需 breaking WIT 变更。本票 world 只含 workspace 只读最小子集（tracer 所需），#7–#10 按版本化 world 扩展。
2. host descriptor `const`（`ToolDescriptor`）。
3. LLM projection metadata（tool definition JSON）。
4. `artifact.sha256` 元数据。
5. inventory registration 模块：`descriptor` + `include_bytes!(noop.wasm)` + ABI 版本 + WIT import 清单。

确定性门：`xtask ci` 内重跑 codegen + `git diff --exit-code`；manifest↔descriptor drift 测试
（#7–#11 验收依赖的机制，前移进本票，spec §9）。

### 3.3 `lattice-tool-prelude` + no-op guest

- prelude：re-export generated guest bindings、Cell/descriptor helpers、export glue；不暴露 host internals。
- `tools/noop`：**`wasm32-unknown-unknown` target + wit-bindgen**（不用 `wasm32-wasip2`：其 std 经
  wasi-libc 自动引入 `wasi:cli`/`wasi:filesystem`/`wasi:random` 等 imports，会触发本票自己的零
  ambient WASI import 门；纯计算 + Tool Host imports 下 import 白名单恰好等于 Tool Host world，
  门的实现变得平凡）。经 prelude 实现 `run`，声明一个 workspace `in` scope +
  单 capability，调用一次 workspace 只读 import 后返回 structured result（打穿完整 tracer）。
  #7–#10 若需 clocks/random，以版本化 host import 显式供给，不回退 wasip2。prelude 封装该选择。
- 构建产物 `.wasm` + sha256 入库（committed，视作生成物）；`xtask` 提供 rebuild-verify（依赖 §9.1 pin）。

## 4. 切片 C：`lancet-runtime` Component engine + composition gate

- wasmtime Component engine：
  - 无 ambient WASI：不 link filesystem/socket/env/process/secret；只 link 版本化 Tool Host imports。
  - 只实例化 embedded artifacts（inventory 收集的 generated registration）；无目录扫描/动态链接/native `&dyn Tool` 收集。
  - limits：`StoreLimits`（memory）+ epoch deadline（timeout/cancellation）+ fuel；
    trap/timeout/exhaustion → 结构化 `CepError`，server 不退出。
- `ToolHost`（call-scoped）：按准入 capability 发通道句柄；本票实现 workspace 只读通道
  （复用现有 guard/policy 路径）+ `events()` + `cancellation()`；不暴露
  policy_hook/raw budget/effect_ledger/authorized_region（spec §6.3）。
- composition gate（启动时、serve 前，逐项 fail closed）：
  missing / duplicate / unapproved name（inventory 集 ≠ 组合清单，双向都失败，spec §8.1）/
  descriptor drift / artifact hash mismatch / WIT import drift / unsupported ABI version。
- 启动后 catalog immutable：不存在运行时注册 API。

## 5. 切片 D：raw registry + descriptor 驱动 adapter + round-trip

- `MethodRegistry` 增加 raw 注册路径：按 `ToolDescriptor` 接受 dyn handler
  （`registry.rs:118` 的 `CatalogMethod` 泛型无法表达非编译期方法名）；与现有
  `register_method<M>` 过渡并存。
- `lattice-api::CepExecutorAdapter`：Component tool 走 descriptor 驱动 dispatch；
  5 个 native 方法保持硬编码 match 至 #7–#10 逐票替换（`cep.rs:106` 8 臂 match 不在本票展开重写）。
- LLM tool definitions：Component tool 只从 generated descriptor inventory 投影；
  断言无手写 method match/schema shim。
- audit：复用 transport 的 audit start/finalize 生命周期 + dispatch 后 effect 校验；
  no-op 调用产出 cell 归因的 effect + durable audit 行（audit 记 cell 不记 capability 名，spec §6.3）。
- round-trip：`CepClient` typed call → no-op Component → structured result。

## 6. 测试矩阵（验收逐条映射）

| 验收项 | 测试 |
| --- | --- |
| codegen determinism | 双跑 diff + golden 文件 |
| manifest 唯一 authority / drift | manifest↔descriptor/WIT/hash 不一致 → 失败 |
| import denial | 未声明 import 的 component 过不了 gate；WASI fs/socket import → link 失败 |
| registration gates | 7 种失败模式各一例，全部 fail closed |
| round-trip | CepClient → result + cell effect + events + audit 行断言 |
| limits | trap / memory 超限 / deadline / cancellation → 结构化错误，server 存活 |
| no-ambient-authority | component 内尝试 WASI 系统调用的负例 |
| catalog immutable | 启动后无注册缝；inventory 集 == 组合清单（恰好相等） |
| LLM projection | 投影列表恰含 5 native + noop（测试组合），无手写 shim |
| capability↔通道绑定 | 未声明 network scope 的工具取 `network()` 失败；alias 单独授权成功 |

验证命令：

```
cargo test -p lancet-protocol -p lancet-dispatch -p lancet-runtime -p lattice-tool-prelude -p lattice-api
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask ci   # 含 codegen determinism 门
```

## 7. PR 切片顺序

A（protocol 类型）→ B（manifest/codegen/WIT/prelude/noop）→ C（runtime engine + gate）→ D（adapter + round-trip）。
每片独立可验证、独立可 revert（复审第 1 轮建议的四切片结构）。

## 8. 边界（本票不做）

- 不迁移 5 个 production Tool（#7–#10）；不删 `lancet-native`/`register_native_*`（#12）。
- 不动 `ImpactKind`-keyed authority 机器（#6 R7b）；旧 effect 类型 add-only 并存。
- 无 `ContextStore`/`Free` 执行机制（PRD R10，仅枚举变体）。
- 无 Tool Bundle install/list/uninstall CLI 与 release gate（#13）。
- network/session 通道 host 实现（#10/#7）；WIT world 版本化预留。

## 9. 决议记录（原"待决点"，全部已决 2026-07-19）

1. **toolchain pin + 产物模式**：首个 commit 落 `rust-toolchain.toml`（pin rustc 1.96.0 +
   `wasm32-unknown-unknown` target）+ `--remap-path-prefix`。`.wasm` 作为生成物入库，普通
   `cargo build/test` 只 `include_bytes!` 现成产物（贡献者无需 wasm 工具链）；`xtask ci` 增
   rebuild-and-compare lane，**先 warn-only**，验证 bit-for-bit 可复现后升级为硬门。
2. **budget/measure 归属与 tokenizer 选型**：本票只做 `result_cell` ∈ descriptor cell 集校验；
   `measure()`/context 记账推迟到 #6。canonical 尺 = **o200k_base，经 `bpe-openai` crate**
   （纯 Rust，词表编译进二进制，零运行时资产），取代原 DeepSeek HF 资产方案；spec 决议 8 已修订。
3. **capability 归因粒度**：本票按 "observed ⊆ declared 上界检查" 实现；但 WIT 形状现在就按
   handle-per-capability 设计（§3.2），#6 精确归因只加实现、不动接口。
4. **wasmtime 选型**：当前 stable 主版本 **46.x**，`Cargo.toml` 精确 pin。deadline/cancellation 走
   epoch interruption（cancel 时 bump epoch），memory 走 `StoreLimits`，fuel 作粗粒度 CPU 上界。
   注意：wasmtime 46 MSRV = 1.94，workspace `rust-version = "1.88"` 需在 pin commit 同步升至 1.94；
   `xtask ci` 前置检查 wasm target 已安装。

## 10. 残余风险

- `.wasm` bit-for-bit 可复现性未经验证（依赖 wasm-tools/wit-bindgen 版本稳定）——rebuild lane
  warn-only 阶段收集数据，不阻塞切片 B。
- `bpe-openai` 含一方 build.rs（构建期解压词表）：不属 ADR 0004 "Tool Bundle 安装不执行第三方
  build code" 范畴，但若后续建 build-script 审计门需列入白名单。
- 切片 C（wasmtime 集成）是最高风险点：可在 #23 期间以 spike 分支先行验证
  unknown-unknown → component → 实例化链路，泄压后按序合入。
