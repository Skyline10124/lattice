use serde::{Deserialize, Serialize};
use tracing::warn;

const MAX_FORK_TARGETS: usize = 10;
const MAX_JSON_DEPTH: usize = 50;
const FLOAT_EQ_EPSILON: f64 = 1e-9;

/// Routing target for handoff: a single agent or a parallel fork.
#[derive(Debug, Clone, PartialEq)]
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
    /// Parse `"agent-name"` or `"fork:agent1,agent2"`.
    pub fn parse(s: &str) -> Self {
        let trimmed = s.trim();
        if let Some(fork_part) = trimmed.strip_prefix("fork:") {
            let mut targets: Vec<String> = fork_part
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if targets.len() > MAX_FORK_TARGETS {
                warn!(
                    "Fork target count {} exceeds max {}, truncating",
                    targets.len(),
                    MAX_FORK_TARGETS
                );
                targets.truncate(MAX_FORK_TARGETS);
            }
            HandoffTarget::Fork(targets)
        } else {
            HandoffTarget::Single(trimmed.to_string())
        }
    }

    pub fn agent_names(&self) -> Vec<&str> {
        match self {
            HandoffTarget::Single(name) => vec![name],
            HandoffTarget::Fork(names) => names.iter().map(String::as_str).collect(),
        }
    }
}

impl Serialize for HandoffTarget {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            HandoffTarget::Single(name) => serializer.serialize_str(name),
            HandoffTarget::Fork(names) => {
                serializer.serialize_str(&format!("fork:{}", names.join(",")))
            }
        }
    }
}

impl<'de> Deserialize<'de> for HandoffTarget {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(HandoffTarget::parse(&s))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HandoffCondition {
    pub field: String,
    pub op: String,
    pub value: serde_json::Value,
}

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
    /// None means the pipeline/DAG ends.
    pub target: Option<HandoffTarget>,
}

impl HandoffRule {
    pub fn eval(&self, output: &serde_json::Value) -> bool {
        if self.default {
            return true;
        }
        if let Some(ref c) = self.condition {
            return eval_condition(c, output);
        }
        if let Some(ref all) = self.all {
            return !all.is_empty() && all.iter().all(|c| eval_condition(c, output));
        }
        if let Some(ref any) = self.any {
            return !any.is_empty() && any.iter().any(|c| eval_condition(c, output));
        }
        false
    }
}

pub fn eval_rules(rules: &[HandoffRule], output: &serde_json::Value) -> Option<HandoffTarget> {
    rules
        .iter()
        .find(|rule| rule.eval(output))
        .and_then(|rule| rule.target.clone())
}

fn eval_condition(cond: &HandoffCondition, output: &serde_json::Value) -> bool {
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

    resolve_field(output, &cond.field).is_some_and(|field| eval_operator(field, cond))
}

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
        (serde_json::Value::Number(n), serde_json::Value::String(s))
        | (serde_json::Value::String(s), serde_json::Value::Number(n)) => s
            .parse::<f64>()
            .is_ok_and(|f| n.as_f64().is_some_and(|nf| float_equal(nf, f))),
        (serde_json::Value::Bool(b1), serde_json::Value::Bool(b2)) => b1 == b2,
        (serde_json::Value::Bool(b), serde_json::Value::String(s)) => {
            s.parse::<bool>().is_ok_and(|b2| *b == b2)
        }
        (serde_json::Value::Null, serde_json::Value::Null) => true,
        _ => a == b,
    }
}

fn float_equal(a: f64, b: f64) -> bool {
    (a - b).abs() < FLOAT_EQ_EPSILON
}

fn compare_values(a: &serde_json::Value, b: &serde_json::Value) -> Option<std::cmp::Ordering> {
    to_f64(a)?.partial_cmp(&to_f64(b)?)
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

fn resolve_field<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = root;
    for (depth, seg) in parse_path(path).iter().enumerate() {
        if depth + 1 > MAX_JSON_DEPTH {
            warn!(
                "JSON path traversal depth {} exceeds limit {}, aborting",
                depth + 1,
                MAX_JSON_DEPTH
            );
            return None;
        }
        current = match seg {
            PathSegment::Key(k) => current.get(k)?,
            PathSegment::Index(i) => current.get(*i)?,
            PathSegment::Any => return Some(current),
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
                    warn!("Malformed path segment [{current}] in handoff condition");
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

fn split_at_any(path: &str) -> Option<(String, String)> {
    let segments = parse_path(path);
    let any_pos = segments
        .iter()
        .position(|s| matches!(s, PathSegment::Any))?;
    Some((
        rebuild_path(&segments[..any_pos]),
        rebuild_path(&segments[any_pos + 1..]),
    ))
}

fn rebuild_path(segments: &[PathSegment]) -> String {
    segments.iter().fold(String::new(), |mut path, seg| {
        match seg {
            PathSegment::Key(k) => {
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(k);
            }
            PathSegment::Index(i) => path.push_str(&format!("[{i}]")),
            PathSegment::Any => path.push_str("[any]"),
        }
        path
    })
}
