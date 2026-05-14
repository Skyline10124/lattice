use serde::{Deserialize, Serialize};

use lattice_plugin::Plugin;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanGenInput {
    pub spec: String,
    pub project_path: String,
    #[serde(default)]
    pub context_rules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: u32,
    pub title: String,
    #[serde(default)]
    pub files: Vec<String>,
    pub description: String,
    pub verification: String,
    pub estimated_difficulty: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanGenOutput {
    #[serde(default)]
    pub tasks: Vec<Task>,
    pub estimated_total_turns: u32,
    pub summary: String,
}

pub struct PlanGenPlugin;

impl PlanGenPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PlanGenPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for PlanGenPlugin {
    type Input = PlanGenInput;
    type Output = PlanGenOutput;

    fn name(&self) -> &str {
        "plan-gen"
    }

    fn system_prompt(&self) -> &str {
        "You are a planning agent. Given a specification and context rules, \
         break work into small independent tasks (2-5 min each) with exact \
         file paths, clear descriptions, and verification steps. Order tasks \
         by dependency. Return JSON with tasks array and summary."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        let mut s = String::new();
        s.push_str("Create an implementation plan.\n");
        s.push_str(&format!("Project: {}\n", input.project_path));
        s.push_str("\nSPEC:\n");
        s.push_str(&input.spec);
        s.push_str("\n\nRULES:\n");
        if input.context_rules.is_empty() {
            s.push_str("None.");
        } else {
            for (i, r) in input.context_rules.iter().enumerate() {
                s.push_str(&format!("{}. {}\n", i + 1, r));
            }
        }
        s.push_str("\nBreak into small tasks (2-5 min each) with exact file ");
        s.push_str("paths and verification steps. Order by dependency. ");
        s.push_str("Return JSON with tasks, estimated_total_turns, and summary.");
        s
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "integer" },
                            "title": { "type": "string" },
                            "files": { "type": "array", "items": { "type": "string" } },
                            "description": { "type": "string" },
                            "verification": { "type": "string" },
                            "estimated_difficulty": {
                                "type": "string",
                                "enum": ["easy", "medium", "hard"]
                            }
                        },
                        "required": ["id", "title", "description", "verification", "estimated_difficulty"]
                    }
                },
                "estimated_total_turns": { "type": "integer" },
                "summary": { "type": "string" }
            },
            "required": ["tasks", "estimated_total_turns", "summary"]
        }))
    }
}
