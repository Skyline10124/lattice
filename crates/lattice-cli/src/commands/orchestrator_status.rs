use anyhow::Result;
use colored::Colorize;
use lattice_orchestrator::LatticeDir;
use std::path::PathBuf;

/// Show orchestrator status: discover .lattice/ directory, display agents and orchestrator config.
pub fn run(json: bool, project_dir: Option<String>) -> Result<()> {
    let root = project_dir_path(project_dir);

    let ld = LatticeDir::discover(&root).map_err(|e| {
        let msg = format!("{}", e);
        if msg.contains(".lattice/ directory not found") {
            anyhow::anyhow!(
                "LATTICE directory not found. Create ~/.lattice/agents/ or specify --dir."
            )
        } else {
            anyhow::anyhow!("{}", e)
        }
    })?;
    let agents = ld.registry.list();
    let orchestrator_config = &ld.orchestrator_config;

    if json {
        let out = serde_json::json!({
            "project_root": root.display().to_string(),
            "orchestrator_config": {
                "timeout_rpc_secs": orchestrator_config.timeout_rpc_secs,
                "delivery_policy": orchestrator_config.delivery_policy,
                "subscriber_buffer": orchestrator_config.subscriber_buffer,
                "max_concurrent_rpc": orchestrator_config.max_concurrent_rpc,
                "channel_buffer_size": orchestrator_config.channel_buffer_size,
            },
            "agents": agents.iter().map(|p| serde_json::json!({
                "name": p.agent.name,
                "model": p.agent.model,
                "orchestrator": {
                    "subscribe": p.orchestrator.subscribe,
                    "publish": p.orchestrator.publish,
                    "rpc": p.orchestrator.rpc,
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

    println!("{}", "LATTICE Orchestrator Status".cyan().bold());
    println!();

    println!("{}", "── Orchestrator Configuration ──".dimmed());
    println!(
        "  RPC timeout:       {}s",
        orchestrator_config.timeout_rpc_secs
    );
    println!(
        "  Delivery policy:   {}",
        orchestrator_config.delivery_policy
    );
    println!(
        "  Subscriber buffer: {}",
        orchestrator_config.subscriber_buffer
    );
    println!(
        "  Max RPC:           {}",
        orchestrator_config.max_concurrent_rpc
    );
    println!(
        "  Channel buffer:    {}",
        orchestrator_config.channel_buffer_size
    );
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

        if !profile.orchestrator.subscribe.is_empty() {
            println!(
                "    subscribes: {}",
                profile.orchestrator.subscribe.join(", ")
            );
        }
        if !profile.orchestrator.publish.is_empty() {
            println!(
                "    publishes:  {}",
                profile.orchestrator.publish.join(", ")
            );
        }
        if !profile.orchestrator.rpc.is_empty() {
            println!("    RPC whitelist: {}", profile.orchestrator.rpc.join(", "));
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
    if let Some(p) = override_path {
        PathBuf::from(p)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home) // discover() looks for $HOME/.lattice/agents/
    } else {
        PathBuf::from(".")
    }
}
