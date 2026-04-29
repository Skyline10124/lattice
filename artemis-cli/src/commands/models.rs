use anyhow::Result;
use artemis_core::router::ModelRouter;
use colored::Colorize;

pub fn run(auth_only: bool) -> Result<()> {
    let router = ModelRouter::new();
    let models = if auth_only {
        router.list_authenticated_models()
    } else {
        router.list_models()
    };

    let authed = router.list_authenticated_models();

    for m in models {
        let icon = if authed.contains(&m) { "\u2713" } else { "\u2717" };
        let color = if authed.contains(&m) { m.green() } else { m.red() };
        println!("{} {}", icon, color);
    }

    Ok(())
}
