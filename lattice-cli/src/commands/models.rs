use anyhow::Result;
use colored::Colorize;
use lattice_core::router::ModelRouter;
use std::collections::HashSet;

pub fn run(auth_only: bool) -> Result<()> {
    let router = ModelRouter::new();
    let authed: HashSet<String> = router.list_authenticated_models().into_iter().collect();
    let models: Vec<String> = if auth_only {
        authed.iter().cloned().collect()
    } else {
        router.list_models()
    };

    for m in models {
        let icon = if authed.contains(&m) {
            "\u{2713}"
        } else {
            "\u{2717}"
        };
        let color = if authed.contains(&m) {
            m.green()
        } else {
            m.red()
        };
        println!("{} {}", icon, color);
    }

    Ok(())
}
