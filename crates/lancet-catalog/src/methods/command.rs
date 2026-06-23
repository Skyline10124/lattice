// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Command execution methods.

use crate::method::{CatalogMethod, CepMethod};
use lancet_protocol::RequiredCapability;
use lancet_protocol::{
    EffectDecl, EffectKindSet, EffectScopeKind, FailureSemantics, ImpactKind, OverlayBehavior,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `exec.command_run`
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
    const NAME: &'static str = "exec.command_run";
    const DESCRIPTION: &'static str = "Run a command with argv, optional cwd/env/timeout, and declared_impact metadata. Use JSON argv arrays; shell strings are accepted only as the exact carrier [\"sh\",\"-c\",\"<script>\"] and only when policy enables inline shell. An inline script must be a single line of literal-word simple commands joined by '|', '&&', '||', ';' (e.g. \"cargo check && cargo test\"); each sub-command is classified and allowlist-checked individually, and the most restrictive classification applies to the whole script. Scripts containing $variables, $(substitution), backticks, redirections (> < 2>&1), unquoted globs (* ? [ ]), ~, &, environment assignment prefixes (FOO=bar cmd), or shell builtins (eval, exec, source, set, ...) are rejected; run those needs as separate argv calls instead of retrying variants. Diagnostic commands such as cargo fmt --check, cargo check/test/clippy/doc, and git diff/status/log/show should use declared_impact workspace_observation or omit it. Unknown or possibly mutating commands must declare workspace_mutation and require RunCommand plus any needed WritePath scope; cargo fmt without --check and cargo commands that intentionally update Cargo.lock are workspace mutations. Do not use command_execution to claim an unknown command is read-only; policy rejects that as too weak. In command isolation, cargo uses the mounted read-only Cargo cache with offline semantics; Cargo.lock is not a cache inventory. If a new dependency is not cached, repeated cargo retries cannot fetch it. If command isolation fails closed, treat that as terminal for equivalent exec.command_run attempts under the current host/profile and use non-command mutation tools for file changes.";
    type Params = RunParams;
    type Result = RunResult;
    const IMPACT: ImpactKind = ImpactKind::CommandExecution;
    const CAPABILITY: RequiredCapability = RequiredCapability::RunCommand;
    // exec.command_run may execute a command and, for workspace-mutation commands,
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
        overlay: OverlayBehavior::CommandOpaque,
        failure: FailureSemantics::PartialEffectsAudited,
    };
}

impl CatalogMethod for Run {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_run_name_is_dotted() {
        assert_eq!(Run::NAME, "exec.command_run");
        assert!(Run::NAME.contains('.'));
        assert!(!Run::NAME.contains('/'));
    }

    #[test]
    fn command_run_description_guides_declared_impact() {
        assert!(Run::DESCRIPTION.contains("declared_impact"));
        assert!(Run::DESCRIPTION.contains("workspace_observation"));
        assert!(Run::DESCRIPTION.contains("workspace_mutation"));
        assert!(Run::DESCRIPTION.contains("cargo fmt --check"));
        assert!(Run::DESCRIPTION.contains("Cargo.lock"));
        assert!(Run::DESCRIPTION.contains("not a cache inventory"));
        assert!(Run::DESCRIPTION.contains("offline semantics"));
        assert!(Run::DESCRIPTION.contains("fails closed"));
    }

    #[test]
    fn command_run_description_documents_inline_shell_contract() {
        assert!(Run::DESCRIPTION.contains("[\"sh\",\"-c\",\"<script>\"]"));
        assert!(Run::DESCRIPTION.contains("only when policy enables inline shell"));
        assert!(Run::DESCRIPTION.contains("literal-word simple commands"));
        assert!(Run::DESCRIPTION.contains("classified and allowlist-checked individually"));
        assert!(Run::DESCRIPTION.contains("separate argv calls"));
    }

    #[test]
    fn command_run_contract_has_no_supervisor_fields() {
        let params = serde_json::to_value(RunParams {
            argv: vec!["echo".into()],
            cwd: Some(".".into()),
            env: BTreeMap::new(),
            timeout_ms: Some(1000),
            declared_impact: Some(ImpactKind::CommandExecution),
        })
        .unwrap();
        let result = serde_json::to_value(RunResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: None,
            stderr_truncated: None,
            stdout_bytes: Some(0),
            stderr_bytes: Some(0),
        })
        .unwrap();

        for forbidden in [
            "supervisor",
            "effective_boundary",
            "host_probe_summary",
            "boundary_setup_failed_closed",
            "backend_invoked",
        ] {
            assert!(
                params.get(forbidden).is_none(),
                "RunParams leaked metadata field {forbidden}"
            );
            assert!(
                result.get(forbidden).is_none(),
                "RunResult leaked metadata field {forbidden}"
            );
        }
    }
}
