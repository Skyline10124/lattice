use crate::prompt::types::*;
use lattice_core::ResolvedModel;

/// Safety margin subtracted from context_length before budget allocation.
const SAFETY_MARGIN: u32 = 4096;

/// Compile collected sections + user input into a rendered prompt.
///
/// Five phases:
/// 1. Collect: add user_input as Input-layer section
/// 2. Sort: by layer → priority → stable
/// 3. Budget: compute effective budget, ensure convergence
/// 4. Trim: tail-drop sections that exceed budget
/// 5. Render: plain text with === Layer === markers
pub fn compile(
    sections: &[PromptSection],
    user_input: &str,
    model: &ResolvedModel,
) -> RenderedPrompt {
    // Phase 1: Collect — add user input as Input layer
    let input_tokens = lattice_core::tokens::TokenEstimator::estimate_text(user_input);
    let input_section = PromptSection {
        content: user_input.to_string(),
        layer: Layer::Input,
        priority: 255,
        tokens: input_tokens,
    };

    let mut all: Vec<PromptSection> = sections.to_vec();
    all.push(input_section);

    // Phase 2: Sort — layer ascending, then priority ascending
    all.sort_by(|a, b| {
        a.layer
            .cmp(&b.layer)
            .then_with(|| a.priority.cmp(&b.priority))
    });

    // Phase 3: Budget — compute effective budget
    let effective_budget = model.context_length.saturating_sub(SAFETY_MARGIN);

    // Phase 4: Trim — tail-drop sections exceeding budget
    let mut accumulated: u32 = 0;
    let mut drop_from_layer: Option<Layer> = None;

    for section in all.iter().rev() {
        if accumulated + section.tokens > effective_budget {
            drop_from_layer = Some(section.layer);
            break;
        }
        accumulated += section.tokens;
    }

    let trimmed: Vec<PromptSection> = if let Some(drop_layer) = drop_from_layer {
        all.into_iter()
            .filter(|s| s.layer < drop_layer || s.layer == Layer::Input)
            .collect()
    } else {
        all
    };

    // Phase 5: Render
    let total_tokens: u32 = trimmed.iter().map(|s| s.tokens).sum();
    let text = render(&trimmed, user_input);

    RenderedPrompt {
        text,
        sections: trimmed,
        total_tokens,
    }
}

/// Render sections to plain text with layer markers.
///
/// If only one section (user input), render as raw text without markers.
fn render(sections: &[PromptSection], raw_input: &str) -> String {
    // If the only section is input, return raw text
    if sections.len() == 1 && sections[0].layer == Layer::Input {
        return raw_input.to_string();
    }

    let mut output = String::new();
    for (i, section) in sections.iter().enumerate() {
        if section.content.is_empty() {
            continue;
        }
        if i > 0 {
            output.push('\n');
        }
        output.push_str(&format!("=== {:?} ===\n", section.layer));
        output.push_str(&section.content);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_resolved(cl: u32) -> lattice_core::ResolvedModel {
        lattice_core::ResolvedModel {
            canonical_id: "test".into(),
            provider: "test".into(),
            api_key: None,
            base_url: "".into(),
            api_protocol: lattice_core::catalog::ApiProtocol::OpenAiChat,
            api_model_id: "test".into(),
            context_length: cl,
            provider_specific: HashMap::new(),
            credential_status: lattice_core::CredentialStatus::Missing,
        }
    }

    #[test]
    fn input_only_renders_as_raw_text() {
        let sections = vec![];
        let result = compile(&sections, "hello world", &make_resolved(8192));
        assert_eq!(result.text, "hello world");
        assert_eq!(result.total_tokens, result.sections[0].tokens);
    }

    #[test]
    fn system_and_input_renders_with_markers() {
        let sections = vec![PromptSection {
            content: "You are a bot.".into(),
            layer: Layer::System,
            priority: 0,
            tokens: 5,
        }];
        let result = compile(&sections, "hello", &make_resolved(8192));
        assert!(result.text.contains("=== System ==="));
        assert!(result.text.contains("=== Input ==="));
        assert!(result.text.contains("You are a bot."));
        assert!(result.text.contains("hello"));
        // System section should come before Input
        let sys_pos = result.text.find("System").unwrap();
        let inp_pos = result.text.find("Input").unwrap();
        assert!(sys_pos < inp_pos);
    }

    #[test]
    fn budget_overflow_drops_lowest_priority_layer() {
        let sections = vec![
            PromptSection {
                content: "System prompt".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 20,
            },
            PromptSection {
                content: "Lots of events data...".into(),
                layer: Layer::Events,
                priority: 5,
                tokens: 8000,
            },
        ];
        // context_length 8192, safety margin 4096 → effective_budget = 4096
        // System(20) + Events(8000) + Input(~1) = 8021 > 4096 → Events dropped
        let result = compile(&sections, "hi", &make_resolved(8192));
        assert!(result.text.contains("=== System ==="));
        assert!(result.text.contains("=== Input ==="));
        assert!(!result.text.contains("=== Events ==="));
    }

    #[test]
    fn same_layer_respects_priority() {
        let sections = vec![
            PromptSection {
                content: "high".into(),
                layer: Layer::Tools,
                priority: 1,
                tokens: 5,
            },
            PromptSection {
                content: "low".into(),
                layer: Layer::Tools,
                priority: 10,
                tokens: 5,
            },
        ];
        let result = compile(&sections, "input", &make_resolved(8192));
        let high_pos = result.text.find("high").unwrap();
        let low_pos = result.text.find("low").unwrap();
        assert!(
            high_pos < low_pos,
            "higher priority content should appear first"
        );
    }

    #[test]
    fn empty_input_still_produces_input_section() {
        let sections = vec![PromptSection {
            content: "System".into(),
            layer: Layer::System,
            priority: 0,
            tokens: 5,
        }];
        let result = compile(&sections, "", &make_resolved(8192));
        assert!(result.sections.iter().any(|s| s.layer == Layer::Input));
    }

    #[test]
    fn total_tokens_matches_sections_sum() {
        let sections = vec![
            PromptSection {
                content: "S".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 10,
            },
            PromptSection {
                content: "T".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 20,
            },
        ];
        let result = compile(&sections, "I", &make_resolved(8192));
        // System(10) + Tools(20) + Input(estimate_text("I")=1) = 31
        assert_eq!(result.total_tokens, 31);
    }

    #[test]
    fn compile_with_system_and_tools_and_input() {
        let sections = vec![
            PromptSection {
                content: "You are code review AI.".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 8,
            },
            PromptSection {
                content: "curl file:///".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 4,
            },
        ];
        let result = compile(&sections, "review this file", &make_resolved(8192));
        assert!(result.text.contains("=== System ==="));
        assert!(result.text.contains("=== Tools ==="));
        assert!(result.text.contains("=== Input ==="));
        assert!(!result.text.contains("=== Memory ==="));
        assert!(!result.text.contains("=== Events ==="));
        assert!(!result.text.contains("=== Rules ==="));
    }

    #[test]
    fn tiny_budget_only_system_and_input_survive() {
        let sections = vec![
            PromptSection {
                content: "S".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 5,
            },
            PromptSection {
                content: "T".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 1000,
            },
        ];
        // SAFETY_MARGIN = 4096. context_length 5000 - 4096 = 904 effective
        // Sorted: System(5), Tools(1000), Input(~1)
        // Reverse: Input(1) acc=1; Tools(1000) → 1+1000=1001 > 904 → drop_from_layer=Tools
        // Filter: layer < Tools (System, Rules) or Input → only System + Input survive
        let result = compile(&sections, "I", &make_resolved(5000));
        assert!(result.text.contains("=== System ==="));
        assert!(!result.text.contains("=== Tools ==="));
        assert!(!result.text.contains("=== Memory ==="));
    }

    #[test]
    fn layer_integrity_on_boundary_drop() {
        // If Tools layer is dropped, ALL Tools sections dropped (same layer)
        let sections = vec![
            PromptSection {
                content: "S".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 5,
            },
            PromptSection {
                content: "Tool A".into(),
                layer: Layer::Tools,
                priority: 1,
                tokens: 500,
            },
            PromptSection {
                content: "Tool B".into(),
                layer: Layer::Tools,
                priority: 2,
                tokens: 500,
            },
        ];
        // context_length 5000 - 4096 = 904 effective
        // System(5) + ToolA(500) = 505 < 904, but ToolA(500) + ToolB(500) = 1000 > 904
        // From tail: Input(~1) + ToolB(500) + ToolA(500) = 1001 > 904 → drop_from_layer = Tools
        // All Tools sections dropped, only System + Input survive
        let result = compile(&sections, "I", &make_resolved(5000));
        assert!(result.text.contains("=== System ==="));
        assert!(!result.text.contains("Tool A"));
        assert!(!result.text.contains("Tool B"));
    }

    #[test]
    fn no_safety_margin_when_context_too_small() {
        // context_length < SAFETY_MARGIN → effective_budget = 0
        // All non-Input sections dropped; Input always survives
        let result = compile(&[], "hello", &make_resolved(100));
        assert_eq!(result.text, "hello");
    }
}
