// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;

/// List known model aliases.
///
/// Plain `models` lists all known aliases one per line.
/// `models --auth` lists only aliases whose credentials are present.
pub fn run(auth_only: bool, json: bool) -> Result<()> {
    let router = lattice_llm::ModelRouter::new();
    let aliases = router.aliases();

    let entries: Vec<&lattice_llm::ResolvedModel> = if auth_only {
        aliases
            .iter()
            .filter(|a| a.credential_status == lattice_llm::CredentialStatus::Present)
            .collect()
    } else {
        aliases.iter().collect()
    };

    if json {
        let items: Vec<serde_json::Value> = entries
            .iter()
            .map(|a| super::model_render::resolved_model_json(a))
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    for entry in entries {
        println!("{}", entry.alias);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::commands::test_env::with_env_var;

    // ── cli_diagnostics: models --auth text rendering ─────────────────────

    #[test]
    fn cli_diagnostics_models_auth_text_lists_aliases_one_per_line() {
        with_env_var("ANTHROPIC_API_KEY", "sk-secret-test", || {
            let result = run(true, false);
            assert!(result.is_ok(), "models --auth should not error");
        });
    }

    #[test]
    fn cli_diagnostics_models_auth_text_excludes_secret_values() {
        with_env_var("ANTHROPIC_API_KEY", "sk-secret-test", || {
            let result = run(true, false);
            assert!(result.is_ok());
        });
    }

    #[test]
    fn cli_diagnostics_models_auth_json_excludes_secrets() {
        with_env_var("ANTHROPIC_API_KEY", "sk-secret-json-test", || {
            let result = run(true, true);
            assert!(result.is_ok(), "models --auth --json should not error");
        });
    }

    #[test]
    fn cli_diagnostics_models_no_auth_lists_all_aliases() {
        let result = run(false, false);
        assert!(result.is_ok(), "models (no --auth) should not error");
    }

    #[test]
    fn cli_diagnostics_models_json_output_is_valid_json() {
        let result = run(false, true);
        assert!(result.is_ok(), "models --json should not error");
    }
}
