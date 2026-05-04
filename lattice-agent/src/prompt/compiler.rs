use crate::prompt::types::*;
use lattice_core::ResolvedModel;

/// Safety margin subtracted from context_length before budget allocation.
const SAFETY_MARGIN: u32 = 4096;

/// Compile collected sections + user input into a rendered prompt.
///
/// Five phases:
/// 1. Collect: add user_input as Input-layer section
/// 2. Sort: by layer → priority → stable
/// 3. Budget: allocate per Fixed→Ratio→Dynamic, mark over-budget sections
/// 4. Trim: drop sections that exceed their allocated budget
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
        budget: TokenBudget::Fixed(input_tokens),
    };

    let mut all: Vec<PromptSection> = sections.to_vec();
    all.push(input_section);

    // Phase 2: Sort — layer ascending, then priority ascending
    all.sort_by(|a, b| {
        a.layer
            .cmp(&b.layer)
            .then_with(|| a.priority.cmp(&b.priority))
    });

    // Phase 3: Budget — Fixed→Ratio→Dynamic allocation
    let effective_budget = model.context_length.saturating_sub(SAFETY_MARGIN);
    let allocated = allocate_budgets(&all, effective_budget);

    // Phase 4: Trim — drop sections that exceed their allocated budget.
    // Spec: "no truncation, no rewriting" — sections that cannot fit
    // within their allocation are dropped entirely.
    let trimmed: Vec<PromptSection> = all
        .into_iter()
        .enumerate()
        .filter(|(i, s)| {
            // Input layer always survives
            if s.layer == Layer::Input {
                return true;
            }
            // Section survives only if fully funded
            allocated[*i] >= s.tokens
        })
        .map(|(_, s)| s)
        .collect();

    // Phase 5: Render
    let total_tokens: u32 = trimmed.iter().map(|s| s.tokens).sum();
    let text = render(&trimmed, user_input);

    RenderedPrompt {
        text,
        sections: trimmed,
        total_tokens,
    }
}

/// Allocate token budgets to each section using Fixed→Ratio→Dynamic ordering.
///
/// Returns a Vec of allocated token counts parallel to the input sections.
/// Sections with 0 allocation should be trimmed.
fn allocate_budgets(sections: &[PromptSection], effective_budget: u32) -> Vec<u32> {
    let mut allocated = vec![0u32; sections.len()];
    let mut remaining = effective_budget;

    // Pass 1: Fixed — deduct claimed tokens from budget
    for (i, section) in sections.iter().enumerate() {
        if let TokenBudget::Fixed(claim) = section.budget {
            let grant = claim.min(remaining);
            allocated[i] = grant;
            remaining = remaining.saturating_sub(grant);
        }
    }

    // Pass 2: Ratio — each claims proportion of remaining budget.
    // ratio is clamped to [0.0, 1.0]; NaN is treated as 0.0.
    for (i, section) in sections.iter().enumerate() {
        if let TokenBudget::Ratio(ratio) = section.budget {
            let ratio = ratio.clamp(0.0, 1.0);
            let claim = (remaining as f64 * ratio).floor() as u32;
            let grant = claim.min(remaining);
            allocated[i] = grant;
            remaining = remaining.saturating_sub(grant);
        }
    }

    // Pass 3: Dynamic — served in sort order (already sorted by priority),
    // each consumes whatever remains
    for (i, section) in sections.iter().enumerate() {
        if matches!(section.budget, TokenBudget::Dynamic) {
            let grant = section.tokens.min(remaining);
            allocated[i] = grant;
            remaining = remaining.saturating_sub(grant);
        }
    }

    allocated
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
            budget: TokenBudget::Fixed(5),
        }];
        let result = compile(&sections, "hello", &make_resolved(8192));
        assert!(result.text.contains("=== System ==="));
        assert!(result.text.contains("=== Input ==="));
        assert!(result.text.contains("You are a bot."));
        assert!(result.text.contains("hello"));
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
                budget: TokenBudget::Fixed(20),
            },
            PromptSection {
                content: "Lots of events data...".into(),
                layer: Layer::Events,
                priority: 5,
                tokens: 8000,
                budget: TokenBudget::Dynamic,
            },
        ];
        // effective_budget = 8192 - 4096 = 4096
        // Fixed: System claims 20, remaining = 4076
        // Dynamic: Events needs 8000 > 4076 → grant = 4076 < tokens → dropped
        let result = compile(&sections, "hi", &make_resolved(8192));
        assert!(result.text.contains("=== System ==="));
        assert!(result.text.contains("=== Input ==="));
        assert!(!result.text.contains("=== Events ==="));
    }

    #[test]
    fn dynamic_sections_dropped_when_no_budget_remains() {
        let sections = vec![
            PromptSection {
                content: "S".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 5,
                budget: TokenBudget::Fixed(4000),
            },
            PromptSection {
                content: "Rules".into(),
                layer: Layer::Rules,
                priority: 0,
                tokens: 50,
                budget: TokenBudget::Fixed(50),
            },
            PromptSection {
                content: "Events data".into(),
                layer: Layer::Events,
                priority: 5,
                tokens: 100,
                budget: TokenBudget::Dynamic,
            },
        ];
        // effective_budget = 4096 - 4096 = 0
        // Fixed: System claims min(4000, 0) = 0, Rules claims min(50, 0) = 0
        // Dynamic: Events gets 0 → dropped
        // But Input always survives
        let result = compile(&sections, "hi", &make_resolved(4096));
        assert_eq!(result.text, "hi");
    }

    #[test]
    fn fixed_budget_reserved_first() {
        let sections = vec![
            PromptSection {
                content: "System".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 100,
                budget: TokenBudget::Fixed(200),
            },
            PromptSection {
                content: "Tools".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 500,
                budget: TokenBudget::Dynamic,
            },
        ];
        // effective_budget = 8192 - 4096 = 4096
        // Fixed: System claims 200, remaining = 3896
        // Dynamic: Tools gets min(500, 3896) = 500
        let result = compile(&sections, "hi", &make_resolved(8192));
        assert!(result.text.contains("=== System ==="));
        assert!(result.text.contains("=== Tools ==="));
        assert!(result.text.contains("=== Input ==="));
    }

    #[test]
    fn ratio_budget_proportional_allocation() {
        let sections = vec![
            PromptSection {
                content: "System".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 10,
                budget: TokenBudget::Fixed(10),
            },
            PromptSection {
                content: "Memory recall".into(),
                layer: Layer::Memory,
                priority: 5,
                tokens: 2000,
                budget: TokenBudget::Ratio(0.5),
            },
            PromptSection {
                content: "Tools".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 2000,
                budget: TokenBudget::Dynamic,
            },
        ];
        // effective_budget = 8192 - 4096 = 4096
        // Fixed: System claims 10, remaining = 4086
        // Ratio: Memory claims floor(4086 * 0.5) = 2043, remaining = 2043
        // Dynamic: Tools gets min(2000, 2043) = 2000
        let result = compile(&sections, "hi", &make_resolved(8192));
        assert!(result.text.contains("=== System ==="));
        assert!(result.text.contains("=== Memory ==="));
        assert!(result.text.contains("=== Tools ==="));
    }

    #[test]
    fn ratio_overclaimed_drops_underfunded_sections() {
        let sections = vec![
            PromptSection {
                content: "System".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 10,
                budget: TokenBudget::Fixed(10),
            },
            PromptSection {
                content: "Memory".into(),
                layer: Layer::Memory,
                priority: 5,
                tokens: 5000,
                budget: TokenBudget::Ratio(0.9),
            },
            PromptSection {
                content: "Events".into(),
                layer: Layer::Events,
                priority: 5,
                tokens: 500,
                budget: TokenBudget::Dynamic,
            },
        ];
        // effective_budget = 4096
        // Fixed: System=10, remaining=4086
        // Ratio: Memory=floor(4086*0.9)=3677, but Memory tokens=5000 → grant<tokens → dropped
        // Dynamic: Events remaining=409, grant=409 < 500 → dropped
        // Only System + Input survive
        let result = compile(&sections, "hi", &make_resolved(8192));
        assert!(result.text.contains("=== System ==="));
        assert!(result.text.contains("=== Input ==="));
        assert!(!result.text.contains("=== Memory ==="));
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
                budget: TokenBudget::Dynamic,
            },
            PromptSection {
                content: "low".into(),
                layer: Layer::Tools,
                priority: 10,
                tokens: 5,
                budget: TokenBudget::Dynamic,
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
            budget: TokenBudget::Fixed(5),
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
                budget: TokenBudget::Fixed(10),
            },
            PromptSection {
                content: "T".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 20,
                budget: TokenBudget::Dynamic,
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
                budget: TokenBudget::Fixed(8),
            },
            PromptSection {
                content: "curl file:///".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 4,
                budget: TokenBudget::Dynamic,
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
                budget: TokenBudget::Fixed(5),
            },
            PromptSection {
                content: "T".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 1000,
                budget: TokenBudget::Dynamic,
            },
        ];
        // SAFETY_MARGIN = 4096. context_length 5000 - 4096 = 904 effective
        // Fixed: System claims min(5, 904) = 5, remaining = 899
        // Dynamic: Tools needs 1000 > 899 → grant = 899 < tokens → dropped
        let result = compile(&sections, "I", &make_resolved(5000));
        assert!(result.text.contains("=== System ==="));
        assert!(!result.text.contains("=== Tools ==="));
    }

    #[test]
    fn zero_effective_budget_drops_all_non_input() {
        // context_length < SAFETY_MARGIN → effective_budget = 0
        // All non-Input sections get 0 allocation → dropped
        // Input always survives
        let sections = vec![PromptSection {
            content: "S".into(),
            layer: Layer::System,
            priority: 0,
            tokens: 5,
            budget: TokenBudget::Fixed(5),
        }];
        let result = compile(&sections, "hello", &make_resolved(100));
        assert_eq!(result.text, "hello");
    }

    #[test]
    fn allocate_budgets_fixed_deducted_first() {
        let sections = vec![
            PromptSection {
                content: "S".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 100,
                budget: TokenBudget::Fixed(100),
            },
            PromptSection {
                content: "T".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 200,
                budget: TokenBudget::Dynamic,
            },
        ];
        // effective_budget = 1000
        // Fixed: System claims 100, remaining = 900
        // Dynamic: Tools gets min(200, 900) = 200
        let alloc = allocate_budgets(&sections, 1000);
        assert_eq!(alloc[0], 100);
        assert_eq!(alloc[1], 200);
    }

    #[test]
    fn allocate_budgets_fixed_exhausts_budget() {
        let sections = vec![
            PromptSection {
                content: "S".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 600,
                budget: TokenBudget::Fixed(600),
            },
            PromptSection {
                content: "R".into(),
                layer: Layer::Rules,
                priority: 0,
                tokens: 500,
                budget: TokenBudget::Fixed(500),
            },
        ];
        // effective_budget = 1000
        // Fixed: System claims 600, remaining = 400
        // Fixed: Rules claims min(500, 400) = 400
        let alloc = allocate_budgets(&sections, 1000);
        assert_eq!(alloc[0], 600);
        assert_eq!(alloc[1], 400);
    }

    #[test]
    fn allocate_budgets_ratio_after_fixed() {
        let sections = vec![
            PromptSection {
                content: "S".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 10,
                budget: TokenBudget::Fixed(10),
            },
            PromptSection {
                content: "M".into(),
                layer: Layer::Memory,
                priority: 5,
                tokens: 500,
                budget: TokenBudget::Ratio(0.3),
            },
            PromptSection {
                content: "T".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 300,
                budget: TokenBudget::Dynamic,
            },
        ];
        // effective_budget = 1000
        // Fixed: System=10, remaining=990
        // Ratio: Memory=floor(990*0.3)=297, remaining=693
        // Dynamic: Tools=min(300, 693)=300
        let alloc = allocate_budgets(&sections, 1000);
        assert_eq!(alloc[0], 10);
        assert_eq!(alloc[1], 297);
        assert_eq!(alloc[2], 300);
    }

    #[test]
    fn allocate_budgets_dynamic_priority_order() {
        let sections = vec![
            PromptSection {
                content: "T1".into(),
                layer: Layer::Tools,
                priority: 1,
                tokens: 800,
                budget: TokenBudget::Dynamic,
            },
            PromptSection {
                content: "T2".into(),
                layer: Layer::Tools,
                priority: 10,
                tokens: 500,
                budget: TokenBudget::Dynamic,
            },
        ];
        // effective_budget = 1000
        // Dynamic (sorted by priority): T1(1) gets min(800, 1000) = 800, remaining=200
        // Dynamic: T2(10) gets min(500, 200) = 200
        let alloc = allocate_budgets(&sections, 1000);
        assert_eq!(alloc[0], 800);
        assert_eq!(alloc[1], 200);
    }

    #[test]
    fn allocate_budgets_zero_budget() {
        let sections = vec![PromptSection {
            content: "S".into(),
            layer: Layer::System,
            priority: 0,
            tokens: 100,
            budget: TokenBudget::Fixed(100),
        }];
        let alloc = allocate_budgets(&sections, 0);
        assert_eq!(alloc[0], 0);
    }
}
