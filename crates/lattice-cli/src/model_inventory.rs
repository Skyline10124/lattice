use std::collections::{BTreeSet, HashMap};

pub(crate) fn authenticated_models(credentials: &HashMap<String, String>) -> Vec<String> {
    let runtime = crate::runtime::model_runtime(credentials.clone());
    runtime.list_authenticated_models()
}

pub(crate) fn authenticated_providers(credentials: &HashMap<String, String>) -> Vec<String> {
    let mut providers = Vec::new();
    let mut seen = BTreeSet::new();
    for model in authenticated_models(credentials) {
        if let Some((provider, _)) = model.split_once('/') {
            if seen.insert(provider.to_string()) {
                providers.push(provider.to_string());
            }
        }
    }
    providers
}
