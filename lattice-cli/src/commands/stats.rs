use crate::session::SessionManager;
use anyhow::Result;
use colored::Colorize;

pub fn run() -> Result<()> {
    let manager = SessionManager::new();
    let sessions = manager.list()?;

    println!("{}", "Session Statistics".bold());
    println!();

    if sessions.is_empty() {
        println!("{}", "No sessions recorded.".yellow());
        return Ok(());
    }

    let total_sessions = sessions.len();
    let total_messages: usize = sessions.iter().map(|s| s.message_count).sum();

    // Per-model breakdown
    let mut model_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for s in &sessions {
        *model_counts.entry(s.model.clone()).or_insert(0) += 1;
    }

    println!("  Total sessions:  {}", total_sessions);
    println!("  Total messages:  {}", total_messages);
    println!();
    println!("{}", "By Model:".bold());
    for (model, count) in model_counts.iter() {
        println!("  {}  {}", count, model.cyan());
    }

    Ok(())
}
