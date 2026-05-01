use lattice_agent::Agent;
use lattice_core::resolve;
use lattice_plugin::{
    CodeReviewPlugin, Plugin, PluginAgent, PluginConfig, PluginRunner, StrictBehavior,
};
use std::fs;

fn main() {
    let resolved = resolve("deepseek-v4-flash").expect("resolve");
    let mut agent = Agent::new(resolved);

    // Read CLI + TUI source files for review
    let mut code = String::new();
    for path in &[
        "lattice-cli/src/main.rs",
        "lattice-tui/src/app.rs",
        "lattice-tui/src/main.rs",
    ] {
        if let Ok(content) = fs::read_to_string(&format!("/home/astrin/lattice/{}", path)) {
            code.push_str(&format!(
                "\n=== {} ===\n{}",
                path,
                &content[..content.len().min(3000)]
            ));
        }
    }
    // Truncate to avoid blowing context
    let code = &code[..code.len().min(8000)];

    let plugin = CodeReviewPlugin::new();
    let behavior = StrictBehavior {
        confidence_threshold: 0.6,
        max_retries: 2,
        escalate_to: None,
    };
    let config = PluginConfig {
        max_turns: 3,
        ..Default::default()
    };

    let input = serde_json::json!({ "diff": code });
    println!("DEBUG: creating PluginRunner...");
    let mut runner = PluginRunner::new(&plugin, &behavior, &mut agent, &config, None, None, None);

    println!("DEBUG: calling runner.run...");
    let result = runner.run(&input);
    println!("DEBUG: runner.run returned");
    match result {
        Ok(result) => println!("Turns: {}\n{}", result.turns, result.output),
        Err(e) => eprintln!("Error: {}", e),
    }
}
