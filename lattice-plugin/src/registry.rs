use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::bundle::{PluginBundle, PluginMeta};

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("plugin '{0}' already registered")]
    DuplicateName(String),
}

pub struct PluginRegistry {
    plugins: RwLock<HashMap<String, Arc<PluginBundle>>>,
}

impl PluginRegistry {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, bundle: PluginBundle) -> Result<(), RegistryError> {
        let name = bundle.meta.name.clone();
        let mut plugins = self.plugins.write().unwrap_or_else(|e| e.into_inner());
        if plugins.contains_key(&name) {
            return Err(RegistryError::DuplicateName(name));
        }
        plugins.insert(name, Arc::new(bundle));
        Ok(())
    }

    /// Insert or replace a plugin bundle.
    ///
    /// Existing `Arc<PluginBundle>` handles keep running with the old plugin;
    /// later registry lookups see the replacement.
    pub fn replace(
        &self,
        bundle: PluginBundle,
    ) -> Result<Option<Arc<PluginBundle>>, RegistryError> {
        let name = bundle.meta.name.clone();
        let mut plugins = self.plugins.write().unwrap_or_else(|e| e.into_inner());
        Ok(plugins.insert(name, Arc::new(bundle)))
    }

    /// Remove a plugin by name.
    pub fn remove(&self, name: &str) -> Option<Arc<PluginBundle>> {
        let mut plugins = self.plugins.write().unwrap_or_else(|e| e.into_inner());
        plugins.remove(name)
    }

    pub fn replace_all(&self, bundles: Vec<PluginBundle>) -> Result<(), RegistryError> {
        let mut next = HashMap::with_capacity(bundles.len());
        for bundle in bundles {
            let name = bundle.meta.name.clone();
            if next.contains_key(&name) {
                return Err(RegistryError::DuplicateName(name));
            }
            next.insert(name, Arc::new(bundle));
        }

        let mut plugins = self.plugins.write().unwrap_or_else(|e| e.into_inner());
        *plugins = next;
        Ok(())
    }

    /// Return all currently registered plugin names.
    pub fn names(&self) -> Vec<String> {
        let plugins = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        plugins.keys().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<PluginBundle>> {
        let plugins = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        plugins.get(name).cloned()
    }

    pub fn list(&self) -> Vec<PluginMeta> {
        let plugins = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        plugins.values().map(|b| b.meta.clone()).collect()
    }

    pub fn len(&self) -> usize {
        let plugins = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        let plugins = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        plugins.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use crate::bundle::{BehaviorMode, PluginBundle, PluginMeta};
    use crate::CodeReviewPlugin;

    use super::*;

    fn bundle(name: &str, description: &str) -> PluginBundle {
        PluginBundle {
            meta: PluginMeta {
                name: name.into(),
                version: "0.1".into(),
                description: description.into(),
                author: "test".into(),
            },
            plugin: Box::new(CodeReviewPlugin::new()),
            default_behavior: BehaviorMode::Yolo,
            default_tools: vec![],
        }
    }

    #[test]
    fn replace_swaps_future_lookups_without_mutating_old_arc() {
        let registry = PluginRegistry::new();
        registry.register(bundle("p", "old")).unwrap();
        let old = registry.get("p").unwrap();

        let replaced = registry.replace(bundle("p", "new")).unwrap();
        assert!(replaced.is_some());
        assert_eq!(old.meta.description, "old");
        assert_eq!(registry.get("p").unwrap().meta.description, "new");
    }

    #[test]
    fn remove_deletes_future_lookups() {
        let registry = PluginRegistry::new();
        registry.register(bundle("p", "old")).unwrap();
        assert!(registry.remove("p").is_some());
        assert!(registry.get("p").is_none());
    }

    #[test]
    fn replace_all_keeps_old_registry_when_new_set_is_invalid() {
        let registry = PluginRegistry::new();
        registry.register(bundle("old", "keep")).unwrap();

        let result = registry.replace_all(vec![bundle("p", "one"), bundle("p", "two")]);

        assert!(matches!(result, Err(RegistryError::DuplicateName(name)) if name == "p"));
        assert_eq!(registry.get("old").unwrap().meta.description, "keep");
        assert!(registry.get("p").is_none());
    }
}
