use anyhow::Result;
use colored::Colorize;
use lattice_core::router::ModelRouter;
use std::collections::HashSet;

use crate::display::status_icon;

pub fn run(auth_only: bool) -> Result<()> {
    let router = ModelRouter::new();
    let authed: HashSet<String> = router.list_authenticated_models().into_iter().collect();
    let models: Vec<String> = if auth_only {
        authed.iter().cloned().collect()
    } else {
        router.list_models()
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
