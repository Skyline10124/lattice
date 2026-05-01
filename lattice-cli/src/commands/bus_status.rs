use anyhow::Result;
use colored::Colorize;
use lattice_harness::LatticeDir;
use std::path::PathBuf;

/// Show bus status: discover .lattice/ directory, display agents and bus config.
pub fn run(json: bool, project_dir: Option<String>) -> Result<()> {
    let root = project_dir_path(project_dir);

    let ld = LatticeDir::discover(&root).map_err(|e| anyhow::anyhow!("{}", e))?;
    let agents = ld.registry.list();
    let bus_config = &ld.bus_config;

    if json {
        let out = serde_json::json!({
            "project_root": root.display().to_string(),
            "bus_config": {
                "timeout_rpc_secs": bus_config.timeout_rpc_secs,
                "delivery_policy": bus_config.delivery_policy,
                "subscriber_buffer": bus_config.subscriber_buffer,
                "max_concurrent_calls": bus_config.max_concurrent_calls,
            },
            "agents": agents.iter().map(|p| serde_json::json!({
                "name": p.agent.name,
                "model": p.agent.model,
                "bus": {
                    "subscribe": p.bus.subscribe,
                    "publish": p.bus.publish,
                    "rpc": p.bus.rpc,
                },
                "memory": {
                    "shared_read": p.memory.shared_read,
                    "shared_write": p.memory.shared_write,
                },
            })).collect::<Vec<_>>(),
            "shared_db": ld.shared_db_path().display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("{}", "LATTICE Bus Status".cyan().bold());
    println!();

    println!("{}", "── Bus Configuration ──".dimmed());
    println!("  RPC timeout:       {}s", bus_config.timeout_rpc_secs);
    println!("  Delivery policy:   {}", bus_config.delivery_policy);
    println!("  Subscriber buffer: {}", bus_config.subscriber_buffer);
    println!("  Max concurrent:    {}", bus_config.max_concurrent_calls);
    println!("  Shared DB:         {}", ld.shared_db_path().display());
    println!();

    if agents.is_empty() {
        println!("{}", "No agents registered.".yellow());
        return Ok(());
    }

    println!("{}", format!("── {} Agent(s) ──", agents.len()).cyan());
    for profile in agents {
        println!();
        println!("  {} ({})", profile.agent.name.green(), profile.agent.model);

        if !profile.bus.subscribe.is_empty() {
            println!("    subscribes: {}", profile.bus.subscribe.join(", "));
        }
        if !profile.bus.publish.is_empty() {
            println!("    publishes:  {}", profile.bus.publish.join(", "));
        }
        if !profile.bus.rpc.is_empty() {
            println!("    RPC whitelist: {}", profile.bus.rpc.join(", "));
        }
        if !profile.memory.shared_read.is_empty() {
            println!(
                "    reads shared:  {}",
                profile.memory.shared_read.join(", ")
            );
        }
        if !profile.memory.shared_write.is_empty() {
            println!(
                "    writes shared: {}",
                profile.memory.shared_write.join(", ")
            );
        }
    }

    Ok(())
}

fn project_dir_path(override_path: Option<String>) -> PathBuf {
    override_path
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
