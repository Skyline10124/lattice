use std::path::Path;

/// Check if `check_path` starts with `prefix` at a component boundary.
///
/// Returns true if `check_path` is a descendant of `prefix` (i.e., prefix is a
/// directory containing check_path). This prevents byte-prefix attacks where
/// "/home/user/src-hack/" would incorrectly match allowlist entry "/home/user/src".
///
/// Examples:
/// - is_path_prefix("/home/user/src/main.rs", "/home/user/src") -> true
/// - is_path_prefix("/home/user/src-hack/foo.rs", "/home/user/src") -> false
/// - is_path_prefix("/home/user/src", "/home/user/src") -> false (exact match, use ==)
pub fn is_path_prefix(check_path: &Path, prefix: &Path) -> bool {
    let check_comps: Vec<_> = check_path.components().collect();
    let prefix_comps: Vec<_> = prefix.components().collect();
    if prefix_comps.len() >= check_comps.len() {
        return false;
    }
    // All prefix components must match
    for (a, b) in check_comps
        .iter()
        .take(prefix_comps.len())
        .zip(prefix_comps.iter())
    {
        if a != b {
            return false;
        }
    }
    true
}

/// Check if a path matches a sensitive file pattern using path component matching.
///
/// Unlike substring matching (which can have false positives like
/// "some.env.file.txt" matching ".env"), this checks actual path components.
/// For simple patterns like ".env", it matches any path component equal to ".env".
/// For compound patterns like ".git/config", it matches consecutive components.
///
/// Supports glob-style wildcards:
/// - `*` matches any sequence of characters within a single path component
/// - `.env.*` matches `.env.local`, `.env.production`, etc.
/// - `*.pem` matches `cert.pem`, `key.pem`, etc.
/// - `id_*` matches `id_rsa`, `id_ed25519`, etc.
pub fn path_matches_sensitive(path: &Path, pattern: &str) -> bool {
    let parts: Vec<&str> = pattern.split('/').collect();
    let comps: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy()),
            _ => None,
        })
        .collect();

    if comps.is_empty() {
        return false;
    }

    if parts.len() == 1 {
        comps
            .iter()
            .any(|c| component_matches_pattern(c.as_ref(), parts[0]))
    } else {
        comps.windows(parts.len()).any(|window| {
            window
                .iter()
                .zip(parts.iter())
                .all(|(a, b)| component_matches_pattern(a.as_ref(), b))
        })
    }
}

/// Check if a single path component matches a pattern with optional `*` wildcards.
///
/// `*` matches zero or more characters (but not `/` — this is single-component only).
/// No other glob syntax is supported.
pub fn component_matches_pattern(component: &str, pattern: &str) -> bool {
    if !pattern.contains('*') {
        return component == pattern;
    }
    // Simple glob: split pattern by `*` and verify component matches
    // e.g., "*.pem" -> ["", ".pem"] -> component must end with ".pem"
    // e.g., "id_*" -> ["id_", ""] -> component must start with "id_" (trailing empty = match rest)
    // e.g., ".env.*" -> [".env.", ""] -> component must start with ".env."
    let segments: Vec<&str> = pattern.split('*').collect();
    if segments.is_empty() {
        return true;
    }
    // First segment must match the start
    if !segments[0].is_empty() && !component.starts_with(segments[0]) {
        return false;
    }
    // Last segment must match the end
    if segments.len() > 1
        && !segments.last().unwrap().is_empty()
        && !component.ends_with(segments.last().unwrap())
    {
        return false;
    }
    // All middle segments must appear in order
    let mut search_from = 0;
    for seg in &segments[1..segments.len() - 1] {
        if seg.is_empty() {
            continue;
        }
        match component[search_from..].find(*seg) {
            Some(pos) => search_from += pos + seg.len(),
            None => return false,
        }
    }
    true
}
