// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Component runtime — the wasmtime engine, closed catalog, linker, and
//! call execution surface.
//!
//! The runtime is composed at startup from inventory registrations +
//! host-pinned identities. After composition, the catalog is immutable.
//! Each call gets a fresh `Store` with per-call limits, fuel, and epoch
//! deadline. The shared engine's epoch ticker runs in a background task.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use crate::control::{Cancellation, EventSink};
use lancet_protocol::canonical::canonical_json;
use lancet_protocol::cell::{Cell, ChannelKind, ToolDescriptor, parse_cell_ref};
use lancet_protocol::evidence::{EffectSource, ObservedTarget};
use lancet_protocol::{CepError, CepErrorCode, InvocationId, Retriability};
use wasmtime::component::Linker;
use wasmtime::{Engine, Store, UpdateDeadline};

use crate::admission::{AdmittedTool, resolve_scope};
use crate::bindings;
use crate::composition::{ClosedCatalog, CompositionError, EmbeddedTool, ExpectedToolIdentity};
use crate::error_map::{
    cancelled_error, effect_not_admitted, effect_violation, map_component_error, schema_mismatch,
    schema_violation_detail,
};
use crate::evidence::CellEvidenceLedger;
use crate::generated;
use crate::host_mutate::WorkspaceMutationHost;
use crate::host_network::NetworkFetchHost;
use crate::host_workspace::WorkspaceObservationHost;
use crate::limits::{AbortReason, ExecutionLimits, RecordingLimits, ResourceFootprint};

/// Call-scoped context passed to `ComponentRuntime::call`.
///
/// `admitted` is the outcome of the per-call admission step (D1) that runs
/// before the runtime is entered; it is the sole authorization authority for
/// this call's operations and result cell.
pub struct ComponentCallContext {
    pub invocation_id: InvocationId,
    pub turn_id: String,
    pub events: EventSink,
    pub cancellation: Cancellation,
    pub evidence: CellEvidenceLedger,
    pub admitted: Arc<AdmittedTool>,
    /// Host authority for the `workspace-observe` imports (#30).
    ///
    /// `None` means this call has no workspace authority, and every observation
    /// operation is refused — the fail-closed default. It is `Option` rather than
    /// required because most Tools import nothing from the workspace channel and
    /// the transport should not have to fabricate an authority they cannot use;
    /// a Tool that *does* import the interface gets a real one, and one that does
    /// not cannot reach the interface at all.
    ///
    /// Shared per Turn (`Arc`) because the concrete Local Workspace and the
    /// Turn's observation budget are both per-turn, not per-call: a per-call copy
    /// of the budget would silently reset the per-turn ceiling.
    pub workspace: Option<Arc<WorkspaceObservationHost>>,
    /// Host authority for the `workspace-mutate` imports (#31).
    ///
    /// Same fail-closed shape as [`Self::workspace`]: `None` refuses every
    /// mutation import. Observation and mutation authorities are separate so a
    /// Tool that only observes never receives a write host by accident.
    pub mutate: Option<Arc<WorkspaceMutationHost>>,
    /// GET-only network authority for the `network-fetch` import (#32).
    /// `None` refuses every network operation.
    pub network: Option<Arc<NetworkFetchHost>>,
}

/// Per-store state: the call context, resource limits, deadline, and abort
/// reason (set by the epoch callback or `RecordingLimits`).
pub(crate) struct StoreState {
    pub call: ComponentCallContext,
    pub limits: RecordingLimits,
    pub deadline: Instant,
    pub abort: Option<AbortReason>,
    /// Configured per-call count limits, kept so a count-limit instantiation
    /// failure (which has no rejection callback) can be classified against the
    /// tool's static footprint.
    pub count_limits: ResourceFootprint,
}

/// The Component runtime. Composed once at startup; immutable after.
///
/// Three lifecycle tiers:
///
/// 1. **Process-level (built runtime):** `Arc<ComponentRuntime>` via [`Self::shared()`],
///    holding the Wasmtime Engine, compiled Components, sealed `ClosedCatalog`,
///    and linker. Shared for the lifetime of the host process.
/// 2. **Per-Turn:** frozen profile/policy, enabled catalog snapshot, workspace
///    authorities, audit context, budget, and cancellation. Created by
///    `LancetRuntime::build`.
/// 3. **Per-Tool-Call:** fresh `Store` with per-call fuel, deadline, limits,
///    admission, cancellation, and Cell evidence.
///
/// `call` takes `&self`: the epoch ticker is lazily spawned on the first
/// call via a [`OnceLock`], so an `Arc<ComponentRuntime>` can be shared
/// across catalog projection and execution.
pub struct ComponentRuntime {
    core: Arc<ComponentCallRuntime>,
    ticker: OnceLock<EpochTicker>,
}

/// The synchronous Wasmtime/Tool Host half of a Component call.
///
/// It is reference-counted so the whole call can run on Tokio's blocking
/// pool. Host imports use blocking Linux locks while they hold their guards;
/// keeping this object off a Tokio worker preserves that critical section
/// without weakening it to a nonblocking best-effort operation.
struct ComponentCallRuntime {
    engine: Engine,
    async_runtime: ComponentAsyncRuntime,
    catalog: Arc<ClosedCatalog>,
    linker: Arc<Linker<StoreState>>,
    limits: Arc<Mutex<ExecutionLimits>>,
}

struct ComponentAsyncRuntime {
    inner: Option<tokio::runtime::Runtime>,
}

impl ComponentAsyncRuntime {
    fn handle(&self) -> tokio::runtime::Handle {
        self.inner
            .as_ref()
            .expect("Component async runtime is present until drop")
            .handle()
            .clone()
    }
}

impl Drop for ComponentAsyncRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.inner.take() {
            runtime.shutdown_background();
        }
    }
}

/// Handle to the epoch ticker task. Stops when dropped.
struct EpochTicker {
    stop: tokio::sync::watch::Sender<bool>,
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl ComponentRuntime {
    /// Production constructor: compose from `inventory` registrations +
    /// the generated `EXPECTED_COMPONENTS` identity list.
    pub fn from_embedded() -> Result<Self, CompositionError> {
        let regs: Vec<&'static EmbeddedTool> =
            inventory::iter::<&EmbeddedTool>().copied().collect();
        Self::compose_private(&regs, generated::host_composition::EXPECTED_COMPONENTS)
    }

    /// Process-level shared instance of the embedded Component runtime.
    ///
    /// The embedded catalog, Wasmtime Engine, compiled Components, and linker
    /// depend only on the current host binary's static bytes and host-pinned
    /// identities. They do not change across Sessions, Projects, Profiles, or
    /// Turns, so a single composition is shared for the lifetime of the process.
    ///
    /// Concurrent first callers race on `OnceLock::get_or_init`; exactly one
    /// runs `from_embedded()` and the rest observe the same result. A
    /// composition failure is latched: every subsequent call returns the same
    /// error without retrying.
    pub fn shared() -> Result<Arc<Self>, Arc<CompositionError>> {
        static INSTANCE: OnceLock<Result<Arc<ComponentRuntime>, Arc<CompositionError>>> =
            OnceLock::new();
        INSTANCE
            .get_or_init(|| {
                ComponentRuntime::from_embedded()
                    .map(Arc::new)
                    .map_err(Arc::new)
            })
            .clone()
    }

    /// Fixed no-op fixture for integration tests. No caller-controlled
    /// registrations, paths, bytes, handlers, or approval set.
    #[cfg(feature = "test-utils")]
    #[doc(hidden)]
    pub fn noop_fixture() -> Result<Self, CompositionError> {
        let reg: &'static EmbeddedTool = generated::noop_fixture::registration();
        let id = generated::noop_fixture::expected_identity();
        let regs: [&'static EmbeddedTool; 1] = [reg];
        Self::compose_private(&regs, std::slice::from_ref(&id))
    }

    fn compose_private(
        registrations: &[&'static EmbeddedTool],
        expected_identities: &[ExpectedToolIdentity],
    ) -> Result<Self, CompositionError> {
        let engine = build_engine().map_err(CompositionError::Engine)?;
        let async_runtime = build_async_runtime().map_err(CompositionError::AsyncRuntime)?;
        let catalog =
            ClosedCatalog::compose(&engine, registrations.iter().copied(), expected_identities)?;
        let linker = build_linker(&engine).map_err(CompositionError::Linker)?;
        let limits = ExecutionLimits::default();

        Ok(Self {
            core: Arc::new(ComponentCallRuntime {
                engine,
                async_runtime,
                catalog: Arc::new(catalog),
                linker: Arc::new(linker),
                limits: Arc::new(Mutex::new(limits)),
            }),
            ticker: OnceLock::new(),
        })
    }

    /// Ensure the epoch ticker is running. Called lazily from `call` so
    /// composition doesn't require a tokio runtime context. Idempotent via
    /// `OnceLock`: concurrent first calls install the ticker exactly once.
    fn ensure_ticker(&self) {
        if self.ticker.get().is_none() {
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            spawn_epoch_ticker(self.core.engine.clone(), stop_rx);
            let _ = self.ticker.set(EpochTicker { stop: stop_tx });
        }
    }

    /// The tool descriptors in the closed catalog.
    pub fn descriptors(&self) -> impl Iterator<Item = &ToolDescriptor> {
        self.core.catalog.descriptors()
    }

    /// Descriptors paired with their host-pinned descriptor identities.
    pub fn descriptor_identities(&self) -> impl Iterator<Item = (&ToolDescriptor, &str)> {
        self.core.catalog.descriptor_identities()
    }

    /// The shared descriptor for a Component method. The admission step (D1)
    /// runs against it before `call`, so it is handed out by `Arc` rather
    /// than cloned per call.
    pub fn descriptor(&self, name: &str) -> Option<Arc<ToolDescriptor>> {
        self.core
            .catalog
            .get(name)
            .map(|tool| tool.descriptor.clone())
    }

    /// Call a Component Tool by name.
    ///
    /// 1. Look up the tool → `method_not_found` if absent.
    /// 2. Validate params against the descriptor's JSON Schema.
    /// 3. Short-circuit with `CallCancelled` if the call is pre-cancelled.
    /// 4. Instantiate the Component in a fresh Store.
    /// 5. Call `tool-api.run`; a guest logical error while cancellation is
    ///    set maps to `CallCancelled` (state, not the guest's string).
    /// 6. Parse the result CellRef (reject 4-segment guest-supplied trust).
    /// 7. Verify the result cell is in the descriptor's capabilities and in
    ///    the call's admitted set (D3: exact membership in both).
    /// 8. Validate the result value against the descriptor's JSON Schema.
    /// 9. Measure the returned value host-side (D12) and record the result
    ///    effect with host-derived trust.
    pub async fn call(
        &self,
        name: &str,
        params: serde_json::Value,
        ctx: ComponentCallContext,
    ) -> Result<serde_json::Value, CepError> {
        // Poll the Wasmtime call from Tokio's blocking pool because synchronous
        // workspace imports take Linux locks. Async network imports use the bounded
        // runtime owned by this composed Component catalog.
        self.ensure_ticker();

        let name = name.to_owned();
        let error_name = name.clone();
        let core = Arc::clone(&self.core);
        let async_handle = core.async_runtime.handle();
        tokio::task::spawn_blocking(move || {
            async_handle.block_on(core.call_async(&name, params, ctx))
        })
        .await
        .map_err(|_| {
            CepError::tool_execution(
                CepErrorCode::OutcomeIndeterminate,
                &error_name,
                "component_worker_failed_after_dispatch",
                None,
                Retriability::Permanent,
            )
        })?
    }
}

impl ComponentCallRuntime {
    /// Async Wasmtime execution driven by the composed catalog's bounded runtime.
    /// The outer blocking worker keeps synchronous workspace locks off async workers.
    async fn call_async(
        &self,
        name: &str,
        params: serde_json::Value,
        ctx: ComponentCallContext,
    ) -> Result<serde_json::Value, CepError> {
        let tool = self
            .catalog
            .get(name)
            .ok_or_else(|| lancet_protocol::error::method_not_found(name))?;

        // Validate params before entering Wasmtime.
        tool.params_validator.validate(&params).map_err(|e| {
            // Location only: the error message reaches the durable audit row, and
            // `jsonschema`'s Display would put the caller's whole document there.
            schema_mismatch(name, schema_violation_detail("params", &e))
        })?;

        // Pre-cancelled call: cancellation state is the authority; never
        // enter Wasmtime (#70, Codeberg #48).
        if ctx.cancellation.is_cancelled() {
            return Err(cancelled_error(name));
        }

        let mut store = self.new_store(ctx);

        let instance = match bindings::Tool::instantiate_async(
            &mut store,
            &tool.component,
            &self.linker,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return Err(map_component_error(&store, name, e, &tool.footprint)),
        };

        let tool_api = instance.lattice_tool_host_tool_api();
        let params_json = serde_json::to_string(&params)
            .map_err(|e| schema_mismatch(name, format!("params serialize: {e}")))?;

        let raw = match tool_api.call_run(&mut store, &params_json).await {
            Ok(r) => r,
            Err(e) => return Err(map_component_error(&store, name, e, &tool.footprint)),
        };

        // The guest's `result<tool-output, string>`: the outer Result is
        // wasmtime's (traps), the inner is the guest's logical error. A
        // logical error while call-scoped cancellation is set maps to
        // `CallCancelled` — cancellation state, not the guest-controlled
        // error string, is the classification authority (#70).
        let cancelled = store.data().call.cancellation.is_cancelled();
        let raw = match map_guest_logical_result(name, raw, cancelled) {
            Ok(raw) => raw,
            // The host's own refusal outranks the guest's retelling of it: a
            // domain import that refused this call already produced a typed
            // error with a real code, and that is what the caller and the model
            // must see (#30).
            Err(err) => return Err(host_refusal(&store.data().call).unwrap_or(err)),
        };

        // WIT output uses a trust-free 3-segment CellRef. Guest cannot
        // claim `in` — parse_cell_ref rejects a 4th segment.
        let result_cell: Cell = parse_cell_ref(&raw.result_cell)
            .map_err(|e| schema_mismatch(name, format!("result_cell: {e}")))?;
        let result_value: serde_json::Value = serde_json::from_str(&raw.value_json)
            .map_err(|e| schema_mismatch(name, format!("result value: {e}")))?;

        // Result-cell ceiling: declared by the descriptor *and* admitted for
        // this call. A violation fails closed — the result effect is never
        // recorded, while operation effects already in the ledger survive
        // into audit (B5).
        check_result_cell(
            name,
            &tool.descriptor,
            &store.data().call.admitted,
            &result_cell,
        )?;

        // Validate the result value against the descriptor's JSON Schema.
        tool.result_validator
            .validate(&result_value)
            .map_err(|e| schema_mismatch(name, schema_violation_detail("result", &e)))?;

        // Host-derived scope/trust and host-computed measure: the guest has
        // no numeric channel and its scope claim is never trusted.
        let state = store.data();
        record_result_effect(
            &tool.descriptor,
            &state.call.evidence,
            &state.call.turn_id,
            result_cell,
            &result_value,
        );

        Ok(result_value)
    }

    /// Create a fresh `Store` with per-call limits, fuel, and epoch deadline.
    fn new_store(&self, ctx: ComponentCallContext) -> Store<StoreState> {
        let limits = self.limits.lock().unwrap().clone();
        let deadline = Instant::now() + limits.deadline;
        let fuel = limits.fuel;
        let count_limits = ResourceFootprint {
            instances: limits.instances,
            memories: limits.memories,
            tables: limits.tables,
        };
        let limits = RecordingLimits::new(&limits);
        let mut store = Store::new(
            &self.engine,
            StoreState {
                call: ctx,
                limits,
                deadline,
                abort: None,
                count_limits,
            },
        );

        // Resource limiter: memory/tables/instances.
        store.limiter(|state| &mut state.limits as &mut dyn wasmtime::ResourceLimiter);

        // Fuel: coarse CPU upper bound.
        let _ = store.set_fuel(fuel);

        // Epoch interruption: per-Store cancellation/deadline. The callback
        // checks THIS store's cancellation/deadline; unrelated stores yield.
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(|mut cx| {
            if cx.data().call.cancellation.is_cancelled() {
                cx.data_mut().abort = Some(AbortReason::Cancelled);
                return Ok(UpdateDeadline::Interrupt);
            }
            if Instant::now() >= cx.data().deadline {
                cx.data_mut().abort = Some(AbortReason::Deadline);
                return Ok(UpdateDeadline::Interrupt);
            }
            Ok(UpdateDeadline::Continue(1))
        });

        store
    }
}

/// Result-cell ceiling (D3): the cell must be exactly one of the
/// descriptor's declared capability cells **and** exactly one of the cells
/// admitted for this call. There is no partial order over
/// `CtxOp × ExtImpact` — membership is set membership, in both directions.
fn check_result_cell(
    name: &str,
    descriptor: &ToolDescriptor,
    admitted: &AdmittedTool,
    result_cell: &Cell,
) -> Result<(), CepError> {
    let cell_ref = result_cell.reference();
    if !descriptor
        .capabilities
        .iter()
        .any(|cap| &cap.cell == result_cell)
    {
        return Err(effect_violation(name, &cell_ref));
    }
    if !admitted.covers(result_cell) {
        return Err(effect_not_admitted(name, &cell_ref));
    }
    Ok(())
}

/// Record the result effect with host-derived scope/trust and the host-computed
/// measure. The measure is taken for every result cell, so the declared
/// `ctx_op` cannot reach it.
fn record_result_effect(
    descriptor: &ToolDescriptor,
    evidence: &CellEvidenceLedger,
    turn_id: &str,
    result_cell: Cell,
    value: &serde_json::Value,
) {
    let scope = resolve_scope(descriptor, &result_cell.scope, ChannelKind::Session);
    evidence.record_effect(
        result_cell,
        scope,
        ObservedTarget::Session {
            turn_id: turn_id.to_owned(),
        },
        EffectSource::Result,
        Some(context_measure(value)),
    );
}

/// Deterministic context measure (D12): the byte length of the returned
/// value's canonical JSON, computed host-side for **every** result effect
/// whatever `ctx_op` the guest declared.
///
/// Branching on the declared `ctx_op` would make metering guest-switchable —
/// a tool declaring an `alloc` and a `mutate` capability over one scope (the
/// shipped `noop.echo` does) could relabel its result cell and be recorded
/// unmeasured. The declared `ctx_op` states charge intent for #6's
/// charge/credit seam only. Operation effects carry no measure: they return
/// no value.
fn context_measure(value: &serde_json::Value) -> u64 {
    canonical_json(value).len() as u64
}

/// Map the guest's logical result to the host-visible result
/// (#70, Codeberg #48).
///
/// Call-scoped cancellation state is the sole classification authority: a
/// guest logical `Err` while cancellation is set maps to `CallCancelled`,
/// whatever string the guest returned. A guest `Err` without cancellation
/// keeps the `ExecutionFailed` semantics; `Ok` passes through.
fn map_guest_logical_result(
    name: &str,
    raw: Result<bindings::exports::lattice::tool_host::tool_api::ToolOutput, String>,
    cancelled: bool,
) -> Result<bindings::exports::lattice::tool_host::tool_api::ToolOutput, CepError> {
    match raw {
        Ok(output) => Ok(output),
        Err(_) if cancelled => Err(cancelled_error(name)),
        Err(guest_err) => Err(guest_error(name, guest_err)),
    }
}

/// Build a `CepError` for a guest-returned logical error string.
fn guest_error(method: &str, guest_err: String) -> CepError {
    let mut err = CepError::tool_execution(
        CepErrorCode::ExecutionFailed,
        method,
        "guest_error",
        None,
        Retriability::Permanent,
    );
    err.message = format!("tool {method} guest error: {guest_err}");
    err
}

/// The host's own refusal for this call, if a Tool Host import produced one.
///
/// A domain import that refuses an operation already decided *why*, as a typed
/// [`CepError`] with a real error code. The guest can only hand that back as a
/// string, and `guest_error` would then wrap it into a generic
/// `ExecutionFailed` — turning "this path escapes the project root"
/// (`PathContainment`) into "the tool failed", and prefixing the message with
/// `tool <name> guest error:`. A caller reading the code could no longer tell a
/// policy refusal from a crash, and the model would read the host's sentence
/// through the guest's frame.
///
/// So the host records its refusal on the call and the runtime prefers it. The
/// guest's string is not consulted when the host already spoke.
fn host_refusal(ctx: &ComponentCallContext) -> Option<CepError> {
    ctx.workspace
        .as_ref()
        .and_then(|workspace| workspace.take_refusal())
        .or_else(|| {
            ctx.mutate
                .as_ref()
                .and_then(|mutate| mutate.take_refusal(ctx.invocation_id))
        })
        .or_else(|| {
            ctx.network
                .as_ref()
                .and_then(|network| network.take_refusal())
        })
}

// ---------------------------------------------------------------------------
// Engine + linker construction
// ---------------------------------------------------------------------------

fn build_async_runtime() -> std::io::Result<ComponentAsyncRuntime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(8)
        .thread_name("lattice-component")
        .enable_all()
        .build()
        .map(|inner| ComponentAsyncRuntime { inner: Some(inner) })
}

fn build_engine() -> Result<Engine, wasmtime::Error> {
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    Engine::new(&c)
}

/// Link every Tool Host interface the host serves.
///
/// One linker for the whole runtime, so a Tool's *declared* imports — pinned by
/// the composition gate against its Component's real import section — are what
/// decide which of these it can reach, not which bindings happen to be linked. A
/// Tool that never declared `workspace-observe` cannot call it: the world it was
/// compiled against has no such interface.
///
/// `workspace-observe` is additionally inert without call-scoped authority: its
/// implementation refuses every operation when `ComponentCallContext::workspace`
/// is `None`, so a caller that links the interface but supplies no host authority
/// fails closed rather than reaching a default.
fn build_linker(engine: &Engine) -> Result<Linker<StoreState>, wasmtime::Error> {
    let mut linker = Linker::new(engine);
    bindings::lattice::tool_host::host_control::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
        &mut linker,
        |state| state,
    )?;
    bindings::lattice::tool_host::workspace_observe::add_to_linker::<
        _,
        wasmtime::component::HasSelf<_>,
    >(&mut linker, |state| state)?;
    bindings::lattice::tool_host::workspace_mutate::add_to_linker::<
        _,
        wasmtime::component::HasSelf<_>,
    >(&mut linker, |state| state)?;
    bindings::lattice::tool_host::network_fetch::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
        &mut linker,
        |state| state,
    )?;
    Ok(linker)
}

/// Spawn the epoch ticker: periodically increments the shared engine's epoch
/// so each Store's deadline callback fires. Stops when `stop` is signaled.
fn spawn_epoch_ticker(engine: Engine, mut stop: tokio::sync::watch::Receiver<bool>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(5));
        loop {
            tokio::select! {
                _ = tick.tick() => engine.increment_epoch(),
                _ = stop.wait_for(|&s| s) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;

    use bindings::exports::lattice::tool_host::tool_api::ToolOutput;
    use lancet_protocol::cell::{
        ArtifactIdentity, Capability, CtxOp, ExtImpact, ScopeDecl, ToolDescriptor, Trust,
    };
    use lancet_protocol::evidence::CapabilityAdmissionPolicy;

    use crate::admission::admit_tool;

    fn tool_output() -> ToolOutput {
        ToolOutput {
            result_cell: "alloc:none:session".into(),
            value_json: "{}".into(),
        }
    }

    fn scoped_cell(ctx_op: CtxOp, scope: &str) -> Cell {
        Cell {
            ctx_op,
            ext_impact: ExtImpact::None,
            scope: scope.into(),
        }
    }

    fn cell(ctx_op: CtxOp) -> Cell {
        scoped_cell(ctx_op, "session")
    }

    /// The value every measure assertion below is taken over:
    /// `{"echoed":"hello"}` — 18 canonical bytes.
    fn result_value() -> serde_json::Value {
        serde_json::json!({"echoed": "hello"})
    }

    /// A synthetic descriptor declaring one capability over `session/in`. Two
    /// `in` scopes are declared so scope-crossing cases are expressible.
    fn descriptor(capability: &str, ctx_op: CtxOp) -> Arc<ToolDescriptor> {
        Arc::new(ToolDescriptor {
            name: "synthetic.tool".into(),
            version: "0.1.0".into(),
            description: "result-cell fixture".into(),
            params_schema: serde_json::Value::Null,
            result_schema: serde_json::Value::Null,
            scopes: vec![
                ScopeDecl {
                    name: "session".into(),
                    trust: Trust::In,
                    channel: ChannelKind::Session,
                },
                ScopeDecl {
                    name: "workspace".into(),
                    trust: Trust::In,
                    channel: ChannelKind::Workspace,
                },
            ],
            capabilities: vec![Capability {
                name: capability.into(),
                cell: cell(ctx_op),
            }],
            artifact: ArtifactIdentity {
                sha256: "0".repeat(64),
                toolchain: "test".into(),
            },
            abi_version: 1,
        })
    }

    fn admitted(descriptor: Arc<ToolDescriptor>, deny: &[&str]) -> AdmittedTool {
        let policy = CapabilityAdmissionPolicy {
            enabled_tools: BTreeSet::from([descriptor.name.clone()]),
            denied_capabilities: deny.iter().map(|s| (*s).to_string()).collect(),
            max_boundary: Trust::In,
        };
        admit_tool(descriptor, &policy).expect("fixture admits")
    }

    /// The measure the result path actually records for `result_cell`, read
    /// back from the ledger the production seam writes to.
    fn recorded_measure(descriptor: &ToolDescriptor, result_cell: Cell) -> Option<u64> {
        let ledger = CellEvidenceLedger::new();
        record_result_effect(descriptor, &ledger, "turn-1", result_cell, &result_value());
        let (_, effects, _) = ledger.snapshot();
        effects.effects[0].context_measure
    }

    /// §10: a declared `free:none:session` result cell is accepted, and it is
    /// measured like every other result. `Free`/`Mutate` differ from `Alloc`
    /// in how #6 charges the number, not in whether the host records one.
    #[test]
    fn free_result_cell_is_accepted_and_still_measured() {
        let descriptor = descriptor("session.free", CtxOp::Free);
        let admitted = admitted(descriptor.clone(), &[]);
        assert_eq!(
            check_result_cell("synthetic.tool", &descriptor, &admitted, &cell(CtxOp::Free)),
            Ok(())
        );
        assert_eq!(recorded_measure(&descriptor, cell(CtxOp::Free)), Some(18));
    }

    /// A3: the shipped `noop.echo` declares an `alloc` **and** a `mutate`
    /// capability over one scope, and both are admitted — so a guest can
    /// relabel its result cell and still pass the ceiling. Metering is not
    /// its to switch off: the relabelled result is measured all the same.
    #[test]
    fn a_relabelled_result_cell_is_still_measured() {
        let descriptor: Arc<ToolDescriptor> = Arc::new(
            serde_json::from_str(include_str!(
                "../../../tools/noop/generated/descriptor.json"
            ))
            .expect("the committed noop descriptor parses"),
        );
        let admitted = admitted(descriptor.clone(), &[]);
        assert_eq!(
            admitted.evidence.approved_capabilities,
            vec!["session.events".to_string(), "session.result".to_string()]
        );

        let relabelled = cell(CtxOp::Mutate);
        assert_eq!(
            check_result_cell("noop.echo", &descriptor, &admitted, &relabelled),
            Ok(())
        );
        assert_eq!(recorded_measure(&descriptor, relabelled), Some(18));
    }

    /// D12: the measure is the canonical-JSON byte length, so it is
    /// independent of the guest's key order.
    #[test]
    fn context_measure_is_the_canonical_json_byte_length() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"b":1,"a":"xy"}"#).expect("value parses");
        let reordered: serde_json::Value =
            serde_json::from_str(r#"{"a":"xy","b":1}"#).expect("value parses");
        // `{"a":"xy","b":1}` — 16 bytes.
        assert_eq!(context_measure(&value), 16);
        assert_eq!(context_measure(&value), context_measure(&reordered));
    }

    #[test]
    fn result_cell_must_be_declared_and_exactly_admitted() {
        for (name, extra_capability, deny, result_cell, message_fragment) in [
            // The ceiling intersects declarations and admission; the messages distinguish each boundary.
            (
                "undeclared",
                None,
                vec![],
                cell(CtxOp::Mutate),
                "not in declared capabilities",
            ),
            // A second capability keeps admission nonempty, isolating the result ceiling check.
            (
                "declared_but_not_admitted",
                Some(Capability {
                    name: "session.events".into(),
                    cell: cell(CtxOp::Mutate),
                }),
                vec!["session.result"],
                cell(CtxOp::Alloc),
                "not in the admitted",
            ),
            // A twin differing only by scope must not cover the denied result cell.
            (
                "denied_scope_twin",
                Some(Capability {
                    name: "workspace.result".into(),
                    cell: scoped_cell(CtxOp::Alloc, "workspace"),
                }),
                vec!["workspace.result"],
                scoped_cell(CtxOp::Alloc, "workspace"),
                "not in the admitted",
            ),
        ] {
            let mut descriptor = (*descriptor("session.result", CtxOp::Alloc)).clone();
            if let Some(capability) = extra_capability {
                descriptor.capabilities.push(capability);
            }
            let descriptor = Arc::new(descriptor);
            let Err(err) = check_result_cell(
                "synthetic.tool",
                &descriptor,
                &admitted(descriptor.clone(), &deny),
                &result_cell,
            ) else {
                panic!("{name}: result cell must fail");
            };
            assert_eq!(err.code, CepErrorCode::CapabilityViolation, "{name}");
            assert!(
                err.message.contains(message_fragment),
                "{name}: {}",
                err.message
            );
        }
    }

    /// #70 (Codeberg #48): a guest logical error while call-scoped
    /// cancellation is set
    /// maps to the shared `CallCancelled` error — the guest-controlled
    /// error string plays no role.
    #[test]
    fn guest_error_with_cancellation_set_maps_to_call_cancelled() {
        let err = map_guest_logical_result("noop.echo", Err("cancelled".into()), true)
            .expect_err("cancelled guest error must fail");
        assert_eq!(err.code, CepErrorCode::CallCancelled);
        assert_eq!(err.message, "tool noop.echo failed: cancelled");
    }

    /// #70: a guest logical error without cancellation keeps the
    /// `ExecutionFailed` semantics.
    #[test]
    fn guest_error_without_cancellation_stays_execution_failed() {
        let err = map_guest_logical_result("noop.echo", Err("boom".into()), false)
            .expect_err("guest error must fail");
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert_eq!(err.message, "tool noop.echo guest error: boom");
    }

    #[test]
    fn guest_ok_passes_through_regardless_of_cancellation() {
        for cancelled in [false, true] {
            let out = map_guest_logical_result("noop.echo", Ok(tool_output()), cancelled)
                .expect("ok passes through");
            assert_eq!(out.value_json, "{}");
        }
    }
}

#[cfg(test)]
#[path = "runtime/negative_tests.rs"]
mod negative_tests;

#[cfg(test)]
#[path = "runtime/embedded_tests.rs"]
mod embedded_tests;
