use anyhow::Result;
use lattice_core::catalog::Catalog;
use lattice_core::router::ModelRouter;
use colored::Colorize;

use crate::config::Config;
use crate::credentials::CredentialStore;

pub fn run(config: &Config, creds: &CredentialStore) -> Result<()> {
    println!("{} Artemis v0.1.0\n", "\u{F06A9}".dimmed());

    // Credentials
    println!("{}", "Credentials:".bold());
    for (key, status) in creds.diagnostics() {
        let icon = if status { "\u{2713}" } else { "\u{2717}" };
        let color = if status {
            "set".green()
        } else {
            "not set".red()
        };
        println!("  {} {}: {}", icon, key, color);
    }

    // Models
    println!("\n{}", "Models:".bold());
    let router = ModelRouter::with_credentials(creds.to_hashmap());
    let authed = router.list_authenticated_models();
    let all = router.list_models();
    let authed_set: std::collections::HashSet<_> = authed.iter().cloned().collect();

    for m in &all[..all.len().min(20)] {
        let icon = if authed_set.contains(m) {
            "\u{2713}"
        } else {
            "\u{2717}"
        };
        let color = if authed_set.contains(m) {
            m.green()
        } else {
            m.red()
        };
        println!("  {} {}", icon, color);
    }
    if all.len() > 20 {
        println!("  ... and {} more", all.len() - 20);
    }

    // Catalog
    let catalog = Catalog::get()?;
    println!(
        "\n{}: {} models, {} aliases",
        "Catalog".bold(),
        catalog.model_count(),
        catalog.aliases().len()
    );

    // Config
    println!("\n{}: {}", "Config".bold(), config.path.display());

    Ok(())
}
