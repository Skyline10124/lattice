// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

/// 验收套件的测试分层（见 RFC #41，Codeberg #19）。
///
/// 自 #62（Codeberg #40）起已移除沙盒虚拟层：所有场景均直接针对规范化项目根目录以本地直连方式运行，
/// 不依赖 KVM、沙盒后台或 OCI 镜像；`PtySmoke` 保留为 TUI 入口冒烟层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioLayer {
    FastBinary,
    PtySmoke,
}

/// 场景元数据（内联构造或从 `tests/scenarios/` 下的 JSON 夹具加载）。
///
/// `step_timeout_secs` 是 `LatticeDriver::run` 对该规格派生的每次命令调用强制执行的单命令超时上限。
/// `total_timeout_secs` 是可选的整场景墙钟预算（需求用户故事 19，RFC #41 决策八）：
/// 多步骤场景在开始时记录起点，并在每一步前断言预算尚未耗尽，使每步虽在单步超时内返回但永不收敛的步骤序列仍然有界。
/// 默认值 `None` 仅启用单命令超时，保留 #42（Codeberg #20）快速场景的单步行为。
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
    #[error("读取场景夹具失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("场景夹具格式无效: {0}")]
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

    /// 同时具备单步超时与整场景总预算的多步骤本地直连场景。
    pub fn direct_local(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            layer: ScenarioLayer::FastBinary,
            step_timeout_secs: default_step_timeout_secs(),
            total_timeout_secs: Some(180),
        }
    }

    /// 由 `LatticeDriver::run` 强制执行的单命令超时预算。
    pub fn step_timeout(&self) -> Duration {
        Duration::from_secs(self.step_timeout_secs)
    }

    /// 整场景墙钟总预算；若仅单命令超时生效则返回 `None`。
    pub fn total_timeout(&self) -> Option<Duration> {
        self.total_timeout_secs.map(Duration::from_secs)
    }

    /// 从本 crate 的 `tests/scenarios/` 目录加载场景 JSON 夹具。
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

    /// 多步骤场景在每步之前调用；预算耗尽即触发 panic，没有总预算时不作检查。
    pub fn check(&self, scenario_name: &str) {
        if let Some(limit) = self.limit
            && self.start.elapsed() >= limit
        {
            panic!(
                "场景 '{scenario_name}' 超出总预算 {limit:?}（已耗时 {:?}）；多步骤序列必须在整场景预算内结束",
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
