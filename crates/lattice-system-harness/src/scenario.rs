// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

/// Harness test layer (see RFC #41, Codeberg #19).
///
/// After #62 (Codeberg #40) there is no Microsandbox layer: every scenario
/// runs Direct-Local against the canonical project root — no KVM, no MSB
/// daemon, no OCI image. `PtySmoke` remains as the TUI-entrypoint layer.
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
/// (PRD user story 19, RFC #41 decision eight): a multi-step scenario
/// records its start time and asserts the budget has not elapsed before each
/// step, so a sequence of steps that each finish within their step timeout
/// but never terminates is still bounded. `None` (the default) leaves only
/// the per-command timeout in effect, preserving the #42 (Codeberg #20)
/// single-step behavior for fast scenarios.
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

    /// 默认单命令上限；未登记预算的场景收尾时沿用。
    pub(crate) fn default_step_timeout() -> Duration {
        Duration::from_secs(default_step_timeout_secs())
    }
}

/// 整个场景的墙钟预算。场景内唯一起点由 [`crate::ScenarioWorld::budget`] 登记并共享，
/// 副本不会重新计时。多步骤场景在每步之前调用 `check`；预算耗尽时明确失败，
/// 与单命令超时一样，不跳过测试或让失控的步骤序列无限等待。
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

    /// 自预算起点起经过的墙钟时间。
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// 整个场景的剩余时间；没有总预算时返回 `None`。
    pub fn remaining(&self) -> Option<Duration> {
        self.limit
            .map(|limit| limit.saturating_sub(self.start.elapsed()))
    }

    /// 将命令上限约束为单步上限与场景剩余时间中的较小值。
    pub fn cap_timeout(&self, step_timeout: Duration) -> Duration {
        self.remaining()
            .map_or(step_timeout, |remaining| step_timeout.min(remaining))
    }

    /// 将步骤绝对期限限制在场景原始期限内，避免扫描或锁等待后重置剩余量。
    pub(crate) fn deadline(&self, step_deadline: std::time::Instant) -> std::time::Instant {
        self.limit
            .map_or(step_deadline, |limit| step_deadline.min(self.start + limit))
    }

    /// 多步骤场景在每步之前调用；预算耗尽即 panic，没有总预算时不作检查。
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
    fn direct_local_spec_has_a_whole_scenario_budget() {
        let spec = ScenarioSpec::direct_local("direct");
        assert_eq!(spec.total_timeout(), Some(Duration::from_secs(180)));
    }
}
