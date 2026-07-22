// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

/// Harness test layer (see RFC #19).
///
/// After #40 there is no Microsandbox layer: every scenario runs Direct-Local
/// against the canonical project root — no KVM, no MSB daemon, no OCI image.
/// `PtySmoke` remains as the TUI-entrypoint layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioLayer {
    FastBinary,
    PtySmoke,
}

/// Scenario metadata (inline builder or JSON fixture under `tests/scenarios/`).
///
/// `step_timeout_secs` is the per-command timeout enforced by
/// `LatticeDriver::run` for every invocation derived from this spec.
/// `total_timeout_secs` is an optional whole-scenario wall-clock budget
/// (PRD user story 19, RFC #19 decision eight): a multi-step scenario
/// records its start time and asserts the budget has not elapsed before each
/// step, so a sequence of steps that each finish within their step timeout
/// but never terminates is still bounded. `None` (the default) leaves only
/// the per-command timeout in effect, preserving the #20 single-step
/// behavior for fast scenarios.
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioSpec {
    pub name: String,
    pub layer: ScenarioLayer,
    #[serde(default = "default_step_timeout_secs")]
    pub step_timeout_secs: u64,
    #[serde(default)]
    pub total_timeout_secs: Option<u64>,
}

#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("failed to read scenario fixture: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid scenario fixture: {0}")]
    Parse(#[from] serde_json::Error),
}

fn default_step_timeout_secs() -> u64 {
    30
}

impl ScenarioSpec {
    pub fn fast(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            layer: ScenarioLayer::FastBinary,
            step_timeout_secs: default_step_timeout_secs(),
            total_timeout_secs: None,
        }
    }

    /// Multi-step Direct-Local scenario with both step and whole-run bounds.
    pub fn direct_local(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            layer: ScenarioLayer::FastBinary,
            step_timeout_secs: default_step_timeout_secs(),
            total_timeout_secs: Some(180),
        }
    }

    /// Per-command timeout enforced by `LatticeDriver::run`.
    pub fn step_timeout(&self) -> Duration {
        Duration::from_secs(self.step_timeout_secs)
    }

    /// Whole-scenario wall-clock budget, or `None` if only the per-command
    /// timeout is in effect.
    pub fn total_timeout(&self) -> Option<Duration> {
        self.total_timeout_secs.map(Duration::from_secs)
    }

    /// Load a scenario JSON fixture from the harness crate `tests/scenarios/` tree.
    pub fn load_fixture(relative: &str) -> Result<Self, ScenarioError> {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios");
        let path = base.join(relative);
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Start a [`ScenarioBudget`] tracking this scenario's whole-wall-clock
    /// budget. Returns a no-op budget when `total_timeout` is `None`, so
    /// fast scenarios pay nothing.
    pub fn start_budget(&self) -> ScenarioBudget {
        ScenarioBudget::new(self.total_timeout())
    }
}

/// Whole-scenario wall-clock budget (PRD user story 19, RFC #19 decision
/// eight). Created from `ScenarioSpec::start_budget`; call `check` before
/// each step of a multi-step scenario. If the budget has elapsed, `check`
/// panics with a clear message — a runaway multi-step sequence must fail
/// the scenario rather than hang, mirroring the per-command timeout's
/// "fail, not skip" contract.
#[derive(Debug, Clone)]
pub struct ScenarioBudget {
    limit: Option<Duration>,
    start: std::time::Instant,
}

impl ScenarioBudget {
    pub fn new(limit: Option<Duration>) -> Self {
        Self {
            limit,
            start: std::time::Instant::now(),
        }
    }

    /// Elapsed wall-clock since the budget started.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Remaining whole-scenario time, or `None` when there is no total budget.
    pub fn remaining(&self) -> Option<Duration> {
        self.limit
            .map(|limit| limit.saturating_sub(self.start.elapsed()))
    }

    /// Cap a command's timeout to `min(step_timeout, remaining_total)`.
    pub fn cap_timeout(&self, step_timeout: Duration) -> Duration {
        self.remaining()
            .map_or(step_timeout, |remaining| step_timeout.min(remaining))
    }

    /// Panic if the budget has elapsed. A no-op when there is no total
    /// budget. Call this before each step of a multi-step scenario.
    pub fn check(&self, scenario_name: &str) {
        if let Some(limit) = self.limit
            && self.start.elapsed() >= limit
        {
            panic!(
                "scenario '{scenario_name}' exceeded total timeout {limit:?} \
                 (elapsed {:?}); a multi-step sequence must terminate within \
                 the whole-scenario budget",
                self.start.elapsed()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_with_no_limit_is_noop_and_never_panics() {
        let b = ScenarioBudget::new(None);
        b.check("no-limit");
        b.check("no-limit");
        assert!(b.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn budget_with_elapsed_limit_panics() {
        let b = ScenarioBudget::new(Some(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        let result = std::panic::catch_unwind(|| b.check("expired"));
        assert!(
            result.is_err(),
            "an expired budget must panic, not silently pass"
        );
    }

    #[test]
    fn budget_within_limit_does_not_panic() {
        let b = ScenarioBudget::new(Some(Duration::from_secs(60)));
        b.check("fresh");
    }

    #[test]
    fn remaining_total_budget_caps_later_command_timeout() {
        let b = ScenarioBudget {
            limit: Some(Duration::from_secs(5)),
            start: std::time::Instant::now() - Duration::from_secs(3),
        };
        let capped = b.cap_timeout(Duration::from_secs(30));
        assert!(capped <= Duration::from_secs(2));
        assert!(capped > Duration::from_secs(1));
        assert_eq!(
            ScenarioBudget::new(None).cap_timeout(Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn spec_total_timeout_round_trips() {
        let spec = ScenarioSpec {
            name: "x".into(),
            layer: ScenarioLayer::PtySmoke,
            step_timeout_secs: 30,
            total_timeout_secs: Some(180),
        };
        assert_eq!(spec.total_timeout(), Some(Duration::from_secs(180)));
        let budget = spec.start_budget();
        budget.check("x");
    }

    #[test]
    fn spec_without_total_timeout_yields_no_budget() {
        let spec = ScenarioSpec::fast("fast");
        assert!(spec.total_timeout().is_none());
        let budget = spec.start_budget();
        budget.check("fast");
    }

    #[test]
    fn direct_local_spec_has_a_whole_scenario_budget() {
        let spec = ScenarioSpec::direct_local("direct");
        assert_eq!(spec.total_timeout(), Some(Duration::from_secs(180)));
    }
}
