use crate::prompt::types::*;
use lattice_core::ResolvedModel;

/// Safety margin subtracted from context_length before budget allocation.
/// Accounts for system overhead, response tokens, and token estimation inaccuracy.
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
    budgets: &[TokenBudget],
    user_input: &str,
    model: &ResolvedModel,
) -> RenderedPrompt {
    assert_eq!(
        sections.len(),
        budgets.len(),
        "sections and budgets must be parallel"
    );

    // Phase 1: Collect — add user input as Input layer with Fixed budget
    let input_tokens = lattice_core::tokens::TokenEstimator::estimate_text(user_input);
    let input_section = PromptSection {
        content: user_input.to_string(),
        layer: Layer::Input,
        priority: 255,
        tokens: input_tokens,
    };
    let input_budget = TokenBudget::Fixed(input_tokens);

    let mut paired: Vec<(PromptSection, TokenBudget)> = sections
        .iter()
        .zip(budgets.iter())
        .map(|(s, b)| (s.clone(), *b))
        .collect();
    paired.push((input_section, input_budget));

    // Phase 2: Sort — layer ascending, then priority ascending
    paired.sort_by(|a, b| {
        a.0.layer
            .cmp(&b.0.layer)
            .then_with(|| a.0.priority.cmp(&b.0.priority))
    });

    // Phase 3: Budget — Fixed→Ratio→Dynamic allocation
    let effective_budget = model.context_length.saturating_sub(SAFETY_MARGIN);
    let allocated = allocate_budgets(&paired, effective_budget);

    // Phase 4: Trim — drop sections that exceed their allocated budget.
    // Spec: "no truncation, no rewriting" — sections that cannot fit
    // within their allocation are dropped entirely.
    let trimmed: Vec<PromptSection> = paired
        .into_iter()
        .enumerate()
        .filter(|(i, (s, _))| {
            // Input layer always survives
            if s.layer == Layer::Input {
                return true;
            }
            // Section survives only if fully funded
            allocated[*i] >= s.tokens
        })
        .map(|(_, (s, _))| s)
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

/// Allocate token budgets using Fixed→Ratio→Dynamic ordering.
///
/// Returns a Vec of allocated token counts parallel to the input pairs.
/// Sections with allocation < tokens should be trimmed (not partially rendered).
fn allocate_budgets(pairs: &[(PromptSection, TokenBudget)], effective_budget: u32) -> Vec<u32> {
    let mut allocated = vec![0u32; pairs.len()];
    let mut remaining = effective_budget;

    // Pass 1: Fixed — deduct claimed tokens from budget
    for (i, (_, budget)) in pairs.iter().enumerate() {
        if let TokenBudget::Fixed(claim) = *budget {
            let grant = claim.min(remaining);
            allocated[i] = grant;
            remaining = remaining.saturating_sub(grant);
        }
    }

    // Pass 2: Ratio — each claims proportion of remaining budget.
    // ratio is clamped to [0.0, 1.0]; NaN is treated as 0.0.
    for (i, (_, budget)) in pairs.iter().enumerate() {
        if let TokenBudget::Ratio(ratio) = *budget {
            let ratio = ratio.clamp(0.0, 1.0);
            let claim = (remaining as f64 * ratio).floor() as u32;
            let grant = claim.min(remaining);
            allocated[i] = grant;
            remaining = remaining.saturating_sub(grant);
        }
    }

    // Pass 3: Dynamic — served in sort order (already sorted by priority),
    // each consumes whatever remains
    for (i, (section, budget)) in pairs.iter().enumerate() {
        if matches!(budget, TokenBudget::Dynamic) {
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
        let budgets = vec![];
        let result = compile(&sections, &budgets, "hello world", &make_resolved(8192));
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
        let budgets = vec![TokenBudget::Fixed(5)];
        let result = compile(&sections, &budgets, "hello", &make_resolved(8192));
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
            },
            PromptSection {
                content: "Lots of events data...".into(),
                layer: Layer::Events,
                priority: 5,
                tokens: 8000,
            },
        ];
        let budgets = vec![TokenBudget::Fixed(20), TokenBudget::Dynamic];
        // effective_budget = 8192 - 4096 = 4096
        // Fixed: System claims 20, remaining = 4076
        // Dynamic: Events needs 8000 > 4076 → grant = 4076 < tokens → dropped
        let result = compile(&sections, &budgets, "hi", &make_resolved(8192));
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
            },
            PromptSection {
                content: "Rules".into(),
                layer: Layer::Rules,
                priority: 0,
                tokens: 50,
            },
            PromptSection {
                content: "Events data".into(),
                layer: Layer::Events,
                priority: 5,
                tokens: 100,
            },
        ];
        let budgets = vec![
            TokenBudget::Fixed(4000),
            TokenBudget::Fixed(50),
            TokenBudget::Dynamic,
        ];
        // effective_budget = 4096 - 4096 = 0
        // All grants = 0 → all dropped; Input always survives
        let result = compile(&sections, &budgets, "hi", &make_resolved(4096));
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
            },
            PromptSection {
                content: "Tools".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 500,
            },
        ];
        let budgets = vec![TokenBudget::Fixed(200), TokenBudget::Dynamic];
        // effective_budget = 4096
        // Fixed: System claims 200, remaining = 3896
        // Dynamic: Tools gets min(500, 3896) = 500
        let result = compile(&sections, &budgets, "hi", &make_resolved(8192));
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
            },
            PromptSection {
                content: "Memory recall".into(),
                layer: Layer::Memory,
                priority: 5,
                tokens: 2000,
            },
            PromptSection {
                content: "Tools".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 2000,
            },
        ];
        let budgets = vec![
            TokenBudget::Fixed(10),
            TokenBudget::Ratio(0.5),
            TokenBudget::Dynamic,
        ];
        // Fixed: System=10, remaining=4086
        // Ratio: Memory=floor(4086*0.5)=2043, remaining=2043
        // Dynamic: Tools=min(2000, 2043)=2000
        let result = compile(&sections, &budgets, "hi", &make_resolved(8192));
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
            },
            PromptSection {
                content: "Memory".into(),
                layer: Layer::Memory,
                priority: 5,
                tokens: 5000,
            },
            PromptSection {
                content: "Events".into(),
                layer: Layer::Events,
                priority: 5,
                tokens: 500,
            },
        ];
        let budgets = vec![
            TokenBudget::Fixed(10),
            TokenBudget::Ratio(0.9),
            TokenBudget::Dynamic,
        ];
        // Fixed: System=10, remaining=4086
        // Ratio: Memory=floor(4086*0.9)=3677 < 5000 → dropped
        // Dynamic: Events remaining=409, grant=409 < 500 → dropped
        let result = compile(&sections, &budgets, "hi", &make_resolved(8192));
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
            },
            PromptSection {
                content: "low".into(),
                layer: Layer::Tools,
                priority: 10,
                tokens: 5,
            },
        ];
        let budgets = vec![TokenBudget::Dynamic, TokenBudget::Dynamic];
        let result = compile(&sections, &budgets, "input", &make_resolved(8192));
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
        let budgets = vec![TokenBudget::Fixed(5)];
        let result = compile(&sections, &budgets, "", &make_resolved(8192));
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
        let budgets = vec![TokenBudget::Fixed(10), TokenBudget::Dynamic];
        let result = compile(&sections, &budgets, "I", &make_resolved(8192));
        // System(10) + Tools(20) + Input(1) = 31
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
        let budgets = vec![TokenBudget::Fixed(8), TokenBudget::Dynamic];
        let result = compile(
            &sections,
            &budgets,
            "review this file",
            &make_resolved(8192),
        );
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
        let budgets = vec![TokenBudget::Fixed(5), TokenBudget::Dynamic];
        // SAFETY_MARGIN = 4096. context_length 5000 - 4096 = 904 effective
        // Fixed: System claims min(5, 904) = 5, remaining = 899
        // Dynamic: Tools needs 1000 > 899 → grant = 899 < tokens → dropped
        let result = compile(&sections, &budgets, "I", &make_resolved(5000));
        assert!(result.text.contains("=== System ==="));
        assert!(!result.text.contains("=== Tools ==="));
    }

    #[test]
    fn zero_effective_budget_drops_all_non_input() {
        let sections = vec![PromptSection {
            content: "S".into(),
            layer: Layer::System,
            priority: 0,
            tokens: 5,
        }];
        let budgets = vec![TokenBudget::Fixed(5)];
        // context_length < SAFETY_MARGIN → effective_budget = 0 → all non-Input dropped
        let result = compile(&sections, &budgets, "hello", &make_resolved(100));
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
            },
            PromptSection {
                content: "T".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 200,
            },
        ];
        let budgets = vec![TokenBudget::Fixed(100), TokenBudget::Dynamic];
        let alloc = allocate_budgets(
            &sections
                .iter()
                .zip(budgets.iter())
                .map(|(s, b)| (s.clone(), *b))
                .collect::<Vec<_>>(),
            1000,
        );
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
            },
            PromptSection {
                content: "R".into(),
                layer: Layer::Rules,
                priority: 0,
                tokens: 500,
            },
        ];
        let budgets = vec![TokenBudget::Fixed(600), TokenBudget::Fixed(500)];
        let alloc = allocate_budgets(
            &sections
                .iter()
                .zip(budgets.iter())
                .map(|(s, b)| (s.clone(), *b))
                .collect::<Vec<_>>(),
            1000,
        );
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
            },
            PromptSection {
                content: "M".into(),
                layer: Layer::Memory,
                priority: 5,
                tokens: 500,
            },
            PromptSection {
                content: "T".into(),
                layer: Layer::Tools,
                priority: 5,
                tokens: 300,
            },
        ];
        let budgets = vec![
            TokenBudget::Fixed(10),
            TokenBudget::Ratio(0.3),
            TokenBudget::Dynamic,
        ];
        let alloc = allocate_budgets(
            &sections
                .iter()
                .zip(budgets.iter())
                .map(|(s, b)| (s.clone(), *b))
                .collect::<Vec<_>>(),
            1000,
        );
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
            },
            PromptSection {
                content: "T2".into(),
                layer: Layer::Tools,
                priority: 10,
                tokens: 500,
            },
        ];
        let budgets = vec![TokenBudget::Dynamic, TokenBudget::Dynamic];
        let alloc = allocate_budgets(
            &sections
                .iter()
                .zip(budgets.iter())
                .map(|(s, b)| (s.clone(), *b))
                .collect::<Vec<_>>(),
            1000,
        );
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
        }];
        let budgets = vec![TokenBudget::Fixed(100)];
        let alloc = allocate_budgets(
            &sections
                .iter()
                .zip(budgets.iter())
                .map(|(s, b)| (s.clone(), *b))
                .collect::<Vec<_>>(),
            0,
        );
        assert_eq!(alloc[0], 0);
    }

    #[test]
    fn ratio_nan_clamped_to_zero() {
        let sections = vec![PromptSection {
            content: "M".into(),
            layer: Layer::Memory,
            priority: 5,
            tokens: 500,
        }];
        let budgets = vec![TokenBudget::Ratio(f64::NAN)];
        let alloc = allocate_budgets(
            &sections
                .iter()
                .zip(budgets.iter())
                .map(|(s, b)| (s.clone(), *b))
                .collect::<Vec<_>>(),
            1000,
        );
        assert_eq!(alloc[0], 0, "NaN ratio should produce 0 allocation");
    }

    #[test]
    fn ratio_negative_clamped_to_zero() {
        let sections = vec![PromptSection {
            content: "M".into(),
            layer: Layer::Memory,
            priority: 5,
            tokens: 500,
        }];
        let budgets = vec![TokenBudget::Ratio(-0.5)];
        let alloc = allocate_budgets(
            &sections
                .iter()
                .zip(budgets.iter())
                .map(|(s, b)| (s.clone(), *b))
                .collect::<Vec<_>>(),
            1000,
        );
        assert_eq!(alloc[0], 0, "negative ratio should produce 0 allocation");
    }

    #[test]
    fn ratio_above_one_clamped() {
        let sections = vec![PromptSection {
            content: "M".into(),
            layer: Layer::Memory,
            priority: 5,
            tokens: 500,
        }];
        let budgets = vec![TokenBudget::Ratio(1.5)];
        let alloc = allocate_budgets(
            &sections
                .iter()
                .zip(budgets.iter())
                .map(|(s, b)| (s.clone(), *b))
                .collect::<Vec<_>>(),
            1000,
        );
        assert_eq!(
            alloc[0], 1000,
            "ratio >1 clamped to 1.0, claims full remaining"
        );
    }

    #[test]
    fn multiple_ratio_providers_sequential() {
        let sections = vec![
            PromptSection {
                content: "M1".into(),
                layer: Layer::Memory,
                priority: 1,
                tokens: 500,
            },
            PromptSection {
                content: "M2".into(),
                layer: Layer::Memory,
                priority: 2,
                tokens: 500,
            },
        ];
        let budgets = vec![TokenBudget::Ratio(0.5), TokenBudget::Ratio(0.5)];
        // remaining = 1000
        // M1: floor(1000*0.5) = 500, remaining = 500
        // M2: floor(500*0.5) = 250, remaining = 250
        let alloc = allocate_budgets(
            &sections
                .iter()
                .zip(budgets.iter())
                .map(|(s, b)| (s.clone(), *b))
                .collect::<Vec<_>>(),
            1000,
        );
        assert_eq!(alloc[0], 500);
        assert_eq!(alloc[1], 250);
    }

    #[test]
    fn system_prompt_and_system_provider_coexist() {
        // set_system_prompt creates System(priority=0) with Fixed budget
        // A registered provider at System(priority=5) with Dynamic budget
        // Both appear; set_system_prompt section comes first (lower priority)
        let sections = vec![
            PromptSection {
                content: "You are an assistant.".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 10,
            },
            PromptSection {
                content: "Additional system rules.".into(),
                layer: Layer::System,
                priority: 5,
                tokens: 20,
            },
        ];
        let budgets = vec![TokenBudget::Fixed(10), TokenBudget::Dynamic];
        let result = compile(&sections, &budgets, "hello", &make_resolved(8192));
        assert!(result.text.contains("=== System ==="));
        // Both sections under same layer marker; priority 0 before priority 5
        let assistant_pos = result.text.find("You are an assistant.").unwrap();
        let rules_pos = result.text.find("Additional system rules.").unwrap();
        assert!(assistant_pos < rules_pos);
    }
}
