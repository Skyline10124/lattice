/// Check if a command matches an allowlist entry using token-prefix matching.
///
/// Single-word entries (e.g. "grep") match by program name only.
/// Multi-word entries (e.g. "cargo test") match by program + subcommand prefix,
/// so "cargo test -p core" matches "cargo test" but "cargo run" does not.
pub fn command_matches_allowlist(cmd: &str, entry: &str) -> bool {
    let cmd_tokens: Vec<&str> = cmd.split_whitespace().take(4).collect();
    let entry_tokens: Vec<&str> = entry.split_whitespace().collect();

    if cmd_tokens.is_empty() {
        return false;
    }

    // Single-word entry: match program name only
    if entry_tokens.len() == 1 {
        return cmd_tokens[0] == entry_tokens[0];
    }

    // Multi-word entry: match program + subcommand prefix
    if cmd_tokens.len() < entry_tokens.len() {
        return false;
    }

    for i in 0..entry_tokens.len() {
        if cmd_tokens[i] != entry_tokens[i] {
            return false;
        }
    }
    true
}
