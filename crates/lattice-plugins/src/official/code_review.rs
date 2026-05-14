use serde::{Deserialize, Serialize};

use lattice_plugin::Plugin;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeReviewInput {
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub context_rules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub severity: String,
    pub file: String,
    pub line: u32,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeReviewOutput {
    #[serde(default)]
    pub issues: Vec<Issue>,
    #[serde(default)]
    pub confidence: f64,
}

pub struct CodeReviewPlugin;

impl CodeReviewPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CodeReviewPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for CodeReviewPlugin {
    type Input = CodeReviewInput;
    type Output = CodeReviewOutput;

    fn name(&self) -> &str {
        "code-review"
    }

    fn system_prompt(&self) -> &str {
        "You are a senior code reviewer specializing in bugs, security vulnerabilities, \
         and design issues. You analyze code with precision and report findings in structured JSON.\n\n\
         ## Output Schema\n\
         Return a JSON object with exactly two fields:\n\
         - \"issues\": array of objects, each with:\n\
           - \"severity\": one of \"critical\", \"high\", \"medium\", \"low\"\n\
           - \"file\": the file path being reviewed\n\
           - \"line\": the 1-based line number where the issue occurs\n\
           - \"description\": a concise explanation of the problem and how to fix it\n\
         - \"confidence\": a float from 0.0 to 1.0 indicating overall review confidence\n\n\
         ## Severity Guidelines\n\
         - critical: data loss, RCE, auth bypass, crash in production path\n\
         - high: security issue, logic bug affecting correctness, resource leak\n\
         - medium: code smell, missing error handling, performance issue\n\
         - low: style, naming, minor readability concern\n\n\
         ## Rules\n\
         - Only report real issues you are confident about\n\
         - If the code is clean, return {\"issues\": [], \"confidence\": 0.95}\n\
         - Never invent line numbers — use 0 if uncertain\n\
         - Output ONLY the JSON object, no markdown fences, no commentary"
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        let mut prompt = String::from("Review the following code.\n\n");
        if !input.file_path.is_empty() {
            prompt.push_str(&format!("File: {}\n\n", input.file_path));
        }
        if !input.context_rules.is_empty() {
            prompt.push_str("Additional review rules:\n");
            for rule in &input.context_rules {
                prompt.push_str(&format!("- {}\n", rule));
            }
            prompt.push('\n');
        }
        prompt.push_str("```\n");
        prompt.push_str(&input.input);
        prompt.push_str("\n```");
        prompt
    }
}
