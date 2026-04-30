use std::sync::Arc;

use lattice_harness::{AgentRegistry, Pipeline};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    // Load agents from ~/.lattice/agents/
    let agents_dir = std::env::var("LATTICE_AGENTS_DIR").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{}/.lattice/agents", home)
    });

    println!("Loading agents from: {}", agents_dir);
    let registry = Arc::new(AgentRegistry::load_dir(std::path::Path::new(&agents_dir))?);

    let profiles = registry.list();
    println!("Loaded {} agent(s):", profiles.len());
    for p in &profiles {
        println!(
            "  - {} (model: {}, rules: {})",
            p.agent.name,
            p.agent.model,
            p.handoff.handoff_rules.len()
        );
    }

    // Review a small snippet for quick demo
    let code = r#"
fn string_contains(a: &Value, b: &Value) -> bool {
    let sa = match a {
        Value::String(s) => s.as_str(),
        _ => return false,
    };
    let sb = match b {
        Value::String(s) => s.as_str(),
        other => &other.to_string(),
    };
    sa.contains(sb)
}
"#;
    let input = format!(
        "Review this Rust function for issues. Return JSON only.\n\n```rust\n{}\n```",
        code
    );

    println!("\n=== Running pipeline: code-review → refactor ===");
    println!("Input size: {} bytes", input.len());

    let mut pipeline = Pipeline::new("dogfood", registry, None, None);
    let result = pipeline.run("code-review", &input);

    println!("\n=== Pipeline Results ===");
    println!("Completed: {}", result.completed);
    println!("Duration: {}ms", result.duration_ms);
    println!("Agents run: {}", result.results.len());
    println!("Errors: {}", result.errors.len());
    println!("Skipped: {:?}", result.skipped);

    for r in &result.results {
        println!("\n--- {} ({}ms) ---", r.agent_name, r.duration_ms);
        println!("Next: {:?}", r.next);
        let output_str = r.output.to_string();
        if output_str.len() > 2000 {
            println!("Output: {}...", &output_str[..2000]);
        } else {
            println!("Output: {}", output_str);
        }
    }

    for e in &result.errors {
        println!("\n[ERROR] {}: {}", e.agent_name, e.message);
    }

    Ok(())
}
