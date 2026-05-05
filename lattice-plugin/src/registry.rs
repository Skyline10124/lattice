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
