use anyhow::Result;
use artemis_core::catalog::Catalog;
use artemis_core::router::ModelRouter;
use colored::Colorize;
use std::time::Duration;

use crate::config::Config;
use crate::credentials::CredentialStore;

pub fn run(config: &Config, creds: &CredentialStore) -> Result<()> {
    println!("{} Artemis v0.1.0\n", "\u{F06A9}".dimmed());

    // Credentials
    println!("{}", "Credentials:".bold());
    for (key, status) in creds.diagnostics() {
        let icon = if status { "\u{2713}" } else { "\u{2717}" };
        let color = if status { "set".green() } else { "not set".red() };
        println!("  {} {}: {}", icon, key, color);
    }

    // Models
    println!("\n{}", "Models:".bold());
    let router = ModelRouter::new();
    let authed = router.list_authenticated_models();
    let all = router.list_models();
    for m in &all[..all.len().min(20)] {
        let icon = if authed.contains(m) { "\u{2713}" } else { "\u{2717}" };
        let color = if authed.contains(m) { m.green() } else { m.red() };
        println!("  {} {}", icon, color);
    }
    if all.len() > 20 {
        println!("  ... and {} more", all.len() - 20);
    }

    // Catalog
    let catalog = Catalog::get()?;
    println!("\n{}: {} models, {} aliases",
        "Catalog".bold(),
        catalog.model_count(),
        catalog.aliases().len()
    );

    // Config
    println!("\n{}: {}", "Config".bold(), config.path.display());

    Ok(())
}
