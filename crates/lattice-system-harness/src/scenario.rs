// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

/// Harness test layer (see RFC #19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioLayer {
    FastBinary,
    Microsandbox,
    PtySmoke,
}

/// Scenario metadata (inline builder or JSON fixture under `tests/scenarios/`).
///
/// `step_timeout_secs` is the per-command timeout enforced by
/// `LatticeDriver::run` for every invocation derived from this spec. There is
/// intentionally no separate "total" timeout field: the harness has no
/// multi-step runner, so a total-timeout would be dead config with no
/// semantics. Per-command timeout is the only timeout the driver actually
/// enforces.
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioSpec {
    pub name: String,
    pub layer: ScenarioLayer,
    #[serde(default = "default_step_timeout_secs")]
    pub step_timeout_secs: u64,
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
        }
    }

    /// Per-command timeout enforced by `LatticeDriver::run`.
    pub fn step_timeout(&self) -> Duration {
        Duration::from_secs(self.step_timeout_secs)
    }

    /// Load a scenario JSON fixture from the harness crate `tests/scenarios/` tree.
    pub fn load_fixture(relative: &str) -> Result<Self, ScenarioError> {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios");
        let path = base.join(relative);
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }
}
