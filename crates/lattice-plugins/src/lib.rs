pub mod official;

use lattice_plugin::bundle::{BehaviorMode, PluginBundle, PluginMeta, YoloSandboxPolicy};
use lattice_plugin::erased::ErasedPlugin;
use lattice_plugin::registry::{PluginRegistry, RegistryError};

#[cfg(feature = "code-review")]
pub use official::code_review::CodeReviewPlugin;
#[cfg(feature = "deep-research")]
pub use official::deep_research::DeepResearchPlugin;
#[cfg(feature = "doc-gen")]
pub use official::doc_gen::DocGenPlugin;
#[cfg(feature = "image-gen")]
pub use official::image_gen::ImageGenPlugin;
#[cfg(feature = "knowledge-base")]
pub use official::knowledge_base::KnowledgeBasePlugin;
#[cfg(feature = "plan-gen")]
pub use official::plan_gen::PlanGenPlugin;
#[cfg(feature = "pptx-gen")]
pub use official::pptx_gen::PptxGenPlugin;
#[cfg(feature = "refactor")]
pub use official::refactor::RefactorPlugin;
#[cfg(feature = "security-audit")]
pub use official::security_audit::SecurityAuditPlugin;
#[cfg(feature = "test-gen")]
pub use official::test_gen::TestGenPlugin;
#[cfg(feature = "verification")]
pub use official::verification::VerificationPlugin;

fn bundle(name: &str, description: &str, plugin: Box<dyn ErasedPlugin>) -> PluginBundle {
    PluginBundle {
        meta: PluginMeta {
            name: name.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: description.to_string(),
            author: "lattice".into(),
        },
        plugin,
        default_behavior: BehaviorMode::Yolo {
            enforce_sandbox: YoloSandboxPolicy::EnforceCommandAllowlist,
        },
        default_tools: vec![],
    }
}

pub fn official_plugin_bundles() -> Vec<PluginBundle> {
    let mut bundles = Vec::new();
    macro_rules! add_bundle {
        ($feature:literal, $name:literal, $description:literal, $plugin:ty) => {
            #[cfg(feature = $feature)]
            bundles.push(bundle($name, $description, Box::new(<$plugin>::default())));
        };
    }

    add_bundle!(
        "code-review",
        "CodeReview",
        "Code review plugin",
        CodeReviewPlugin
    );
    add_bundle!(
        "refactor",
        "Refactor",
        "Code refactoring plugin",
        RefactorPlugin
    );
    add_bundle!(
        "test-gen",
        "TestGen",
        "Test generation plugin",
        TestGenPlugin
    );
    add_bundle!(
        "security-audit",
        "SecurityAudit",
        "Security audit plugin",
        SecurityAuditPlugin
    );
    add_bundle!(
        "doc-gen",
        "DocGen",
        "Documentation generation plugin",
        DocGenPlugin
    );
    add_bundle!(
        "pptx-gen",
        "PptxGen",
        "PPTX generation plugin",
        PptxGenPlugin
    );
    add_bundle!(
        "deep-research",
        "DeepResearch",
        "Deep research plugin",
        DeepResearchPlugin
    );
    add_bundle!(
        "image-gen",
        "ImageGen",
        "Image generation plugin",
        ImageGenPlugin
    );
    add_bundle!(
        "knowledge-base",
        "KnowledgeBase",
        "Knowledge base plugin",
        KnowledgeBasePlugin
    );
    add_bundle!("plan-gen", "PlanGen", "Planning plugin", PlanGenPlugin);
    add_bundle!(
        "verification",
        "Verification",
        "Verification plugin",
        VerificationPlugin
    );

    bundles
}

pub fn register_official_plugins(registry: &PluginRegistry) -> Result<(), RegistryError> {
    for bundle in official_plugin_bundles() {
        registry.register(bundle)?;
    }
    Ok(())
}
