//! Compatibility adapter for the canonical handoff rule Module.

pub use lattice_core::handoff::{eval_rules, HandoffCondition, HandoffRule, HandoffTarget};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexported_rules_parse_and_eval() {
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "confidence", op = ">", value = "0.5" }
target = "next-agent"
"#,
        )
        .unwrap();
        let output = serde_json::json!({ "confidence": 0.85 });

        assert!(rule.eval(&output));
        assert_eq!(
            eval_rules(&[rule], &output),
            Some(HandoffTarget::Single("next-agent".into()))
        );
    }
}
