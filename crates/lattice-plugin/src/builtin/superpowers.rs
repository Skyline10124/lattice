//! Superpowers methodology plugins.
//!
//! The embedded skill text is imported from `obra/superpowers` (MIT licensed)
//! and exposed as native LATTICE plugins so it can participate in PluginRegistry
//! and PluginDagRunner workflows without a runtime network dependency.

use serde::{Deserialize, Serialize};

use crate::Plugin;

const SUPERPOWERS_VERSION: &str = "5.1.0";

#[derive(Debug, Clone, Copy)]
pub struct SuperpowerSkill {
    pub key: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub skill: &'static str,
    pub references: &'static [(&'static str, &'static str)],
}

const EMPTY_REFS: &[(&str, &str)] = &[];

const BRAINSTORMING_REFS: &[(&str, &str)] = &[
    (
        "spec-document-reviewer-prompt.md",
        include_str!("superpowers/skills/brainstorming/spec-document-reviewer-prompt.md"),
    ),
    (
        "visual-companion.md",
        include_str!("superpowers/skills/brainstorming/visual-companion.md"),
    ),
];

const REQUESTING_CODE_REVIEW_REFS: &[(&str, &str)] = &[(
    "code-reviewer.md",
    include_str!("superpowers/skills/requesting-code-review/code-reviewer.md"),
)];

const SUBAGENT_DRIVEN_DEVELOPMENT_REFS: &[(&str, &str)] = &[
    (
        "implementer-prompt.md",
        include_str!("superpowers/skills/subagent-driven-development/implementer-prompt.md"),
    ),
    (
        "spec-reviewer-prompt.md",
        include_str!("superpowers/skills/subagent-driven-development/spec-reviewer-prompt.md"),
    ),
    (
        "code-quality-reviewer-prompt.md",
        include_str!(
            "superpowers/skills/subagent-driven-development/code-quality-reviewer-prompt.md"
        ),
    ),
];

const SYSTEMATIC_DEBUGGING_REFS: &[(&str, &str)] = &[
    (
        "root-cause-tracing.md",
        include_str!("superpowers/skills/systematic-debugging/root-cause-tracing.md"),
    ),
    (
        "defense-in-depth.md",
        include_str!("superpowers/skills/systematic-debugging/defense-in-depth.md"),
    ),
    (
        "condition-based-waiting.md",
        include_str!("superpowers/skills/systematic-debugging/condition-based-waiting.md"),
    ),
    (
        "test-academic.md",
        include_str!("superpowers/skills/systematic-debugging/test-academic.md"),
    ),
    (
        "test-pressure-1.md",
        include_str!("superpowers/skills/systematic-debugging/test-pressure-1.md"),
    ),
    (
        "test-pressure-2.md",
        include_str!("superpowers/skills/systematic-debugging/test-pressure-2.md"),
    ),
    (
        "test-pressure-3.md",
        include_str!("superpowers/skills/systematic-debugging/test-pressure-3.md"),
    ),
];

const TEST_DRIVEN_DEVELOPMENT_REFS: &[(&str, &str)] = &[(
    "testing-anti-patterns.md",
    include_str!("superpowers/skills/test-driven-development/testing-anti-patterns.md"),
)];

const WRITING_PLANS_REFS: &[(&str, &str)] = &[(
    "plan-document-reviewer-prompt.md",
    include_str!("superpowers/skills/writing-plans/plan-document-reviewer-prompt.md"),
)];

const WRITING_SKILLS_REFS: &[(&str, &str)] = &[
    (
        "anthropic-best-practices.md",
        include_str!("superpowers/skills/writing-skills/anthropic-best-practices.md"),
    ),
    (
        "persuasion-principles.md",
        include_str!("superpowers/skills/writing-skills/persuasion-principles.md"),
    ),
    (
        "testing-skills-with-subagents.md",
        include_str!("superpowers/skills/writing-skills/testing-skills-with-subagents.md"),
    ),
    (
        "graphviz-conventions.dot",
        include_str!("superpowers/skills/writing-skills/graphviz-conventions.dot"),
    ),
];

pub const SUPERPOWER_SKILLS: &[SuperpowerSkill] = &[
    SuperpowerSkill {
        key: "using-superpowers",
        title: "Using Superpowers",
        description: "Skill selection and mandatory workflow entry rules",
        skill: include_str!("superpowers/skills/using-superpowers/SKILL.md"),
        references: EMPTY_REFS,
    },
    SuperpowerSkill {
        key: "brainstorming",
        title: "Brainstorming",
        description: "Collaborative design refinement before implementation",
        skill: include_str!("superpowers/skills/brainstorming/SKILL.md"),
        references: BRAINSTORMING_REFS,
    },
    SuperpowerSkill {
        key: "using-git-worktrees",
        title: "Using Git Worktrees",
        description: "Isolated workspace setup and baseline verification",
        skill: include_str!("superpowers/skills/using-git-worktrees/SKILL.md"),
        references: EMPTY_REFS,
    },
    SuperpowerSkill {
        key: "writing-plans",
        title: "Writing Plans",
        description: "Detailed TDD implementation plan generation",
        skill: include_str!("superpowers/skills/writing-plans/SKILL.md"),
        references: WRITING_PLANS_REFS,
    },
    SuperpowerSkill {
        key: "subagent-driven-development",
        title: "Subagent-Driven Development",
        description: "Fresh subagent per task with two-stage review",
        skill: include_str!("superpowers/skills/subagent-driven-development/SKILL.md"),
        references: SUBAGENT_DRIVEN_DEVELOPMENT_REFS,
    },
    SuperpowerSkill {
        key: "executing-plans",
        title: "Executing Plans",
        description: "Inline plan execution with checkpoints",
        skill: include_str!("superpowers/skills/executing-plans/SKILL.md"),
        references: EMPTY_REFS,
    },
    SuperpowerSkill {
        key: "dispatching-parallel-agents",
        title: "Dispatching Parallel Agents",
        description: "Concurrent agent dispatch for independent work",
        skill: include_str!("superpowers/skills/dispatching-parallel-agents/SKILL.md"),
        references: EMPTY_REFS,
    },
    SuperpowerSkill {
        key: "test-driven-development",
        title: "Test-Driven Development",
        description: "Red-green-refactor implementation discipline",
        skill: include_str!("superpowers/skills/test-driven-development/SKILL.md"),
        references: TEST_DRIVEN_DEVELOPMENT_REFS,
    },
    SuperpowerSkill {
        key: "requesting-code-review",
        title: "Requesting Code Review",
        description: "Pre-merge and per-task review workflow",
        skill: include_str!("superpowers/skills/requesting-code-review/SKILL.md"),
        references: REQUESTING_CODE_REVIEW_REFS,
    },
    SuperpowerSkill {
        key: "receiving-code-review",
        title: "Receiving Code Review",
        description: "Handling and triaging review feedback",
        skill: include_str!("superpowers/skills/receiving-code-review/SKILL.md"),
        references: EMPTY_REFS,
    },
    SuperpowerSkill {
        key: "systematic-debugging",
        title: "Systematic Debugging",
        description: "Disciplined root-cause debugging loop",
        skill: include_str!("superpowers/skills/systematic-debugging/SKILL.md"),
        references: SYSTEMATIC_DEBUGGING_REFS,
    },
    SuperpowerSkill {
        key: "verification-before-completion",
        title: "Verification Before Completion",
        description: "Evidence gate before completion claims",
        skill: include_str!("superpowers/skills/verification-before-completion/SKILL.md"),
        references: EMPTY_REFS,
    },
    SuperpowerSkill {
        key: "finishing-a-development-branch",
        title: "Finishing a Development Branch",
        description: "Merge, PR, keep, or discard workflow",
        skill: include_str!("superpowers/skills/finishing-a-development-branch/SKILL.md"),
        references: EMPTY_REFS,
    },
    SuperpowerSkill {
        key: "writing-skills",
        title: "Writing Skills",
        description: "Methodology for authoring new agent skills",
        skill: include_str!("superpowers/skills/writing-skills/SKILL.md"),
        references: WRITING_SKILLS_REFS,
    },
];

pub fn find_skill(key: &str) -> Option<&'static SuperpowerSkill> {
    SUPERPOWER_SKILLS
        .iter()
        .find(|skill| skill.key.eq_ignore_ascii_case(key))
}

pub fn all_skill_keys() -> Vec<&'static str> {
    SUPERPOWER_SKILLS.iter().map(|skill| skill.key).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperpowerSkillInput {
    #[serde(default)]
    pub request: String,
    #[serde(default)]
    pub project_context: String,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperpowerSkillOutput {
    pub skill: String,
    #[serde(default)]
    pub next_action: String,
    #[serde(default)]
    pub checklist: Vec<String>,
    #[serde(default)]
    pub questions: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub confidence: f64,
}

#[derive(Debug, Clone)]
pub struct SuperpowerSkillPlugin {
    skill: &'static SuperpowerSkill,
}

impl SuperpowerSkillPlugin {
    pub fn new(skill: &'static SuperpowerSkill) -> Self {
        Self { skill }
    }

    pub fn skill(&self) -> &'static SuperpowerSkill {
        self.skill
    }
}

impl Plugin for SuperpowerSkillPlugin {
    type Input = SuperpowerSkillInput;
    type Output = SuperpowerSkillOutput;

    fn name(&self) -> &str {
        self.skill.key
    }

    fn system_prompt(&self) -> &str {
        self.skill.skill
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        let mut prompt = String::new();
        prompt.push_str("Execute this Superpowers skill as a LATTICE plugin.\n");
        prompt.push_str("Return ONLY valid JSON with fields: ");
        prompt.push_str("skill, next_action, checklist, questions, artifacts, confidence.\n");
        prompt.push_str("Do not claim completion without evidence required by the skill.\n\n");
        prompt.push_str(&format!("Skill: {}\n", self.skill.key));
        prompt.push_str(&format!("Upstream version: {SUPERPOWERS_VERSION}\n"));
        if !input.project_context.is_empty() {
            prompt.push_str("\nPROJECT CONTEXT:\n");
            prompt.push_str(&input.project_context);
            prompt.push('\n');
        }
        if !input.constraints.is_empty() {
            prompt.push_str("\nCONSTRAINTS:\n");
            for constraint in &input.constraints {
                prompt.push_str("- ");
                prompt.push_str(constraint);
                prompt.push('\n');
            }
        }
        if !input.artifacts.is_empty() {
            prompt.push_str("\nARTIFACTS:\n");
            for artifact in &input.artifacts {
                prompt.push_str("---\n");
                prompt.push_str(artifact);
                prompt.push('\n');
            }
        }
        prompt.push_str("\nUSER REQUEST:\n");
        prompt.push_str(&input.request);
        if !self.skill.references.is_empty() {
            prompt.push_str("\n\nREFERENCE MATERIAL AVAILABLE TO THIS SKILL:\n");
            for (name, content) in self.skill.references {
                prompt.push_str("\n--- ");
                prompt.push_str(name);
                prompt.push_str(" ---\n");
                prompt.push_str(content);
                prompt.push('\n');
            }
        }
        prompt
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "skill": { "type": "string" },
                "next_action": { "type": "string" },
                "checklist": { "type": "array", "items": { "type": "string" } },
                "questions": { "type": "array", "items": { "type": "string" } },
                "artifacts": { "type": "array", "items": { "type": "string" } },
                "confidence": { "type": "number" }
            },
            "required": ["skill", "next_action", "checklist", "questions", "artifacts", "confidence"]
        }))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperpowersWorkflowInput {
    #[serde(default)]
    pub request: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub available_artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperpowersWorkflowOutput {
    pub selected_skill: String,
    pub reason: String,
    #[serde(default)]
    pub next_plugins: Vec<String>,
    #[serde(default)]
    pub requires_user_approval: bool,
    #[serde(default)]
    pub confidence: f64,
}

pub struct SuperpowersWorkflowPlugin;

impl SuperpowersWorkflowPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SuperpowersWorkflowPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for SuperpowersWorkflowPlugin {
    type Input = SuperpowersWorkflowInput;
    type Output = SuperpowersWorkflowOutput;

    fn name(&self) -> &str {
        "superpowers-workflow"
    }

    fn system_prompt(&self) -> &str {
        "You route coding-agent work through the Superpowers methodology. \
         Pick the next Superpowers skill/plugin for the request. Respect user \
         instructions above methodology rules. Return ONLY valid JSON."
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        let skills = SUPERPOWER_SKILLS
            .iter()
            .map(|skill| format!("- superpowers:{} — {}", skill.key, skill.description))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Select the next Superpowers plugin.\n\n\
             WORKFLOW ORDER:\n\
             brainstorming -> using-git-worktrees -> writing-plans -> \
             subagent-driven-development or executing-plans -> \
             test-driven-development -> requesting-code-review -> \
             verification-before-completion -> finishing-a-development-branch\n\n\
             AVAILABLE SKILLS:\n{skills}\n\n\
             CURRENT STAGE: {}\n\
             ARTIFACTS: {}\n\n\
             REQUEST:\n{}\n\n\
             Return JSON with selected_skill, reason, next_plugins, \
             requires_user_approval, confidence.",
            input.stage,
            input.available_artifacts.join(", "),
            input.request
        )
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "selected_skill": { "type": "string" },
                "reason": { "type": "string" },
                "next_plugins": { "type": "array", "items": { "type": "string" } },
                "requires_user_approval": { "type": "boolean" },
                "confidence": { "type": "number" }
            },
            "required": ["selected_skill", "reason", "next_plugins", "requires_user_approval", "confidence"]
        }))
    }
}
