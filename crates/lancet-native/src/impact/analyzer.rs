// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::impact::layer1::{derive_layer1, Layer1Result};
use crate::impact::layer2::{derive_layer2, Layer2Result};
use lancet_protocol::envelope::{Impact, ImpactConfidence, ImpactScope, ImpactUnknown};

pub struct ImpactAnalyzer;

impl ImpactAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn analyze(
        &self,
        method_name: &str,
        target: &str,
        params: Option<&serde_json::Value>,
    ) -> AnalysisResult {
        let layer1 = derive_layer1(method_name, target);
        let layer2 = derive_layer2(&layer1, target, params);
        AnalysisResult::from_layers(layer1, layer2, target)
    }
}

impl Default for ImpactAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AnalysisResult {
    pub impact: Impact,
    pub scope: ImpactScope,
    pub confidence: ImpactConfidence,
    pub unknowns: Vec<ImpactUnknown>,
}

impl AnalysisResult {
    fn from_layers(layer1: Layer1Result, layer2: Layer2Result, target: &str) -> Self {
        let impact = Impact {
            kind: layer1.kind,
            target: target.to_string(),
            scope: Some(layer2.scope.clone()),
            confidence: layer2.confidence,
            unknowns: layer2.unknowns.clone(),
        };
        Self {
            impact,
            scope: layer2.scope,
            confidence: layer2.confidence,
            unknowns: layer2.unknowns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyzer_end_to_end_create() {
        let analyzer = ImpactAnalyzer::new();
        let result = analyzer.analyze("mutate.create", "src/main.rs", None);
        assert_eq!(
            result.impact.kind,
            lancet_protocol::envelope::ImpactKind::WorkspaceMutation
        );
        assert_eq!(result.impact.target, "src/main.rs");
        assert_eq!(result.confidence, ImpactConfidence::Complete);
        assert!(matches!(result.scope, ImpactScope::ExactFile(_)));
    }

    #[test]
    fn test_analyzer_end_to_end_grep_glob() {
        let analyzer = ImpactAnalyzer::new();
        let result = analyzer.analyze("observe.workspace", "src/**/*.rs", None);
        assert_eq!(
            result.impact.kind,
            lancet_protocol::envelope::ImpactKind::WorkspaceObservation
        );
        assert_eq!(result.confidence, ImpactConfidence::Conservative);
        assert!(matches!(result.scope, ImpactScope::Directory(_)));
    }

    #[test]
    fn test_analyzer_end_to_end_command_run() {
        let analyzer = ImpactAnalyzer::new();
        let result = analyzer.analyze("exec.command_run", "echo hello", None);
        assert_eq!(
            result.impact.kind,
            lancet_protocol::envelope::ImpactKind::CommandExecution
        );
    }
}
