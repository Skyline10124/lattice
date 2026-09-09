# #27 切片 C：`lancet-runtime` Component engine、ToolHost 与 composition gate

- 父计划：`2026-07-19-issue27-wasm-component-tool-substrate.md` §4
- 前置：切片 A（Cell/descriptor wire types）+ B（generated WIT、descriptor JSON、artifact、inventory registration）。
- 本切片只建 Component 承载和最小 tracer ToolHost；不接 LLM/CEP registry（切片 D）。

## C.1 crate 与依赖边界

新增 `crates/lancet-runtime`，依赖方向：

```text
lancet-runtime -> lancet-protocol, lancet-dispatch(过渡事件/取消), inventory,
                  wasmtime, jsonschema, sha2, serde_json, tokio
lancet-native  -> lancet-runtime（切片 D：把 call-scoped evidence 接入旧 audit lifecycle）
```

`lancet-runtime` **不依赖** `lancet-native`/`lancet-api`/任何 LATTICE crate；不存在环。
# 12 吸收 `lancet-dispatch` 后删过渡依赖。

`Cargo.toml` 精确 pin，明确不启用 WASI：

```toml
[dependencies]
wasmtime = { version = "=46.0.1", default-features = false, features = [
  "async", "component-model", "cranelift", "runtime", "std", "wit-parser"
] }
inventory = "=0.3.24"
lancet-protocol = { path = "../lancet-protocol" }
lancet-dispatch = { path = "../lancet-dispatch" }
jsonschema = "=0.48.1"
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
tokio = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
wat = "1"
```

没有 `wasmtime-wasi`/`wasi-common` 依赖；这比「不调用 WASI linker helper」更强。

## C.2 inventory registration：只存静态字节，不伪造不可 const 的 descriptor

`ToolDescriptor` 含 owning values，不能做 Rust `const`。Tool registration 只提交 name + bytes；
trusted host composition 另有 private expected identity（digest/ABI/imports），避免 registration 自报
hash 后自证正确：

```rust
/// Tool-side discovery record. It is not an approval record.
pub struct EmbeddedTool {
    pub name: &'static str,
    pub descriptor_json: &'static [u8],
    pub component_bytes: &'static [u8],
}
inventory::collect!(&'static EmbeddedTool);

// Private to lancet-runtime; generated only by trusted host composition.
pub(crate) struct ExpectedToolIdentity {
    pub name: &'static str,
    pub descriptor_sha256: [u8; 32],
    pub artifact_sha256: [u8; 32],
    pub abi_version: u32,
    pub imports: &'static [&'static str],
}

// Tool-side generated registration.
pub static NOOP_ECHO: EmbeddedTool = EmbeddedTool {
    name: "noop.echo",
    descriptor_json: include_bytes!("descriptor.json"),
    component_bytes: include_bytes!("noop_echo.component.wasm"),
};
inventory::submit!(&NOOP_ECHO);

// Host-side generated identity exists only in fixed test fixture for #27;
// production EXPECTED_COMPONENTS is empty until a production Tool migrates.
const NOOP_EXPECTED: ExpectedToolIdentity = ExpectedToolIdentity {
    name: "noop.echo",
    descriptor_sha256: [/* host-pinned */],
    artifact_sha256: [/* host-pinned */],
    abi_version: 1,
    imports: &["lattice:tool-host/host-control@0.1.0"],
};
```

crate-local gate 单测直接传 synthetic registrations/expected identities 制造负例；production public
构造器只能读取 inventory + private generated `EXPECTED_COMPONENTS`。

## C.3 closed composition gate

```rust
const SUPPORTED_ABI: u32 = 1;

pub(crate) struct ClosedCatalog {
    tools: std::collections::BTreeMap<String, CompiledTool>,
}

impl ClosedCatalog {
    pub(crate) fn compose(
        engine: &wasmtime::Engine,
        registrations: impl IntoIterator<Item = &'static EmbeddedTool>,
        expected_identities: &'static [ExpectedToolIdentity],
    ) -> Result<Self, CompositionError> {
        let expected: std::collections::BTreeMap<_, _> = expected_identities
            .iter().map(|identity| (identity.name, identity)).collect();
        let mut tools = std::collections::BTreeMap::new();
        for reg in registrations {
            let identity = expected.get(reg.name)
                .ok_or_else(|| CompositionError::UnapprovedName(reg.name.into()))?;
            if tools.contains_key(reg.name) {
                return Err(CompositionError::DuplicateName(reg.name.into()));
            }
            // Digests are host-pinned, not supplied by the registration.
            verify_sha256(reg.descriptor_json, identity.descriptor_sha256)
                .map_err(CompositionError::DescriptorDrift)?;
            verify_sha256(reg.component_bytes, identity.artifact_sha256)
                .map_err(CompositionError::ArtifactHashMismatch)?;

            let descriptor: ToolDescriptor = serde_json::from_slice(reg.descriptor_json)
                .map_err(CompositionError::DescriptorJson)?;
            descriptor.validate().map_err(CompositionError::DescriptorInvalid)?;
            if descriptor.name != reg.name {
                return Err(CompositionError::DescriptorNameDrift {
                    registration: reg.name.into(), descriptor: descriptor.name,
                });
            }
            if descriptor.abi_version != identity.abi_version || identity.abi_version != SUPPORTED_ABI {
                return Err(CompositionError::UnsupportedAbi(descriptor.abi_version));
            }
            if descriptor.artifact.sha256 != encode_sha256_hex(identity.artifact_sha256) {
                return Err(CompositionError::DescriptorArtifactDrift(reg.name.into()));
            }
            let component = wasmtime::component::Component::new(engine, reg.component_bytes)
                .map_err(CompositionError::Compile)?;
            let actual_imports: std::collections::BTreeSet<_> = component
                .component_type()
                .imports(engine)
                .map(|(name, _)| name.to_string())
                .collect();
            let declared_imports = identity.imports.iter().map(ToString::to_string).collect();
            if actual_imports.iter().any(|name| name.starts_with("wasi:")) {
                return Err(CompositionError::AmbientImport(actual_imports));
            }
            if actual_imports != declared_imports {
                return Err(CompositionError::ImportDrift {
                    declared: declared_imports,
                    actual: actual_imports,
                });
            }
            validate_tool_api_export(&component, engine)?;

            // `tool-api` result sink is the non-operational Session adapter.
            // Domain channels are supplied only by known ToolHost imports;
            // host-control never grants a domain channel.
            let mut supplied_channels = channels_for_domain_imports(&actual_imports)?;
            supplied_channels.insert(ChannelKind::Session);
            let required_channels = descriptor.required_channels();
            if !required_channels.is_subset(&supplied_channels) {
                return Err(CompositionError::MissingChannelAdapters {
                    missing: required_channels.difference(&supplied_channels).copied().collect(),
                });
            }
            // Invalid JSON Schemas fail startup, not the first call.
            let params_validator = jsonschema::validator_for(&descriptor.params_schema)
                .map_err(CompositionError::ParamsSchema)?;
            let result_validator = jsonschema::validator_for(&descriptor.result_schema)
                .map_err(CompositionError::ResultSchema)?;
            let descriptor = std::sync::Arc::new(descriptor);
            tools.insert(descriptor.name.clone(), CompiledTool {
                descriptor, component, params_validator, result_validator,
            });
        }

        let missing: Vec<String> = expected.keys()
            .filter(|name| !tools.contains_key(**name))
            .map(|name| (*name).to_string())
            .collect();
        if !missing.is_empty() {
            return Err(CompositionError::MissingNames(missing));
        }
        Ok(Self { tools })
    }

    pub(crate) fn get(&self, name: &str) -> Option<&CompiledTool> { self.tools.get(name) }
    pub(crate) fn descriptors(&self) -> impl Iterator<Item = &ToolDescriptor> {
        self.tools.values().map(|tool| tool.descriptor.as_ref())
    }
    // No insert/register/load/path API: immutable after `compose`.
}
```

`ExpectedToolIdentity` 只能来自 private build-generated composition，不是 profile `enabled_tools`。
production public constructor 不接受 registrations/names；test helper 也只能构造固定 no-op fixture：

```rust
impl ComponentRuntime {
    pub fn from_embedded() -> Result<Self, CompositionError> {
        Self::compose_private(
            inventory::iter::<&EmbeddedTool>,
            generated::host_composition::EXPECTED_COMPONENTS,
        )
    }

    #[cfg(feature = "test-utils")]
    #[doc(hidden)]
    pub fn noop_fixture() -> Result<Self, CompositionError> {
        // Fixed generated bytes + fixed expected name; no caller-controlled
        // registrations, paths, bytes, handlers, or approval set.
        Self::compose_private(
            [generated::noop_fixture::registration()],
            &[generated::noop_fixture::expected_identity()],
        )
    }

    fn compose_private(
        registrations: impl IntoIterator<Item = &'static EmbeddedTool>,
        expected_identities: &'static [ExpectedToolIdentity],
    ) -> Result<Self, CompositionError> {
        let engine = build_engine().map_err(CompositionError::Engine)?;
        let catalog = ClosedCatalog::compose(&engine, registrations, expected_identities)?;
        Ok(Self::new(engine, catalog))
    }
}
```

`test-utils` 非默认 feature 只暴露零参数 fixed fixture，不暴露 generic composition。

## C.4 engine limits + epoch ticker（精确取消，不全局误伤）

共享 engine 用一个 ticker 周期性 `increment_epoch()`；**取消时不直接 bump engine epoch**
（那会推进所有 Store 的 epoch）。每个 Store 的 callback 查看自己的 cancellation/deadline，
不相关 Store 返回 `YieldCustom(1, tokio::task::yield_now())` 继续。

```rust
fn build_engine() -> anyhow::Result<wasmtime::Engine> {
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true)
        .async_support(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    wasmtime::Engine::new(&c)
}

struct StoreState {
    call: ComponentCallContext,
    limits: RecordingLimits, // wraps StoreLimits; records which limit rejected growth
    deadline: std::time::Instant,
    abort: Option<AbortReason>,
}

fn configure_store(store: &mut wasmtime::Store<StoreState>, limits: &ExecutionLimits) -> anyhow::Result<()> {
    store.limiter(|state| &mut state.limits);
    store.set_fuel(limits.fuel)?;
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(|mut cx| {
        if cx.data().call.cancellation.is_cancelled() {
            cx.data_mut().abort = Some(AbortReason::Cancelled);
            return Ok(wasmtime::UpdateDeadline::Interrupt);
        }
        if std::time::Instant::now() >= cx.data().deadline {
            cx.data_mut().abort = Some(AbortReason::Deadline);
            return Ok(wasmtime::UpdateDeadline::Interrupt);
        }
        Ok(wasmtime::UpdateDeadline::YieldCustom(
            1,
            Box::pin(tokio::task::yield_now()),
        ))
    });
    Ok(())
}

fn spawn_epoch_ticker(engine: wasmtime::Engine, stop: tokio::sync::watch::Receiver<bool>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(5));
        loop {
            tokio::select! {
                _ = tick.tick() => engine.increment_epoch(),
                _ = wait_for_stop(stop.clone()) => break,
            }
        }
    });
}
```

`RecordingLimits` 以 `StoreLimitsBuilder::memory_size/tables/memories/instances` 为底，
在拒绝增长时写 `AbortReason::Memory/Instances`，使错误映射不依赖字符串匹配。

## C.5 最小 ToolHost：control-only，不抢 #28 admission

#27 no-op 只导入 `host-control`（event + cooperative cancellation），**不导入任何
workspace/network/session domain operation**。理由：#28 才统一 capability admission；如果 #27
先做 `session_context::open` 且只检查“descriptor 已声明”，就会形成声明即授权的旁路。

```rust
pub struct ComponentCallContext {
    pub call_id: u64,
    pub turn_id: String,
    pub events: lancet_dispatch::EventSink,
    pub cancellation: lancet_dispatch::Cancellation,
    pub evidence: CellEvidenceLedger,
}

// wasmtime::component::bindgen!({ path: "wit", world: "tool", async: true });
impl bindings::lattice::tool_host::host_control::Host for StoreState {
    async fn cancelled(&mut self) -> wasmtime::Result<bool> {
        Ok(self.call.cancellation.is_cancelled())
    }
    async fn emit_event(&mut self, payload: String) -> wasmtime::Result<()> {
        // Convert generated ToolHost payload to existing typed/best-effort EventSink.
        emit_component_event(&self.call.events, payload).await?;
        Ok(())
    }
}
```

`(Alloc,None,session/in)` evidence 由 Tool 的 **result CellRef** 在 call 返回后产生，不由
domain host operation 产生；host 从 descriptor 派生 trust。这样仍贯通 ToolHost control、event、
cancellation、Cell、audit，同时不存在任何领域 authority。#28 落 admission 后，#29–#32 再以
handle-per-capability WIT 形状增加真实通道。

## C.6 call + result/effect/error validation

```rust
pub async fn call(
    &self,
    name: &str,
    params: serde_json::Value,
    ctx: ComponentCallContext,
) -> Result<serde_json::Value, CepError> {
    let tool = self.catalog.get(name).ok_or_else(|| CepError::method_not_found(name))?;
    tool.params_validator.validate(&params).map_err(schema_mismatch)?;

    let mut store = self.new_store(ctx)?;
    let (bindings, _) = bindings::Tool::instantiate_async(
        &mut store, &tool.component, &self.linker,
    ).await.map_err(|e| map_component_error(&store, name, e))?;
    let raw = bindings.lattice_tool_tool_api().call_run(
        &mut store, &serde_json::to_string(&params)?,
    ).await.map_err(|e| map_component_error(&store, name, e))??;
    // WIT output uses a trust-free 3-segment CellRef. Guest cannot claim `in`.
    let result_cell = parse_cell_ref(&raw.result_cell).map_err(schema_mismatch)?;
    let result_value: serde_json::Value = serde_json::from_str(&raw.value_json)
        .map_err(schema_mismatch)?;

    if !tool.descriptor.capabilities.iter().any(|cap| cap.cell == result_cell) {
        return Err(effect_violation(name, &result_cell));
    }
    tool.result_validator.validate(&result_value).map_err(schema_mismatch)?;
    let trust = tool.descriptor.trust_of(&result_cell.scope);
    store.data().call.evidence.record(result_cell, trust);
    Ok(result_value)
}
```

结构化错误：在 `lancet-protocol` execution family 新增 `ToolTrap`、`ToolTimeout`、
`ToolResourceExhausted`、`CallCancelled` code（先改 wire mapping/round-trip tests，再在此消费）。
映射优先级：`abort=Cancelled` → CallCancelled；Deadline → ToolTimeout；RecordingLimits/fuel=0 →
ToolResourceExhausted；其余 Wasmtime trap → ToolTrap。每个错误 data 含 `method`、`resource`
或 trap category，**不暴露 guest backtrace/host path/secret**。

## C.7 focused tests

- gate 负例逐项覆盖：missing / duplicate / unapproved / descriptor digest+name+artifact drift /
  component artifact hash / import drift / unsupported ABI / missing derived channel adapter；
  `wasi:filesystem` import → `AmbientImport`。
- trap：guest `unreachable`；timeout/cancel：infinite loop + epoch；fuel=1；memory grow 超限；
  每例后再调用正常 no-op，证明 server/engine 存活。
- guest result CellRef 不在 descriptor capability cells → EffectViolation；4 段 guest key（自带 trust）→ SchemaMismatch；result schema mismatch → SchemaMismatch。
- `ClosedCatalog` 无 mutation API（compile-time surface review）且 descriptors 与 expected set 恰等。

## 验收

```text
cargo test -p lancet-runtime -p lancet-protocol
cargo clippy -p lancet-runtime --all-targets -- -D warnings
cargo tree -p lancet-runtime | grep -E "wasmtime-wasi|wasi-common"  # 应为空
```
