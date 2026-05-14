//! Plugin DAG configuration types.

use serde::{Deserialize, Serialize};

pub use lattice_core::handoff::{eval_rules, HandoffCondition, HandoffRule, HandoffTarget};
use lattice_core::types::BehaviorMode;

/// Plugins configuration for an agent profile.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginsConfig {
    pub entry: String,
    #[serde(default)]
    pub slots: Vec<PluginSlotConfig>,
    #[serde(default)]
    pub edges: Vec<DagEdgeConfig>,
    #[serde(default)]
    pub shared_tools: Vec<String>,
}

/// A single plugin slot in an agent-local plugin DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSlotConfig {
    pub name: String,
    pub plugin: String,
    #[serde(default)]
    pub tools: Vec<String>,
    pub model_override: Option<String>,
    pub max_turns: Option<u32>,
    pub behavior: Option<BehaviorMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagEdgeConfig {
    pub from: String,
    pub rule: HandoffRule,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_output() -> serde_json::Value {
        serde_json::json!({
            "confidence": 0.85,
            "issues": [
                {"severity": "minor", "file": "src/a.rs"},
                {"severity": "critical", "file": "src/b.rs"}
            ],
            "summary": "Code looks good overall"
        })
    }

    #[test]
    fn evals_numeric_and_contains_conditions() {
        let output = json_output();
        let confidence = HandoffRule {
            condition: Some(HandoffCondition {
                field: "confidence".into(),
                op: ">".into(),
                value: serde_json::json!("0.5"),
            }),
            all: None,
            any: None,
            default: false,
            target: Some(HandoffTarget::Single("next".into())),
        };
        let summary = HandoffRule {
            condition: Some(HandoffCondition {
                field: "summary".into(),
                op: "contains".into(),
                value: serde_json::json!("good"),
            }),
            all: None,
            any: None,
            default: false,
            target: Some(HandoffTarget::Single("next".into())),
        };

        assert!(confidence.eval(&output));
        assert!(summary.eval(&output));
    }

    #[test]
    fn evals_any_array_path() {
        let rule = HandoffRule {
            condition: Some(HandoffCondition {
                field: "issues[any].severity".into(),
                op: "==".into(),
                value: serde_json::json!("critical"),
            }),
            all: None,
            any: None,
            default: false,
            target: Some(HandoffTarget::Single("security".into())),
        };

        assert!(rule.eval(&json_output()));
    }

    #[test]
    fn first_matching_rule_wins() {
        let rules = vec![
            HandoffRule {
                condition: Some(HandoffCondition {
                    field: "confidence".into(),
                    op: ">".into(),
                    value: serde_json::json!("0.9"),
                }),
                all: None,
                any: None,
                default: false,
                target: Some(HandoffTarget::Single("agent-a".into())),
            },
            HandoffRule {
                condition: Some(HandoffCondition {
                    field: "confidence".into(),
                    op: ">".into(),
                    value: serde_json::json!("0.5"),
                }),
                all: None,
                any: None,
                default: false,
                target: Some(HandoffTarget::Single("agent-b".into())),
            },
        ];

        assert_eq!(
            eval_rules(&rules, &json_output()),
            Some(HandoffTarget::Single("agent-b".into()))
        );
    }
}
