use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::warn;

// ---------------------------------------------------------------------------
// HandoffTarget — single or parallel routing target
// ---------------------------------------------------------------------------

/// The target of a handoff rule.  `Single("refactor")` routes to one agent;
/// `Fork(["security","performance"])` runs multiple agents in parallel and
/// merges their outputs.
///
/// TOML syntax: `target = "refactor"` or `target = "fork:security,performance"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffTarget {
    Single(String),
    Fork(Vec<String>),
}

impl std::fmt::Display for HandoffTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandoffTarget::Single(name) => write!(f, "{}", name),
            HandoffTarget::Fork(names) => write!(f, "fork:{}", names.join(",")),
        }
    }
}

impl HandoffTarget {
    /// Parse a target string.  `"fork:A,B"` → `Fork(["A","B"])`,
    /// anything else → `Single(s)`.
    pub fn parse(s: &str) -> Self {
        if let Some(rest) = s.strip_prefix("fork:") {
            let targets = rest.split(',').map(|t| t.trim().to_string()).collect();
            HandoffTarget::Fork(targets)
        } else {
            HandoffTarget::Single(s.to_string())
        }
    }

    /// Return all agent names referenced by this target.
    pub fn agent_names(&self) -> Vec<&str> {
        match self {
            HandoffTarget::Single(name) => vec![name],
            HandoffTarget::Fork(names) => names.iter().map(|s| s.as_str()).collect(),
        }
    }
}

impl Serialize for HandoffTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            HandoffTarget::Single(name) => serializer.serialize_str(name),
            HandoffTarget::Fork(names) => {
                serializer.serialize_str(&format!("fork:{}", names.join(",")))
            }
        }
    }
}

impl<'de> Deserialize<'de> for HandoffTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(HandoffTarget::parse(&s))
    }
}

// ---------------------------------------------------------------------------
// HandoffRule — TOML-based agent routing rules
// ---------------------------------------------------------------------------

/// A single condition: `field op value`.
///
/// `field` is a dotted path into the JSON output, e.g. `"confidence"` or
/// `"issues[0].severity"`.  Array indices and `[any]` (match any element)
/// are supported.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HandoffCondition {
    pub field: String,
    pub op: String,
    pub value: serde_json::Value,
}

/// One routing rule.  A rule matches when:
/// - `condition` is set and evaluates to `true`, OR
/// - all conditions in `all` evaluate to `true`, OR
/// - any condition in `any` evaluates to `true`, OR
/// - `default` is `true` (unconditional fallback).
///
/// Rules are evaluated in order; the first match wins.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HandoffRule {
    #[serde(default)]
    pub condition: Option<HandoffCondition>,

    #[serde(default)]
    pub all: Option<Vec<HandoffCondition>>,

    #[serde(default)]
    pub any: Option<Vec<HandoffCondition>>,

    #[serde(default)]
    pub default: bool,

    /// `None` means the pipeline ends (no further agent).
    pub target: Option<HandoffTarget>,
}

impl HandoffRule {
    /// Evaluate this rule against `output` (the JSON value returned by the
    /// previous agent).  Returns `true` when the rule matches.
    pub fn eval(&self, output: &serde_json::Value) -> bool {
        if self.default {
            return true;
        }
        if let Some(ref c) = self.condition {
            return eval_condition(c, output);
        }
        if let Some(ref all) = self.all {
            if all.is_empty() {
                return false;
            }
            return all.iter().all(|c| eval_condition(c, output));
        }
        if let Some(ref any) = self.any {
            return any.iter().any(|c| eval_condition(c, output));
        }
        false
    }
}

/// Resolve a dotted field path like `"confidence"` or `"issues[0].severity"`
/// against a `serde_json::Value`.
fn resolve_field<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let segments = parse_path(path);
    let mut current = root;
    for seg in &segments {
        current = match seg {
            PathSegment::Key(k) => current.get(k)?,
            PathSegment::Index(i) => current.get(*i)?,
            PathSegment::Any => {
                // [any] — return the array itself so the caller can iterate
                return Some(current);
            }
        };
    }
    Some(current)
}

enum PathSegment {
    Key(String),
    Index(usize),
    Any,
}

fn parse_path(path: &str) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    let mut current = String::new();

    for ch in path.chars() {
        match ch {
            '.' => {
                if !current.is_empty() {
                    segments.push(PathSegment::Key(std::mem::take(&mut current)));
                }
            }
            '[' => {
                if !current.is_empty() {
                    segments.push(PathSegment::Key(std::mem::take(&mut current)));
                }
            }
            ']' => {
                if current == "any" {
                    segments.push(PathSegment::Any);
                } else if let Ok(i) = current.parse::<usize>() {
                    segments.push(PathSegment::Index(i));
                } else {
                    warn!("Malformed path segment [{current}] in handoff condition — skipping");
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        segments.push(PathSegment::Key(current));
    }
    segments
}

/// Evaluate a single condition against `output`.
fn eval_condition(cond: &HandoffCondition, output: &serde_json::Value) -> bool {
    // If the path contains [any], split into prefix (→ array) + suffix (→ per-element)
    if let Some((prefix, suffix)) = split_at_any(&cond.field) {
        let arr = match resolve_field(output, &prefix) {
            Some(serde_json::Value::Array(arr)) => arr,
            _ => return false,
        };
        return arr.iter().any(|elem| match resolve_field(elem, &suffix) {
            Some(v) => eval_operator(v, cond),
            None => false,
        });
    }

    let field_val = match resolve_field(output, &cond.field) {
        Some(v) => v,
        None => return false,
    };
    eval_operator(field_val, cond)
}

/// Split `issues[any].severity` into `("issues", "severity")`.
fn split_at_any(path: &str) -> Option<(String, String)> {
    let segments = parse_path(path);
    let any_pos = segments
        .iter()
        .position(|s| matches!(s, PathSegment::Any))?;
    let prefix = rebuild_path(&segments[..any_pos]);
    let suffix = rebuild_path(&segments[any_pos + 1..]);
    Some((prefix, suffix))
}

fn rebuild_path(segments: &[PathSegment]) -> String {
    segments.iter().fold(String::new(), |mut s, seg| {
        match seg {
            PathSegment::Key(k) => {
                if !s.is_empty() {
                    s.push('.');
                }
                s.push_str(k);
            }
            PathSegment::Index(i) => s.push_str(&format!("[{i}]")),
            PathSegment::Any => s.push_str("[any]"),
        }
        s
    })
}

/// Apply the condition operator to a single value (no array iteration).
fn eval_operator(field_val: &serde_json::Value, cond: &HandoffCondition) -> bool {
    match cond.op.as_str() {
        "==" => values_equal(field_val, &cond.value),
        "!=" => !values_equal(field_val, &cond.value),
        "<" => compare_values(field_val, &cond.value) == Some(std::cmp::Ordering::Less),
        ">" => compare_values(field_val, &cond.value) == Some(std::cmp::Ordering::Greater),
        "<=" => matches!(
            compare_values(field_val, &cond.value),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        ),
        ">=" => matches!(
            compare_values(field_val, &cond.value),
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        ),
        "contains" => string_contains(field_val, &cond.value),
        _ => false,
    }
}

fn values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::String(s), serde_json::Value::String(t)) => s == t,
        (serde_json::Value::Number(n), serde_json::Value::String(s)) => {
            if let Ok(f) = s.parse::<f64>() {
                n.as_f64().is_some_and(|nf| (nf - f).abs() < f64::EPSILON)
            } else {
                false
            }
        }
        (serde_json::Value::String(s), serde_json::Value::Number(n)) => {
            if let Ok(f) = s.parse::<f64>() {
                n.as_f64().is_some_and(|nf| (nf - f).abs() < f64::EPSILON)
            } else {
                false
            }
        }
        (serde_json::Value::Bool(b1), serde_json::Value::Bool(b2)) => b1 == b2,
        (serde_json::Value::Bool(b), serde_json::Value::String(s)) => {
            s.parse::<bool>().is_ok_and(|b2| *b == b2)
        }
        (serde_json::Value::Null, serde_json::Value::Null) => true,
        _ => a == b,
    }
}

fn compare_values(a: &serde_json::Value, b: &serde_json::Value) -> Option<std::cmp::Ordering> {
    let na = to_f64(a)?;
    let nb = match b {
        serde_json::Value::String(s) => s.parse::<f64>().ok()?,
        serde_json::Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    na.partial_cmp(&nb)
}

fn to_f64(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

fn string_contains(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let sa = match a {
        serde_json::Value::String(s) => s.as_str(),
        _ => return false,
    };
    let sb = match b {
        serde_json::Value::String(s) => s.as_str(),
        other => &other.to_string(),
    };
    sa.contains(sb)
}

/// Evaluate a list of rules in order; return the target of the first match.
pub fn eval_rules(rules: &[HandoffRule], output: &serde_json::Value) -> Option<HandoffTarget> {
    rules
        .iter()
        .find(|r| r.eval(output))
        .and_then(|r| r.target.clone())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    // --- condition parsing ---

    #[test]
    fn test_parse_single_condition() {
        let toml = r#"
condition = { field = "confidence", op = ">", value = "0.5" }
target = "next-agent"
"#;
        let rule: HandoffRule = toml::from_str(toml).unwrap();
        assert!(rule.condition.is_some());
        assert!(rule.all.is_none());
        assert!(rule.any.is_none());
        assert!(!rule.default);
        assert_eq!(
            rule.target,
            Some(HandoffTarget::Single("next-agent".into()))
        );
    }

    #[test]
    fn test_parse_all_conditions() {
        let toml = r#"
all = [
  { field = "confidence", op = ">", value = "0.5" },
  { field = "summary", op = "contains", value = "good" },
]
target = "next-agent"
"#;
        let rule: HandoffRule = toml::from_str(toml).unwrap();
        let all = rule.all.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].field, "confidence");
        assert_eq!(all[1].op, "contains");
    }

    #[test]
    fn test_parse_any_conditions() {
        let toml = r#"
any = [
  { field = "issues[0].severity", op = "==", value = "critical" },
  { field = "confidence", op = "<", value = "0.5" },
]
target = "review"
"#;
        let rule: HandoffRule = toml::from_str(toml).unwrap();
        let any = rule.any.unwrap();
        assert_eq!(any.len(), 2);
    }

    #[test]
    fn test_parse_default_rule() {
        let toml = r#"
default = true
"#;
        let rule: HandoffRule = toml::from_str(toml).unwrap();
        assert!(rule.default);
        assert!(rule.target.is_none());
    }

    // --- single condition eval ---

    #[test]
    fn test_eval_op_gt_true() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "confidence", op = ">", value = "0.5" }
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_eval_op_gt_false() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "confidence", op = ">", value = "0.9" }
target = "next"
"#,
        )
        .unwrap();
        assert!(!rule.eval(&output));
    }

    #[test]
    fn test_eval_op_equals() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "issues[0].severity", op = "==", value = "minor" }
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_eval_op_not_equal() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "confidence", op = "!=", value = "0" }
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_eval_op_contains() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "summary", op = "contains", value = "good" }
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_eval_op_contains_false() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "summary", op = "contains", value = "terrible" }
target = "next"
"#,
        )
        .unwrap();
        assert!(!rule.eval(&output));
    }

    #[test]
    fn test_eval_missing_field() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "nonexistent", op = "==", value = "x" }
target = "next"
"#,
        )
        .unwrap();
        assert!(!rule.eval(&output));
    }

    #[test]
    fn test_eval_number_comparison() {
        let output = serde_json::json!({"score": 75});
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "score", op = ">=", value = "70" }
target = "pass"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_eval_bool_equals() {
        let output = serde_json::json!({"passed": true});
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "passed", op = "==", value = "true" }
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    // --- AND compound ---

    #[test]
    fn test_all_both_true() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
all = [
  { field = "confidence", op = ">", value = "0.5" },
  { field = "summary", op = "contains", value = "good" },
]
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_all_one_false() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
all = [
  { field = "confidence", op = ">", value = "0.5" },
  { field = "confidence", op = "<", value = "0.1" },
]
target = "next"
"#,
        )
        .unwrap();
        assert!(!rule.eval(&output));
    }

    #[test]
    fn test_all_empty() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
all = []
target = "next"
"#,
        )
        .unwrap();
        assert!(!rule.eval(&output));
    }

    // --- OR compound ---

    #[test]
    fn test_any_one_true() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
any = [
  { field = "confidence", op = "<", value = "0.5" },
  { field = "summary", op = "contains", value = "good" },
]
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_any_all_false() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
any = [
  { field = "confidence", op = "<", value = "0.5" },
  { field = "summary", op = "contains", value = "terrible" },
]
target = "next"
"#,
        )
        .unwrap();
        assert!(!rule.eval(&output));
    }

    // --- default rule ---

    #[test]
    fn test_default_matches_always() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
default = true
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    // --- rule list evaluation ---

    #[derive(Deserialize)]
    struct RuleList {
        rules: Vec<HandoffRule>,
    }

    #[test]
    fn test_eval_rules_first_match_wins() {
        let output = json_output();
        let list: RuleList = toml::from_str(
            r#"
[[rules]]
condition = { field = "confidence", op = ">", value = "0.5" }
target = "agent-a"

[[rules]]
condition = { field = "confidence", op = ">", value = "0.1" }
target = "agent-b"
"#,
        )
        .unwrap();
        assert_eq!(
            eval_rules(&list.rules, &output),
            Some(HandoffTarget::Single("agent-a".into()))
        );
    }

    #[test]
    fn test_eval_rules_no_match() {
        let output = json_output();
        let list: RuleList = toml::from_str(
            r#"
[[rules]]
condition = { field = "confidence", op = "<", value = "0.1" }
target = "agent-a"
"#,
        )
        .unwrap();
        assert_eq!(eval_rules(&list.rules, &output), None);
    }

    #[test]
    fn test_eval_rules_default_fallback() {
        let output = json_output();
        let list: RuleList = toml::from_str(
            r#"
[[rules]]
condition = { field = "confidence", op = "<", value = "0.1" }
target = "agent-a"

[[rules]]
default = true
target = "fallback-agent"
"#,
        )
        .unwrap();
        assert_eq!(
            eval_rules(&list.rules, &output),
            Some(HandoffTarget::Single("fallback-agent".into()))
        );
    }

    #[test]
    fn test_eval_rules_default_null_ends_pipeline() {
        let output = json_output();
        let list: RuleList = toml::from_str(
            r#"
[[rules]]
condition = { field = "confidence", op = "<", value = "0.1" }
target = "agent-a"

[[rules]]
default = true
"#,
        )
        .unwrap();
        assert_eq!(eval_rules(&list.rules, &output), None);
    }

    // --- nested field access ---

    #[test]
    fn test_nested_array_index() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "issues[1].severity", op = "==", value = "critical" }
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    // --- [any] array matching ---

    #[test]
    fn test_any_array_severity_match() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "issues[any].severity", op = "==", value = "critical" }
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_any_array_no_match() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "issues[any].severity", op = "==", value = "blocker" }
target = "next"
"#,
        )
        .unwrap();
        assert!(!rule.eval(&output));
    }

    #[test]
    fn test_any_array_with_contains() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "issues[any].file", op = "contains", value = "src/b" }
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_string_field_contains() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "issues[1].file", op = "contains", value = "src/b" }
target = "next"
"#,
        )
        .unwrap();
        assert!(rule.eval(&output));
    }

    #[test]
    fn test_invalid_operator() {
        let output = json_output();
        let rule: HandoffRule = toml::from_str(
            r#"
condition = { field = "confidence", op = "??", value = "0.5" }
target = "next"
"#,
        )
        .unwrap();
        assert!(!rule.eval(&output));
    }

    // --- HandoffTarget parsing + fork syntax ---

    #[test]
    fn test_handoff_target_single() {
        assert_eq!(
            HandoffTarget::parse("refactor"),
            HandoffTarget::Single("refactor".into())
        );
    }

    #[test]
    fn test_handoff_target_fork() {
        assert_eq!(
            HandoffTarget::parse("fork:security,performance"),
            HandoffTarget::Fork(vec!["security".into(), "performance".into()])
        );
    }

    #[test]
    fn test_handoff_target_fork_single() {
        assert_eq!(
            HandoffTarget::parse("fork:review"),
            HandoffTarget::Fork(vec!["review".into()])
        );
    }

    #[test]
    fn test_handoff_target_fork_whitespace() {
        assert_eq!(
            HandoffTarget::parse("fork: A , B "),
            HandoffTarget::Fork(vec!["A".into(), "B".into()])
        );
    }

    #[test]
    fn test_handoff_target_agent_names() {
        let single = HandoffTarget::Single("x".into());
        assert_eq!(single.agent_names(), vec!["x"]);
        let fork = HandoffTarget::Fork(vec!["a".into(), "b".into()]);
        assert_eq!(fork.agent_names(), vec!["a", "b"]);
    }

    #[test]
    fn test_toml_fork_target_deserialization() {
        let toml = r#"
condition = { field = "confidence", op = "<", value = "0.5" }
target = "fork:security,performance"
"#;
        let rule: HandoffRule = toml::from_str(toml).unwrap();
        assert_eq!(
            rule.target,
            Some(HandoffTarget::Fork(vec![
                "security".into(),
                "performance".into()
            ]))
        );
    }

    #[test]
    fn test_toml_fork_target_serialization() {
        let target = HandoffTarget::Fork(vec!["a".into(), "b".into()]);
        let serialized = serde_json::to_string(&target).unwrap();
        assert_eq!(serialized, "\"fork:a,b\"");
    }

    #[test]
    fn test_toml_single_target_serialization() {
        let target = HandoffTarget::Single("refactor".into());
        let serialized = serde_json::to_string(&target).unwrap();
        assert_eq!(serialized, "\"refactor\"");
    }

    #[test]
    fn test_eval_rules_fork_target() {
        let output = json_output();
        let list: RuleList = toml::from_str(
            r#"
[[rules]]
condition = { field = "confidence", op = ">", value = "0.5" }
target = "fork:security,performance"
"#,
        )
        .unwrap();
        assert_eq!(
            eval_rules(&list.rules, &output),
            Some(HandoffTarget::Fork(vec![
                "security".into(),
                "performance".into()
            ]))
        );
    }
}
