// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Component runtime — the wasmtime engine, closed catalog, linker, and
//! call execution surface.
//!
//! The runtime is composed at startup from inventory registrations +
//! host-pinned identities. After composition, the catalog is immutable.
//! Each call gets a fresh `Store` with per-call limits, fuel, and epoch
//! deadline. The shared engine's epoch ticker runs in a background task.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use lancet_dispatch::{Cancellation, EventSink};
use lancet_protocol::canonical::canonical_json;
use lancet_protocol::cell::{Cell, ChannelKind, ToolDescriptor, parse_cell_ref};
use lancet_protocol::evidence::{EffectSource, ObservedTarget};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
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
use crate::limits::{AbortReason, ExecutionLimits, RecordingLimits, ResourceFootprint};

/// Call-scoped context passed to `ComponentRuntime::call`.
///
/// `admitted` is the outcome of the per-call admission step (D1) that runs
/// before the runtime is entered; it is the sole authorization authority for
/// this call's operations and result cell.
pub struct ComponentCallContext {
    pub call_id: u64,
    pub turn_id: String,
    pub events: EventSink,
    pub cancellation: Cancellation,
    pub evidence: CellEvidenceLedger,
    pub admitted: Arc<AdmittedTool>,
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
/// `call` takes `&self`: the epoch ticker is lazily spawned on the first
/// call via a [`OnceLock`], so an `Arc<ComponentRuntime>` can be shared
/// across the projection and server dispatch paths (D.2/D.3).
pub struct ComponentRuntime {
    engine: Engine,
    catalog: ClosedCatalog,
    linker: Linker<StoreState>,
    limits: std::sync::Mutex<ExecutionLimits>,
    ticker: OnceLock<EpochTicker>,
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
        let catalog =
            ClosedCatalog::compose(&engine, registrations.iter().copied(), expected_identities)?;
        let linker = build_linker(&engine).map_err(CompositionError::Linker)?;
        let limits = ExecutionLimits::default();

        Ok(Self {
            engine,
            catalog,
            linker,
            limits: std::sync::Mutex::new(limits),
            ticker: OnceLock::new(),
        })
    }

    /// Ensure the epoch ticker is running. Called lazily from `call` so
    /// composition doesn't require a tokio runtime context. Idempotent via
    /// `OnceLock`: concurrent first calls install the ticker exactly once.
    fn ensure_ticker(&self) {
        if self.ticker.get().is_none() {
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            spawn_epoch_ticker(self.engine.clone(), stop_rx);
            let _ = self.ticker.set(EpochTicker { stop: stop_tx });
        }
    }

    /// The tool descriptors in the closed catalog.
    pub fn descriptors(&self) -> impl Iterator<Item = &ToolDescriptor> {
        self.catalog.descriptors()
    }

    /// The shared descriptor for a Component method. The admission step (D1)
    /// runs against it before `call`, so it is handed out by `Arc` rather
    /// than cloned per call.
    pub fn descriptor(&self, name: &str) -> Option<Arc<ToolDescriptor>> {
        self.catalog.get(name).map(|tool| tool.descriptor.clone())
    }

    /// True if `name` is a Component method in the closed catalog.
    /// D.3's dispatch split uses this to branch Component vs native.
    pub fn contains(&self, name: &str) -> bool {
        self.catalog.get(name).is_some()
    }

    /// Override the per-call execution limits. Used by tests to exercise
    /// fuel/timeout/memory limits without composing a new runtime.
    pub fn set_limits(&mut self, limits: ExecutionLimits) {
        *self.limits.lock().unwrap() = limits;
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
        // Lazily start the epoch ticker (requires a tokio runtime context,
        // which `call` runs inside; composition does not).
        self.ensure_ticker();

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

        let instance = match bindings::Tool::instantiate(&mut store, &tool.component, &self.linker)
        {
            Ok(v) => v,
            Err(e) => return Err(map_component_error(&store, name, e, &tool.footprint)),
        };

        let tool_api = instance.lattice_tool_host_tool_api();
        let params_json = serde_json::to_string(&params)
            .map_err(|e| schema_mismatch(name, format!("params serialize: {e}")))?;

        let raw = match tool_api.call_run(&mut store, &params_json) {
            Ok(r) => r,
            Err(e) => return Err(map_component_error(&store, name, e, &tool.footprint)),
        };

        // The guest's `result<tool-output, string>`: the outer Result is
        // wasmtime's (traps), the inner is the guest's logical error. A
        // logical error while call-scoped cancellation is set maps to
        // `CallCancelled` — cancellation state, not the guest-controlled
        // error string, is the classification authority (#70).
        let cancelled = store.data().call.cancellation.is_cancelled();
        let raw = map_guest_logical_result(name, raw, cancelled)?;

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
        // Snapshot the count limits before `limits` is consumed into the
        // `RecordingLimits` wrapper; the mapper needs them to classify a
        // count-limit instantiation failure.
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

    /// The ceiling is an intersection: an undeclared cell is a declaration
    /// violation, a declared-but-denied cell is an admission violation, and
    /// both fail the call closed. The two branches are discriminated by their
    /// message, so neither can stand in for the other.
    #[test]
    fn result_cell_outside_the_ceiling_is_refused() {
        let descriptor = descriptor("session.result", CtxOp::Alloc);

        let undeclared = check_result_cell(
            "synthetic.tool",
            &descriptor,
            &admitted(descriptor.clone(), &[]),
            &cell(CtxOp::Mutate),
        )
        .expect_err("undeclared result cell must fail");
        assert_eq!(undeclared.code, CepErrorCode::EffectViolation);
        assert!(
            undeclared.message.contains("not in declared capabilities"),
            "{}",
            undeclared.message
        );

        // Denying the only capability leaves nothing admitted, so admission
        // itself refuses the tool — build the admitted set from a second
        // capability to isolate the effect-ceiling check.
        let mut two_caps = (*descriptor).clone();
        two_caps.capabilities.push(Capability {
            name: "session.events".into(),
            cell: cell(CtxOp::Mutate),
        });
        let two_caps = Arc::new(two_caps);
        let not_admitted = check_result_cell(
            "synthetic.tool",
            &two_caps,
            &admitted(two_caps.clone(), &["session.result"]),
            &cell(CtxOp::Alloc),
        )
        .expect_err("declared but not admitted must fail");
        assert_eq!(not_admitted.code, CepErrorCode::EffectViolation);
        assert!(
            not_admitted.message.contains("not in the admitted"),
            "{}",
            not_admitted.message
        );
    }

    /// B10/b1: the ceiling is exact on the scope axis. Two capabilities differ
    /// only by scope and one is denied, so a coverage predicate that ignored
    /// scope would admit the denied twin's cell.
    #[test]
    fn result_cell_on_a_denied_scope_is_not_covered_by_its_twin() {
        let mut two_scopes = (*descriptor("session.result", CtxOp::Alloc)).clone();
        two_scopes.capabilities.push(Capability {
            name: "workspace.result".into(),
            cell: scoped_cell(CtxOp::Alloc, "workspace"),
        });
        let two_scopes = Arc::new(two_scopes);

        let err = check_result_cell(
            "synthetic.tool",
            &two_scopes,
            &admitted(two_scopes.clone(), &["workspace.result"]),
            &scoped_cell(CtxOp::Alloc, "workspace"),
        )
        .expect_err("a declared cell on a denied scope must fail");
        assert_eq!(err.code, CepErrorCode::EffectViolation);
        assert!(
            err.message.contains("not in the admitted"),
            "{}",
            err.message
        );
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
