//! Command execution methods.

use crate::method::{CatalogMethod, CepMethod};
use lancet_protocol::RequiredCapability;
use lancet_protocol::{EffectDecl, EffectKindSet, EffectScopeKind, ImpactKind};
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
    const DESCRIPTION: &'static str = "Run a command with argv, optional cwd/env/timeout, and declared_impact metadata. Use JSON argv arrays, never shell strings. Diagnostic commands such as cargo check/test/clippy/doc and git diff/status/log/show should use declared_impact workspace_observation or omit it. Unknown or possibly mutating commands must declare workspace_mutation and require an active envelope with RunCommand plus any needed WritePath scope. Do not use command_execution to claim an unknown command is read-only; policy rejects that as too weak.";
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

    #[test]
    fn command_run_description_guides_declared_impact() {
        assert!(Run::DESCRIPTION.contains("declared_impact"));
        assert!(Run::DESCRIPTION.contains("workspace_observation"));
        assert!(Run::DESCRIPTION.contains("workspace_mutation"));
        assert!(Run::DESCRIPTION.contains("never shell strings"));
    }
}
