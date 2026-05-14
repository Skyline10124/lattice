use std::sync::Arc;

use anyhow::Result;
use lattice_plugin::registry::PluginRegistry;

pub(crate) fn build_plugin_registry() -> Result<Arc<PluginRegistry>> {
    let registry = Arc::new(PluginRegistry::new());
    lattice_plugins::register_official_plugins(&registry)
        .map_err(|err| anyhow::anyhow!("Failed to register official plugins: {err}"))?;
    Ok(registry)
}

pub(crate) fn sorted_plugin_names(registry: &PluginRegistry) -> Vec<String> {
    let mut names = registry.names();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names
}
