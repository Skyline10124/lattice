//! Command execution methods.

use crate::method::{CatalogMethod, CepMethod};
use lattice_cep_protocol::RequiredCapability;
use lattice_cep_protocol::{EffectDecl, EffectKindSet, EffectScopeKind, ImpactKind};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_bytes: Option<usize>,
}

impl CepMethod for Run {
    const NAME: &'static str = "command.run";
    const DESCRIPTION: &'static str =
        "Run a command with argv, optional cwd/env/timeout, and declared impact metadata.";
    type Params = RunParams;
    type Result = RunResult;
    const IMPACT: ImpactKind = ImpactKind::CommandExecution;
    const CAPABILITY: RequiredCapability = RequiredCapability::RunCommand;
    // command.run may execute a command and, for workspace-mutation commands,
    // mutate files within the declared scope.
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[
            ImpactKind::WorkspaceMutation,
            ImpactKind::CommandExecution,
        ]),
        scope: EffectScopeKind::CommandExecution,
        capabilities: &[
            RequiredCapability::RunCommand,
            RequiredCapability::WritePath,
        ],
    };
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
