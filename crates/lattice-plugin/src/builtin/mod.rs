use crate::bundle::{BehaviorMode, PluginBundle, PluginMeta, YoloSandboxPolicy};
use crate::erased::ErasedPlugin;
use crate::registry::{PluginRegistry, RegistryError};

pub mod superpowers;

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

fn superpowers_bundle(skill: &'static superpowers::SuperpowerSkill) -> PluginBundle {
    bundle(
        &format!("superpowers:{}", skill.key),
        skill.description,
        Box::new(superpowers::SuperpowerSkillPlugin::new(skill)),
    )
}

pub fn builtin_plugin_bundles() -> Vec<PluginBundle> {
    let mut bundles = Vec::new();

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
