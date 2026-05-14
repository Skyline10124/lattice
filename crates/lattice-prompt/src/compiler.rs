use crate::types::*;
use lattice_core::types::{Message, Role};
use lattice_core::ResolvedModel;

/// Safety margin subtracted from context_length before budget allocation.
/// Accounts for system overhead, response tokens, and token estimation inaccuracy.
const SAFETY_MARGIN: u32 = 4096;

/// Compile collected contributions + user input into a rendered prompt.
pub fn compile_contributions(
    contributions: &[PromptContribution],
    user_input: &str,
    model: &ResolvedModel,
) -> Result<RenderedPrompt, PromptCompileError> {
    // Phase 1: Collect — add user input as Input layer with Fixed budget
    let input_tokens = lattice_core::tokens::TokenEstimator::estimate_text(user_input);
    let input = PromptContribution::new(
        "input.user",
        user_input,
        Layer::Input,
        255,
        input_tokens,
        TokenBudget::Fixed(input_tokens),
        "user-input",
    );

    let mut paired = contributions.to_vec();
    paired.push(input);

    // Phase 2: Sort — layer ascending, then priority ascending
    paired.sort_by(|a, b| {
        a.layer
            .cmp(&b.layer)
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.id.cmp(&b.id))
    });

    // Phase 3: Budget — Fixed→Ratio→Dynamic allocation
    let effective_budget = model.context_length.saturating_sub(SAFETY_MARGIN);
    let allocated = allocate_contribution_budgets(&paired, effective_budget);

    // Phase 4: Trim — drop sections that exceed their allocated budget.
    // Spec: "no truncation, no rewriting" — sections that cannot fit
    // within their allocation are dropped entirely.
    let mut manifest = Vec::new();
    let trimmed: Vec<PromptContribution> = paired
        .into_iter()
        .enumerate()
        .filter(|(i, contribution)| {
            // Input layer always survives
            if contribution.layer == Layer::Input {
                return true;
            }
            // Contribution survives only if fully funded
            let keep = allocated[*i] >= contribution.tokens;
            if !keep && contribution.layer.is_manifest_disclosable() {
                manifest.push(contribution.to_fragment().manifest_entry());
            }
            keep
        })
        .map(|(_, contribution)| contribution)
        .collect();

    // Phase 5: Render — produce Vec<Message> with proper role assignments
    let total_tokens: u32 = trimmed.iter().map(|s| s.tokens).sum();
    let fragments: Vec<PromptFragment> = trimmed
        .iter()
        .map(PromptContribution::to_fragment)
        .collect();
    let sections: Vec<PromptSection> = trimmed.iter().map(PromptContribution::section).collect();
    let messages = render_messages(&fragments, &manifest, user_input);

    Ok(RenderedPrompt {
        messages,
        sections,
        contributions: trimmed,
        fragments,
        context_manifest: manifest,
        total_tokens,
    })
}

fn allocate_contribution_budgets(
    contributions: &[PromptContribution],
    effective_budget: u32,
) -> Vec<u32> {
    let mut allocated = vec![0u32; contributions.len()];
    let mut remaining = effective_budget;

    for (i, contribution) in contributions.iter().enumerate() {
        if let TokenBudget::Fixed(claim) = contribution.budget {
            let grant = claim.min(remaining);
            allocated[i] = grant;
            remaining = remaining.saturating_sub(grant);
        }
    }

    for (i, contribution) in contributions.iter().enumerate() {
        if let TokenBudget::Ratio(ratio) = contribution.budget {
            let ratio = ratio.clamp(0.0, 1.0);
            let claim = (remaining as f64 * ratio).floor() as u32;
            let grant = claim.min(remaining);
            allocated[i] = grant;
            remaining = remaining.saturating_sub(grant);
        }
    }

    for (i, contribution) in contributions.iter().enumerate() {
        if matches!(contribution.budget, TokenBudget::Dynamic) {
            let grant = contribution.tokens.min(remaining);
            allocated[i] = grant;
            remaining = remaining.saturating_sub(grant);
        }
    }

    allocated
}

/// Render trimmed fragments into Vec<Message> with proper role assignments.
///
/// System-layer content becomes one Role::System message. All other layers
/// become one Role::User message with explicit lattice-context markers. Dropped
/// fragments are disclosed through a manifest so the model knows context exists
/// without receiving the full body.
fn render_messages(
    fragments: &[PromptFragment],
    manifest: &[ContextManifestEntry],
    raw_input: &str,
) -> Vec<Message> {
    let system_fragments: Vec<&PromptFragment> = fragments
        .iter()
        .filter(|s| s.layer == Layer::System)
        .collect();
    let other_fragments: Vec<&PromptFragment> = fragments
        .iter()
        .filter(|s| s.layer != Layer::System)
        .collect();

    let mut messages = Vec::new();

    // System message: plain concatenated content (role conveys system authority)
    if !system_fragments.is_empty() {
        let system_content = system_fragments
            .iter()
            .filter(|s| !s.content.is_empty())
            .map(|s| s.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !system_content.is_empty() {
            messages.push(Message::new(Role::System, system_content, None, None, None));
        }
    }

    // User message: non-System sections with markers, or raw input if only Input
    if !other_fragments.is_empty() || !manifest.is_empty() {
        let user_content = if other_fragments.len() == 1
            && other_fragments[0].layer == Layer::Input
            && manifest.is_empty()
        {
            raw_input.to_string()
        } else {
            render_with_markers(&other_fragments, manifest)
        };
        if !user_content.is_empty() {
            messages.push(Message::new(Role::User, user_content, None, None, None));
        }
    }

    messages
}

/// Render non-System fragments with explicit lattice-context markers.
fn render_with_markers(sections: &[&PromptFragment], manifest: &[ContextManifestEntry]) -> String {
    let mut output = String::new();
    let mut first = true;
    for fragment in sections {
        if fragment.content.is_empty() {
            continue;
        }
        if !first {
            output.push('\n');
        }
        first = false;
        output.push_str(&fragment.render_marked());
    }

    if !manifest.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("<lattice-context-manifest>\n");
        for entry in manifest {
            output.push_str(&entry.render_line());
            output.push('\n');
        }
        output.push_str("</lattice-context-manifest>");
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

    /// Helper: find the first message with a given role in a RenderedPrompt.
    fn find_msg(result: &RenderedPrompt, role: Role) -> Option<&Message> {
        result.messages.iter().find(|m| m.role == role)
    }

    #[test]
    fn input_only_renders_as_raw_text() {
        let contributions = vec![];
        let result =
            compile_contributions(&contributions, "hello world", &make_resolved(8192)).unwrap();
        assert_eq!(result.messages.len(), 1);
        let msg = &result.messages[0];
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "hello world");
        assert_eq!(result.total_tokens, result.sections[0].tokens);
    }

    #[test]
    fn system_and_input_produces_two_messages() {
        let contributions = vec![PromptContribution::from_section(
            PromptSection {
                content: "You are a bot.".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 5,
            },
            TokenBudget::Fixed(5),
            "test",
        )];
        let result = compile_contributions(&contributions, "hello", &make_resolved(8192)).unwrap();
        assert_eq!(result.messages.len(), 2);
        // System message — plain content, no markers
        let sys = find_msg(&result, Role::System).unwrap();
        assert_eq!(sys.content, "You are a bot.");
        // User message — raw input (only Input section)
        let user = find_msg(&result, Role::User).unwrap();
        assert_eq!(user.content, "hello");
    }

    #[test]
    fn budget_overflow_drops_lowest_priority_layer() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "System prompt".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 20,
                },
                TokenBudget::Fixed(20),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "Lots of events data...".into(),
                    layer: Layer::Events,
                    priority: 5,
                    tokens: 8000,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        // effective_budget = 8192 - 4096 = 4096
        // Fixed: System claims 20, remaining = 4076
        // Dynamic: Events needs 8000 > 4076 → grant = 4076 < tokens → dropped
        let result = compile_contributions(&contributions, "hi", &make_resolved(8192)).unwrap();
        let sys = find_msg(&result, Role::System).unwrap();
        assert_eq!(sys.content, "System prompt");
        let user = find_msg(&result, Role::User).unwrap();
        assert!(user.content.contains("<lattice-context-manifest>"));
        assert!(user.content.contains("layer=Events"));
    }

    #[test]
    fn dynamic_sections_dropped_when_no_budget_remains() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "S".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 5,
                },
                TokenBudget::Fixed(4000),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "Rules".into(),
                    layer: Layer::Rules,
                    priority: 0,
                    tokens: 50,
                },
                TokenBudget::Fixed(50),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "Events data".into(),
                    layer: Layer::Events,
                    priority: 5,
                    tokens: 100,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        // effective_budget = 4096 - 4096 = 0
        // All grants = 0. System/Rules are dropped without disclosure; Events
        // is disclosed as manifest-only context.
        let result = compile_contributions(&contributions, "hi", &make_resolved(4096)).unwrap();
        assert_eq!(result.messages.len(), 1);
        let user = find_msg(&result, Role::User).unwrap();
        assert!(user.content.contains("<lattice-context-manifest>"));
        assert!(user.content.contains("layer=Events"));
        assert!(user.content.contains("Events data"));
    }

    #[test]
    fn fixed_budget_reserved_first() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "System".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 100,
                },
                TokenBudget::Fixed(200),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "Tools".into(),
                    layer: Layer::Tools,
                    priority: 5,
                    tokens: 500,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        // effective_budget = 4096
        // Fixed: System claims 200, remaining = 3896
        // Dynamic: Tools gets min(500, 3896) = 500
        let result = compile_contributions(&contributions, "hi", &make_resolved(8192)).unwrap();
        let sys = find_msg(&result, Role::System).unwrap();
        assert_eq!(sys.content, "System");
        let user = find_msg(&result, Role::User).unwrap();
        assert!(user.content.contains("<lattice-context id="));
        assert!(user.content.contains("<lattice-context id="));
    }

    #[test]
    fn ratio_budget_proportional_allocation() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "System".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 10,
                },
                TokenBudget::Fixed(10),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "Memory recall".into(),
                    layer: Layer::Memory,
                    priority: 5,
                    tokens: 2000,
                },
                TokenBudget::Ratio(0.5),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "Tools".into(),
                    layer: Layer::Tools,
                    priority: 5,
                    tokens: 2000,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        // Fixed: System=10, remaining=4086
        // Ratio: Memory=floor(4086*0.5)=2043, remaining=2043
        // Dynamic: Tools=min(2000, 2043)=2000
        let result = compile_contributions(&contributions, "hi", &make_resolved(8192)).unwrap();
        let sys = find_msg(&result, Role::System).unwrap();
        assert_eq!(sys.content, "System");
        let user = find_msg(&result, Role::User).unwrap();
        assert!(user.content.contains("<lattice-context id="));
        assert!(user.content.contains("<lattice-context id="));
    }

    #[test]
    fn ratio_overclaimed_drops_underfunded_sections() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "System".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 10,
                },
                TokenBudget::Fixed(10),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "Memory".into(),
                    layer: Layer::Memory,
                    priority: 5,
                    tokens: 5000,
                },
                TokenBudget::Ratio(0.9),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "Events".into(),
                    layer: Layer::Events,
                    priority: 5,
                    tokens: 500,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        // Fixed: System=10, remaining=4086
        // Ratio: Memory=floor(4086*0.9)=3677 < 5000 → dropped
        // Dynamic: Events remaining=409, grant=409 < 500 → dropped
        let result = compile_contributions(&contributions, "hi", &make_resolved(8192)).unwrap();
        assert_eq!(result.messages.len(), 2);
        let sys = find_msg(&result, Role::System).unwrap();
        assert_eq!(sys.content, "System");
        let user = find_msg(&result, Role::User).unwrap();
        assert!(user.content.contains("<lattice-context-manifest>"));
        assert!(user.content.contains("layer=Memory"));
        assert!(user.content.contains("layer=Events"));
        assert!(user.content.contains("summary=\"Memory\""));
    }

    #[test]
    fn same_layer_respects_priority() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "high".into(),
                    layer: Layer::Tools,
                    priority: 1,
                    tokens: 5,
                },
                TokenBudget::Dynamic,
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "low".into(),
                    layer: Layer::Tools,
                    priority: 10,
                    tokens: 5,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        let result = compile_contributions(&contributions, "input", &make_resolved(8192)).unwrap();
        let user = find_msg(&result, Role::User).unwrap();
        let high_pos = user.content.find("high").unwrap();
        let low_pos = user.content.find("low").unwrap();
        assert!(
            high_pos < low_pos,
            "higher priority content should appear first"
        );
    }

    #[test]
    fn empty_input_still_produces_input_section() {
        let contributions = vec![PromptContribution::from_section(
            PromptSection {
                content: "System".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 5,
            },
            TokenBudget::Fixed(5),
            "test",
        )];
        let result = compile_contributions(&contributions, "", &make_resolved(8192)).unwrap();
        assert!(result.sections.iter().any(|s| s.layer == Layer::Input));
    }

    #[test]
    fn total_tokens_matches_sections_sum() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "S".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 10,
                },
                TokenBudget::Fixed(10),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "T".into(),
                    layer: Layer::Tools,
                    priority: 5,
                    tokens: 20,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        let result = compile_contributions(&contributions, "I", &make_resolved(8192)).unwrap();
        // System(10) + Tools(20) + Input(1) = 31
        assert_eq!(result.total_tokens, 31);
    }

    #[test]
    fn compile_with_system_and_tools_and_input() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "You are code review AI.".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 8,
                },
                TokenBudget::Fixed(8),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "curl file:///".into(),
                    layer: Layer::Tools,
                    priority: 5,
                    tokens: 4,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        let result =
            compile_contributions(&contributions, "review this file", &make_resolved(8192))
                .unwrap();
        // System message: plain content
        let sys = find_msg(&result, Role::System).unwrap();
        assert!(sys.content.contains("You are code review AI."));
        // User message: Tools + Input with markers
        let user = find_msg(&result, Role::User).unwrap();
        assert!(user.content.contains("layer=\"tools\""));
        assert!(user.content.contains("layer=\"input\""));
        assert!(!user.content.contains("layer=\"memory\""));
        assert!(!user.content.contains("layer=\"events\""));
        assert!(!user.content.contains("layer=\"rules\""));
        // No System marker in user message (role conveys that authority)
        assert!(!user.content.contains("layer=\"system\""));
    }

    #[test]
    fn tiny_budget_only_system_and_input_survive() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "S".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 5,
                },
                TokenBudget::Fixed(5),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "T".into(),
                    layer: Layer::Tools,
                    priority: 5,
                    tokens: 1000,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        // SAFETY_MARGIN = 4096. context_length 5000 - 4096 = 904 effective
        // Fixed: System claims min(5, 904) = 5, remaining = 899
        // Dynamic: Tools needs 1000 > 899 → grant = 899 < tokens → dropped
        let result = compile_contributions(&contributions, "I", &make_resolved(5000)).unwrap();
        assert_eq!(result.messages.len(), 2);
        let sys = find_msg(&result, Role::System).unwrap();
        assert_eq!(sys.content, "S");
        let user = find_msg(&result, Role::User).unwrap();
        assert_eq!(user.content, "I"); // Only Input
    }

    #[test]
    fn zero_effective_budget_drops_all_non_input() {
        let contributions = vec![PromptContribution::from_section(
            PromptSection {
                content: "S".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 5,
            },
            TokenBudget::Fixed(5),
            "test",
        )];
        // context_length < SAFETY_MARGIN → effective_budget = 0 → all non-Input dropped
        let result = compile_contributions(&contributions, "hello", &make_resolved(100)).unwrap();
        assert_eq!(result.messages.len(), 1);
        let user = find_msg(&result, Role::User).unwrap();
        assert_eq!(user.content, "hello");
    }

    #[test]
    fn allocate_budgets_fixed_deducted_first() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "S".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 100,
                },
                TokenBudget::Fixed(100),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "T".into(),
                    layer: Layer::Tools,
                    priority: 5,
                    tokens: 200,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        let alloc = allocate_contribution_budgets(&contributions, 1000);
        assert_eq!(alloc[0], 100);
        assert_eq!(alloc[1], 200);
    }

    #[test]
    fn allocate_budgets_fixed_exhausts_budget() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "S".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 600,
                },
                TokenBudget::Fixed(600),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "R".into(),
                    layer: Layer::Rules,
                    priority: 0,
                    tokens: 500,
                },
                TokenBudget::Fixed(500),
                "test",
            ),
        ];
        let alloc = allocate_contribution_budgets(&contributions, 1000);
        assert_eq!(alloc[0], 600);
        assert_eq!(alloc[1], 400);
    }

    #[test]
    fn allocate_budgets_ratio_after_fixed() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "S".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 10,
                },
                TokenBudget::Fixed(10),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "M".into(),
                    layer: Layer::Memory,
                    priority: 5,
                    tokens: 500,
                },
                TokenBudget::Ratio(0.3),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "T".into(),
                    layer: Layer::Tools,
                    priority: 5,
                    tokens: 300,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        let alloc = allocate_contribution_budgets(&contributions, 1000);
        assert_eq!(alloc[0], 10);
        assert_eq!(alloc[1], 297);
        assert_eq!(alloc[2], 300);
    }

    #[test]
    fn allocate_budgets_dynamic_priority_order() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "T1".into(),
                    layer: Layer::Tools,
                    priority: 1,
                    tokens: 800,
                },
                TokenBudget::Dynamic,
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "T2".into(),
                    layer: Layer::Tools,
                    priority: 10,
                    tokens: 500,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        let alloc = allocate_contribution_budgets(&contributions, 1000);
        assert_eq!(alloc[0], 800);
        assert_eq!(alloc[1], 200);
    }

    #[test]
    fn allocate_budgets_zero_budget() {
        let contributions = vec![PromptContribution::from_section(
            PromptSection {
                content: "S".into(),
                layer: Layer::System,
                priority: 0,
                tokens: 100,
            },
            TokenBudget::Fixed(100),
            "test",
        )];
        let alloc = allocate_contribution_budgets(&contributions, 0);
        assert_eq!(alloc[0], 0);
    }

    #[test]
    fn ratio_nan_clamped_to_zero() {
        let contributions = vec![PromptContribution::from_section(
            PromptSection {
                content: "M".into(),
                layer: Layer::Memory,
                priority: 5,
                tokens: 500,
            },
            TokenBudget::Ratio(f64::NAN),
            "test",
        )];
        let alloc = allocate_contribution_budgets(&contributions, 1000);
        assert_eq!(alloc[0], 0, "NaN ratio should produce 0 allocation");
    }

    #[test]
    fn ratio_negative_clamped_to_zero() {
        let contributions = vec![PromptContribution::from_section(
            PromptSection {
                content: "M".into(),
                layer: Layer::Memory,
                priority: 5,
                tokens: 500,
            },
            TokenBudget::Ratio(-0.5),
            "test",
        )];
        let alloc = allocate_contribution_budgets(&contributions, 1000);
        assert_eq!(alloc[0], 0, "negative ratio should produce 0 allocation");
    }

    #[test]
    fn ratio_above_one_clamped() {
        let contributions = vec![PromptContribution::from_section(
            PromptSection {
                content: "M".into(),
                layer: Layer::Memory,
                priority: 5,
                tokens: 500,
            },
            TokenBudget::Ratio(1.5),
            "test",
        )];
        let alloc = allocate_contribution_budgets(&contributions, 1000);
        assert_eq!(
            alloc[0], 1000,
            "ratio >1 clamped to 1.0, claims full remaining"
        );
    }

    #[test]
    fn multiple_ratio_providers_sequential() {
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "M1".into(),
                    layer: Layer::Memory,
                    priority: 1,
                    tokens: 500,
                },
                TokenBudget::Ratio(0.5),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "M2".into(),
                    layer: Layer::Memory,
                    priority: 2,
                    tokens: 500,
                },
                TokenBudget::Ratio(0.5),
                "test",
            ),
        ];
        // remaining = 1000
        // M1: floor(1000*0.5) = 500, remaining = 500
        // M2: floor(500*0.5) = 250, remaining = 250
        let alloc = allocate_contribution_budgets(&contributions, 1000);
        assert_eq!(alloc[0], 500);
        assert_eq!(alloc[1], 250);
    }

    #[test]
    fn system_prompt_and_system_provider_coexist() {
        // set_system_prompt creates System(priority=0) with Fixed budget
        // A registered provider at System(priority=5) with Dynamic budget
        // Both appear; content merged into one System message
        let contributions = vec![
            PromptContribution::from_section(
                PromptSection {
                    content: "You are an assistant.".into(),
                    layer: Layer::System,
                    priority: 0,
                    tokens: 10,
                },
                TokenBudget::Fixed(10),
                "test",
            ),
            PromptContribution::from_section(
                PromptSection {
                    content: "Additional system rules.".into(),
                    layer: Layer::System,
                    priority: 5,
                    tokens: 20,
                },
                TokenBudget::Dynamic,
                "test",
            ),
        ];
        let result = compile_contributions(&contributions, "hello", &make_resolved(8192)).unwrap();
        // Both System sections merged into one System message
        let sys = find_msg(&result, Role::System).unwrap();
        let assistant_pos = sys.content.find("You are an assistant.").unwrap();
        let rules_pos = sys.content.find("Additional system rules.").unwrap();
        assert!(
            assistant_pos < rules_pos,
            "priority 0 content before priority 5"
        );
    }
}
