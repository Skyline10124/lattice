//! Shared orchestration types used by both lattice-plugin (dag_runner) and
//! lattice-bus (pipeline, profile, handoff_rule).
//!
//! These live in lattice-plugin so lattice-bus can depend on them without
//! creating a circular dependency (bus → plugin → bus).

use serde::{Deserialize, Serialize};
use tracing::warn;

// ---------------------------------------------------------------------------
// HandoffTarget — routing destination for plugin DAG and pipeline
// ---------------------------------------------------------------------------

/// Routing target for handoff — single agent or fork to multiple agents.
#[derive(Debug, Clone, PartialEq)]
pub enum HandoffTarget {
    /// Route to a single agent.
    Single(String),
    /// Fork to multiple agents in parallel.
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

/// Maximum number of allowed fork targets to prevent resource exhaustion.
const MAX_FORK_TARGETS: usize = 10;

impl HandoffTarget {
    /// Parse a string into a HandoffTarget.
    /// Supports `"agent-name"` for Single and `"fork:agent1,agent2"` for Fork.
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
                    "Fork target count {} exceeds max {}, truncating to {}",
                    targets.len(),
                    MAX_FORK_TARGETS,
                    MAX_FORK_TARGETS
                );
                targets.truncate(MAX_FORK_TARGETS);
            }
            HandoffTarget::Fork(targets)
        } else {
            HandoffTarget::Single(trimmed.to_string())
        }
    }

    /// Return all agent names this target routes to.
    pub fn agent_names(&self) -> Vec<&str> {
        match self {
            HandoffTarget::Single(name) => vec![name],
            HandoffTarget::Fork(names) => names.iter().map(|s| s.as_str()).collect(),
        }
    }
}

impl Serialize for HandoffTarget {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            HandoffTarget::Single(name) => serializer.serialize_str(name),
            HandoffTarget::Fork(names) => {
                let joined = format!("fork:{}", names.join(","));
                serializer.serialize_str(&joined)
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

// ---------------------------------------------------------------------------
// PluginsConfig — plugin slot configuration for agent profiles
// ---------------------------------------------------------------------------

/// Plugins configuration for an agent profile — defines which plugins to
/// run, in what order, with optional DAG edges between them.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginsConfig {
    pub entry: String,
    #[serde(default)]
    pub slots: Vec<PluginSlotConfig>,
    #[serde(default)]
    pub edges: Vec<AgentEdgeConfig>,
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
    pub behavior: Option<crate::bundle::BehaviorMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEdgeConfig {
    pub from: String,
    pub rule: HandoffRule,
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

/// Maximum recursion depth for nested JSON traversal in condition evaluation.
const MAX_JSON_DEPTH: usize = 50;

/// Resolve a dotted field path like `"confidence"` or `"issues[0].severity"`
/// against a `serde_json::Value`.
fn resolve_field<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let segments = parse_path(path);
    let mut current = root;
    let mut depth = 0;
    for seg in &segments {
        depth += 1;
        if depth > MAX_JSON_DEPTH {
            warn!(
                "JSON path traversal depth {} exceeds limit {}, aborting",
                depth, MAX_JSON_DEPTH
            );
            return None;
        }
        current = match seg {
            PathSegment::Key(k) => current.get(k)?,
            PathSegment::Index(i) => current.get(*i)?,
            PathSegment::Any => {
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
                    warn!("Malformed path segment [{current}] in handoff condition - skipping");
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

    let field_val = match resolve_field(output, &cond.field) {
        Some(v) => v,
        None => return false,
    };
    eval_operator(field_val, cond)
}

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
