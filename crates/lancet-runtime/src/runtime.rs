// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Component runtime — the wasmtime engine, closed catalog, linker, and
//! call execution surface.
//!
//! The runtime is composed at startup from inventory registrations +
//! host-pinned identities. After composition, the catalog is immutable.
//! Each call gets a fresh `Store` with per-call limits, fuel, and epoch
//! deadline. The shared engine's epoch ticker runs in a background task.

use std::time::Instant;

use lancet_dispatch::{Cancellation, EventSink};
use lancet_protocol::cell::{Cell, parse_cell_ref};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use wasmtime::{Engine, Store, UpdateDeadline};
use wasmtime::component::Linker;

use crate::bindings;
use crate::composition::{ClosedCatalog, CompositionError, EmbeddedTool, ExpectedToolIdentity};
use crate::error_map::{effect_violation, map_component_error, schema_mismatch};
use crate::evidence::CellEvidenceLedger;
use crate::generated;
use crate::limits::{AbortReason, ExecutionLimits, RecordingLimits};

/// Call-scoped context passed to `ComponentRuntime::call`.
pub struct ComponentCallContext {
    pub call_id: u64,
    pub turn_id: String,
    pub events: EventSink,
    pub cancellation: Cancellation,
    pub evidence: CellEvidenceLedger,
}

/// Per-store state: the call context, resource limits, deadline, and abort
/// reason (set by the epoch callback or `RecordingLimits`).
pub(crate) struct StoreState {
    pub call: ComponentCallContext,
    pub limits: RecordingLimits,
    pub deadline: Instant,
    pub abort: Option<AbortReason>,
}

/// The Component runtime. Composed once at startup; immutable after.
pub struct ComponentRuntime {
    engine: Engine,
    catalog: ClosedCatalog,
    linker: Linker<StoreState>,
    limits: ExecutionLimits,
    ticker: Option<EpochTicker>,
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
        let catalog = ClosedCatalog::compose(&engine, registrations.iter().copied(), expected_identities)?;
        let linker = build_linker(&engine).map_err(CompositionError::Linker)?;
        let limits = ExecutionLimits::default();

        Ok(Self {
            engine,
            catalog,
            linker,
            limits,
            ticker: None,
        })
    }

    /// Ensure the epoch ticker is running. Called lazily from `call` so
    /// composition doesn't require a tokio runtime context.
    fn ensure_ticker(&mut self) {
        if self.ticker.is_none() {
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            spawn_epoch_ticker(self.engine.clone(), stop_rx);
            self.ticker = Some(EpochTicker { stop: stop_tx });
        }
    }

    /// The tool descriptors in the closed catalog.
    pub fn descriptors(&self) -> impl Iterator<Item = &lancet_protocol::cell::ToolDescriptor> {
        self.catalog.descriptors()
    }

    /// Override the per-call execution limits. Used by tests to exercise
    /// fuel/timeout/memory limits without composing a new runtime.
    pub fn set_limits(&mut self, limits: ExecutionLimits) {
        self.limits = limits;
    }

    /// Call a Component Tool by name.
    ///
    /// 1. Look up the tool → `method_not_found` if absent.
    /// 2. Validate params against the descriptor's JSON Schema.
    /// 3. Instantiate the Component in a fresh Store.
    /// 4. Call `tool-api.run`.
    /// 5. Parse the result CellRef (reject 4-segment guest-supplied trust).
    /// 6. Verify the result cell is in the descriptor's capabilities.
    /// 7. Validate the result value against the descriptor's JSON Schema.
    /// 8. Derive trust from the descriptor's scope and record Cell evidence.
    pub async fn call(
        &mut self,
        name: &str,
        params: serde_json::Value,
        ctx: ComponentCallContext,
    ) -> Result<serde_json::Value, CepError> {
        // Lazily start the epoch ticker (requires a tokio runtime context,
        // which `call` runs inside; composition does not).
        self.ensure_ticker();

        let tool = self
            .catalog
            .get(name)
            .ok_or_else(|| lancet_protocol::error::method_not_found(name))?;

        // Validate params before entering Wasmtime.
        tool.params_validator
            .validate(&params)
            .map_err(|e| schema_mismatch(name, format!("params: {e}")))?;

        let mut store = self.new_store(ctx);

        let instance = match bindings::Tool::instantiate(&mut store, &tool.component, &self.linker) {
            Ok(v) => v,
            Err(e) => return Err(map_component_error(&store, name, e)),
        };

        let tool_api = instance.lattice_tool_host_tool_api();
        let params_json = serde_json::to_string(&params)
            .map_err(|e| schema_mismatch(name, format!("params serialize: {e}")))?;

        let raw = match tool_api.call_run(&mut store, &params_json) {
            Ok(r) => r,
            Err(e) => return Err(map_component_error(&store, name, e)),
        };

        // The guest's `result<tool-output, string>`: the outer Result is
        // wasmtime's (traps), the inner is the guest's logical error.
        let raw = match raw {
            Ok(output) => output,
            Err(guest_err) => {
                return Err(guest_error(name, guest_err));
            }
        };

        // WIT output uses a trust-free 3-segment CellRef. Guest cannot
        // claim `in` — parse_cell_ref rejects a 4th segment.
        let result_cell: Cell = parse_cell_ref(&raw.result_cell)
            .map_err(|e| schema_mismatch(name, format!("result_cell: {e}")))?;
        let result_value: serde_json::Value = serde_json::from_str(&raw.value_json)
            .map_err(|e| schema_mismatch(name, format!("result value: {e}")))?;

        // Verify the result cell is in the descriptor's capability cells.
        let cell_ref = result_cell.reference();
        let declared = tool
            .descriptor
            .capabilities
            .iter()
            .any(|cap| cap.cell == result_cell);
        if !declared {
            return Err(effect_violation(name, &cell_ref));
        }

        // Validate the result value against the descriptor's JSON Schema.
        tool.result_validator
            .validate(&result_value)
            .map_err(|e| schema_mismatch(name, format!("result: {e}")))?;

        // Derive trust from the descriptor's scope and record evidence.
        let trust = tool.descriptor.trust_of(&result_cell.scope);
        store.data_mut().call.evidence.record(result_cell, trust);

        Ok(result_value)
    }

    /// Create a fresh `Store` with per-call limits, fuel, and epoch deadline.
    fn new_store(&self, ctx: ComponentCallContext) -> Store<StoreState> {
        let limits = RecordingLimits::new(&self.limits);
        let deadline = Instant::now() + self.limits.deadline;
        let mut store = Store::new(&self.engine, StoreState {
            call: ctx,
            limits,
            deadline,
            abort: None,
        });

        // Resource limiter: memory/tables/instances.
        store.limiter(|state| &mut state.limits as &mut dyn wasmtime::ResourceLimiter);

        // Fuel: coarse CPU upper bound.
        let _ = store.set_fuel(self.limits.fuel);

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

// ---------------------------------------------------------------------------
// Engine + linker construction
// ---------------------------------------------------------------------------

fn build_engine() -> Result<Engine, wasmtime::Error> {
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    Engine::new(&c)
}

fn build_linker(engine: &Engine) -> Result<Linker<StoreState>, wasmtime::Error> {
    let mut linker = Linker::new(engine);
    bindings::lattice::tool_host::host_control::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
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
