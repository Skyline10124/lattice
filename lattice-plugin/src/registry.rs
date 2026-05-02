use std::collections::HashMap;

use crate::bundle::{PluginBundle, PluginMeta};

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("plugin '{0}' already registered")]
    DuplicateName(String),
}

pub struct PluginRegistry {
    plugins: HashMap<String, PluginBundle>,
}

impl PluginRegistry {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    pub fn register(&mut self, bundle: PluginBundle) -> Result<(), RegistryError> {
        let name = bundle.meta.name.clone();
        if self.plugins.contains_key(&name) {
            return Err(RegistryError::DuplicateName(name));
        }
        self.plugins.insert(name, bundle);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&PluginBundle> {
        self.plugins.get(name)
    }

    pub fn list(&self) -> Vec<&PluginMeta> {
        self.plugins.values().map(|b| &b.meta).collect()
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}
