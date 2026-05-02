use lattice_core::retry::RetryPolicy;
use lattice_harness::dag_runner::PluginDagRunner;
use lattice_harness::handoff_rule::{HandoffCondition, HandoffRule, HandoffTarget};
use lattice_harness::profile::{AgentEdgeConfig, PluginSlotConfig, PluginsConfig};
use lattice_harness::tools::ToolRegistry;
use lattice_plugin::builtin::code_review::CodeReviewPlugin;
use lattice_plugin::builtin::refactor::RefactorPlugin;
use lattice_plugin::bundle::{BehaviorMode, PluginBundle, PluginMeta};
use lattice_plugin::registry::PluginRegistry;

fn setup_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry
        .register(PluginBundle {
            meta: PluginMeta {
                name: "CodeReview".into(),
                version: "0.1".into(),
                description: "Code review plugin".into(),
                author: "lattice".into(),
            },
            plugin: Box::new(CodeReviewPlugin::new()),
            default_behavior: BehaviorMode::Yolo,
            default_tools: vec![],
        })
        .unwrap();
    registry
        .register(PluginBundle {
            meta: PluginMeta {
                name: "Refactor".into(),
                version: "0.1".into(),
                description: "Refactor plugin".into(),
                author: "lattice".into(),
            },
            plugin: Box::new(RefactorPlugin::new()),
            default_behavior: BehaviorMode::Yolo,
            default_tools: vec![],
        })
        .unwrap();
    registry
}

#[test]
fn test_plugin_dag_construction_and_edge_matching() {
    let registry = setup_registry();
    let tool_registry = ToolRegistry::new();

    let config = PluginsConfig {
        entry: "review".into(),
        shared_tools: vec![],
        slots: vec![
            PluginSlotConfig {
                name: "review".into(),
                plugin: "CodeReview".into(),
                tools: vec![],
                model_override: None,
                max_turns: Some(3),
                behavior: None,
            },
            PluginSlotConfig {
                name: "refactor".into(),
                plugin: "Refactor".into(),
                tools: vec![],
                model_override: None,
                max_turns: Some(5),
                behavior: None,
            },
        ],
        edges: vec![
            AgentEdgeConfig {
                from: "review".into(),
                rule: HandoffRule {
                    condition: Some(HandoffCondition {
                        field: "confidence".into(),
                        op: ">".into(),
                        value: serde_json::json!(0.5),
                    }),
                    all: None,
                    any: None,
                    default: false,
                    target: Some(HandoffTarget::Single("refactor".into())),
                },
            },
            AgentEdgeConfig {
                from: "review".into(),
                rule: HandoffRule {
                    condition: None,
                    all: None,
                    any: None,
                    default: true,
                    target: None,
                },
            },
            AgentEdgeConfig {
                from: "refactor".into(),
                rule: HandoffRule {
                    condition: None,
                    all: None,
                    any: None,
                    default: true,
                    target: None,
                },
            },
        ],
    };

    let dag = PluginDagRunner::new(
        &config,
        &registry,
        &tool_registry,
        RetryPolicy::default(),
        None,
    );

    // High confidence → match first edge → refactor
    let output = serde_json::json!({"confidence": 0.9, "issues": []});
    let next = dag.find_edge("review", &output);
    assert_eq!(next, Some(HandoffTarget::Single("refactor".into())));

    // Low confidence → match default (no target → endpoint)
    let output = serde_json::json!({"confidence": 0.3, "issues": []});
    let next = dag.find_edge("review", &output);
    assert_eq!(next, None);
}

#[test]
fn test_dag_error_types() {
    use lattice_harness::dag_runner::DAGError;

    let err = DAGError::SlotNotFound("test".into());
    assert!(err.to_string().contains("test"));

    let err = DAGError::ForkNotSupportedInDag;
    assert!(err.to_string().contains("fork"));
}

#[test]
fn test_dag_config_entry_validation_logic() {
    // Config with entry pointing to non-existent slot
    let config = PluginsConfig {
        entry: "nonexistent".into(),
        slots: vec![PluginSlotConfig {
            name: "review".into(),
            plugin: "CodeReview".into(),
            tools: vec![],
            model_override: None,
            max_turns: None,
            behavior: None,
        }],
        edges: vec![],
        shared_tools: vec![],
    };

    let slot_names: Vec<&str> = config.slots.iter().map(|s| s.name.as_str()).collect();
    assert!(!slot_names.contains(&config.entry.as_str()));
}

#[test]
fn test_behavior_mode_to_behavior_roundtrip() {
    use lattice_plugin::bundle::BehaviorMode;

    let strict = BehaviorMode::Strict {
        confidence_threshold: 0.8,
        max_retries: 2,
        escalate_to: Some("human".into()),
    };
    let behavior = strict.to_behavior();
    assert!(matches!(behavior.decide(0.9), lattice_plugin::Action::Done));
    assert!(matches!(
        behavior.decide(0.5),
        lattice_plugin::Action::Retry
    ));
    assert!(matches!(
        behavior.on_error(&lattice_plugin::PluginError::Parse("x".into()), 3),
        lattice_plugin::ErrorAction::Escalate
    ));

    let yolo = BehaviorMode::Yolo;
    let behavior = yolo.to_behavior();
    assert!(matches!(behavior.decide(0.1), lattice_plugin::Action::Done));
}
