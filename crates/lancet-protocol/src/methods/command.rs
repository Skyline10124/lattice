//! Command execution methods.

use crate::envelope::ImpactKind;
use crate::method::{CatalogMethod, CepMethod, RequiredCapability};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `command.run`
pub struct Run;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunParams {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_impact: Option<ImpactKind>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CepMethod for Run {
    const NAME: &'static str = "command.run";
    const DESCRIPTION: &'static str =
        "Run a command with argv, optional cwd/env/timeout, and declared impact metadata.";
    type Params = RunParams;
    type Result = RunResult;
    const IMPACT: ImpactKind = ImpactKind::CommandExecution;
    const CAPABILITY: RequiredCapability = RequiredCapability::RunCommand;
}

impl CatalogMethod for Run {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_run_name_is_dotted() {
        assert_eq!(Run::NAME, "command.run");
        assert!(Run::NAME.contains('.'));
        assert!(!Run::NAME.contains('/'));
    }
}
