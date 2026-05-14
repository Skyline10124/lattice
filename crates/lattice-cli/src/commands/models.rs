use anyhow::Result;
use colored::Colorize;
use std::collections::HashSet;

use crate::credentials::CredentialStore;
use crate::display::status_icon;

pub fn run(auth_only: bool, creds: &CredentialStore) -> Result<()> {
    let runtime = crate::runtime::model_runtime(creds.to_hashmap());
    let authed: HashSet<String> = runtime.list_authenticated_models().into_iter().collect();
    let models: Vec<String> = if auth_only {
        authed.iter().cloned().collect()
    } else {
        runtime.list_models()
    };

    for m in models {
        let ok = authed.contains(&m);
        println!(
            "{} {}",
            status_icon(ok),
            if ok { m.green() } else { m.red() }
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_models_list_ok() {
        // list_models() only reads the catalog — no credentials needed.
        let creds = super::super::super::credentials::CredentialStore::from_values(
            std::collections::HashMap::new(),
        );
        let result = run(false, &creds);
        assert!(result.is_ok(), "models list should not error");
    }

    #[test]
    fn test_models_auth_only_ok() {
        // Even without credentials, auth_only should not error
        // (the router returns an empty list when no env vars are set).
        let creds = super::super::super::credentials::CredentialStore::from_values(
            std::collections::HashMap::new(),
        );
        let result = run(true, &creds);
        assert!(result.is_ok(), "models --auth should not error");
    }
}
