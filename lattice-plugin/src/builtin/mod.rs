use crate::bundle::{BehaviorMode, PluginBundle, PluginMeta};
use crate::erased::ErasedPlugin;
use crate::registry::{PluginRegistry, RegistryError};

pub mod code_review;
pub mod deep_research;
pub mod doc_gen;
pub mod image_gen;
pub mod knowledge_base;
pub mod parse_utils;
pub mod plan_gen;
pub mod pptx_gen;
pub mod refactor;
pub mod security_audit;
pub mod superpowers;
pub mod test_gen;
pub mod verification;

fn bundle(name: &str, description: &str, plugin: Box<dyn ErasedPlugin>) -> PluginBundle {
    PluginBundle {
        meta: PluginMeta {
            name: name.to_string(),
            version: "0.1".into(),
            description: description.to_string(),
            author: "lattice".into(),
        },
        plugin,
        default_behavior: BehaviorMode::Yolo,
        default_tools: vec![],
    }
}

fn superpowers_bundle(skill: &'static superpowers::SuperpowerSkill) -> PluginBundle {
    bundle(
        &format!("superpowers:{}", skill.key),
        skill.description,
        Box::new(superpowers::SuperpowerSkillPlugin::new(skill)),
    )
}

pub fn builtin_plugin_bundles() -> Vec<PluginBundle> {
    let mut bundles = vec![
        bundle(
            "CodeReview",
            "Code review plugin",
            Box::new(code_review::CodeReviewPlugin::new()),
        ),
        bundle(
            "Refactor",
            "Code refactoring plugin",
            Box::new(refactor::RefactorPlugin::new()),
        ),
        bundle(
            "TestGen",
            "Test generation plugin",
            Box::new(test_gen::TestGenPlugin::new()),
        ),
        bundle(
            "SecurityAudit",
            "Security audit plugin",
            Box::new(security_audit::SecurityAuditPlugin::new()),
        ),
        bundle(
            "DocGen",
            "Documentation generation plugin",
            Box::new(doc_gen::DocGenPlugin::new()),
        ),
        bundle(
            "PptxGen",
            "PPTX generation plugin",
            Box::new(pptx_gen::PptxGenPlugin::new()),
        ),
        bundle(
            "DeepResearch",
            "Deep research plugin",
            Box::new(deep_research::DeepResearchPlugin::new()),
        ),
        bundle(
            "ImageGen",
            "Image generation plugin",
            Box::new(image_gen::ImageGenPlugin::new()),
        ),
        bundle(
            "KnowledgeBase",
            "Knowledge base plugin",
            Box::new(knowledge_base::KnowledgeBasePlugin::new()),
        ),
        bundle(
            "PlanGen",
            "Planning plugin",
            Box::new(plan_gen::PlanGenPlugin::new()),
        ),
        bundle(
            "Verification",
            "Verification plugin",
            Box::new(verification::VerificationPlugin::new()),
        ),
    ];

    bundles.extend(
        superpowers::SUPERPOWER_SKILLS
            .iter()
            .map(superpowers_bundle),
    );
    bundles.push(bundle(
        "superpowers:workflow",
        "Superpowers workflow router",
        Box::new(superpowers::SuperpowersWorkflowPlugin::new()),
    ));

    bundles
}

pub fn register_builtin_plugins(registry: &PluginRegistry) -> Result<(), RegistryError> {
    for bundle in builtin_plugin_bundles() {
        registry.register(bundle)?;
    }
    Ok(())
}
