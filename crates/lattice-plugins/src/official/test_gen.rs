use serde::{Deserialize, Serialize};

use lattice_plugin::Plugin;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestGenInput {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub focus_areas: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestGenOutput {
    #[serde(default)]
    pub tests: String,
    #[serde(default)]
    pub coverage_estimate: f64,
}

pub struct TestGenPlugin;

impl TestGenPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TestGenPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for TestGenPlugin {
    type Input = TestGenInput;
    type Output = TestGenOutput;

    fn name(&self) -> &str {
        "test-gen"
    }

    fn system_prompt(&self) -> &str {
        "You are an expert test engineer. Generate comprehensive, idiomatic tests for the \
         provided code.\n\n\
         ## Output Schema\n\
         Return a JSON object with exactly two fields:\n\
         - \"tests\": a string containing the complete test code (properly escaped for JSON)\n\
         - \"coverage_estimate\": a float from 0.0 to 1.0 estimating branch coverage\n\n\
         ## Test Quality Requirements\n\
         - Cover happy path, edge cases, and error conditions\n\
         - Use the language's standard test framework (e.g., #[test] for Rust, pytest for Python)\n\
         - Name tests descriptively: test_<function>_<scenario>_<expected>\n\
         - Include setup/teardown when needed\n\
         - Test boundary values (empty, zero, max, None/null)\n\
         - For async code, test cancellation and timeout paths\n\n\
         ## Rules\n\
         - Match the language and style of the input code\n\
         - Do not test private implementation details — test through public API\n\
         - If focus_areas are specified, prioritize those paths\n\
         - Output ONLY the JSON object, no markdown fences, no commentary"
    }

    fn to_prompt(&self, input: &Self::Input) -> String {
        let lang = if input.language.is_empty() {
            "the detected language"
        } else {
            &input.language
        };
        let mut prompt = format!("Generate tests for the following {} code.\n\n", lang);
        if !input.focus_areas.is_empty() {
            prompt.push_str("Focus areas:\n");
            for area in &input.focus_areas {
                prompt.push_str(&format!("- {}\n", area));
            }
            prompt.push('\n');
        }
        prompt.push_str("```\n");
        prompt.push_str(&input.code);
        prompt.push_str("\n```");
        prompt
    }
}
