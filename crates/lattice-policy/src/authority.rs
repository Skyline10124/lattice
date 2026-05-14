use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::approval::{ApprovalGrant, ApprovalKind};
use crate::decision::PolicyDecision;
use crate::denial::{DenialKind, DenialReport};
use crate::risk::{PathOperation, RiskClass};
use crate::validation::{
    dangerous_command_pattern, validate_command_exact_rule, validate_command_prefix_rule,
    validate_path_rule, ValidationError,
};

// ---------------------------------------------------------------------------
// ToolCatalog trait — minimal abstraction so lattice-policy can validate
// tool/bundle rules without depending on lattice-tools.
// ---------------------------------------------------------------------------

/// Minimal tool catalog abstraction for tool/bundle validation in policy.
pub trait ToolCatalog: Send + Sync {
    fn tool_exists(&self, name: &str) -> bool;
    fn bundle_exists(&self, name: &str) -> bool;
    fn bundle_contains(&self, bundle_name: &str, tool_name: &str) -> bool;
}

// ---------------------------------------------------------------------------
// Command matching helpers
// ---------------------------------------------------------------------------

/// Check if a command matches an allowlist entry using token-prefix matching.
///
/// Single-word entries (e.g. "grep") match by program name only.
/// Multi-word entries (e.g. "cargo test") match by program + subcommand prefix.
pub fn command_matches_allowlist(cmd: &str, entry: &str) -> bool {
    if contains_shell_control(cmd) {
        return false;
    }

    let cmd_tokens: Vec<&str> = cmd.split_whitespace().take(4).collect();
    let entry_tokens: Vec<&str> = entry.split_whitespace().collect();

    if cmd_tokens.is_empty() {
        return false;
    }
    if entry_tokens.len() == 1 {
        return cmd_tokens[0] == entry_tokens[0];
    }
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

fn contains_shell_control(command: &str) -> bool {
    ["&&", "||", ";", "|", "$(", "`", ">", "<", "\n", "\r"]
        .iter()
        .any(|operator| command.contains(operator))
}

/// Exact command match — only edge whitespace is ignored.
pub fn command_matches_exact(cmd: &str, approved: &str) -> bool {
    cmd.trim() == approved.trim()
}

// ---------------------------------------------------------------------------
// Path matching helpers
// ---------------------------------------------------------------------------

/// Check if `check_path` starts with `prefix` at a component boundary.
///
/// Returns true if `check_path` is a true descendant of `prefix`. This prevents
/// byte-prefix attacks where "/home/user/src-hack/" would incorrectly match
/// "/home/user/src".
pub fn is_path_prefix(check_path: &Path, prefix: &Path) -> bool {
    let check_comps: Vec<_> = check_path.components().collect();
    let prefix_comps: Vec<_> = prefix.components().collect();
    if prefix_comps.len() >= check_comps.len() {
        return false;
    }
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

/// Exact path match — resolved path must equal the approved path.
/// Falls back to relative-to-cwd resolution if the approved path is relative.
pub fn path_matches_exact(check_path: &Path, approved_path: &Path) -> bool {
    check_path == resolve_policy_path(approved_path)
}

/// Recursive path match — check_path is inside root at a component boundary.
pub fn path_matches_recursive(check_path: &Path, root: &Path) -> bool {
    let resolved_root = resolve_policy_path(root);
    check_path == resolved_root || is_path_prefix(check_path, &resolved_root)
}

fn resolve_policy_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    }

    let Ok(cwd) = std::env::current_dir() else {
        return path.to_path_buf();
    };
    let joined = cwd.join(path);
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

/// Check if a canonical path matches any static allowlist prefix using
/// component-boundary matching.
pub fn path_matches_static_allowlist(check_path: &Path, allowlist: &[String]) -> bool {
    let cwd = std::env::current_dir().ok();
    allowlist.iter().any(|prefix| {
        let prefix_path = Path::new(prefix);
        if check_path == prefix_path {
            return true;
        }
        if is_path_prefix(check_path, prefix_path) {
            return true;
        }
        if let Some(ref cwd) = cwd {
            let full = cwd.join(prefix);
            if check_path == full || is_path_prefix(check_path, &full) {
                return true;
            }
        }
        false
    })
}

// ---------------------------------------------------------------------------
// PolicyAuthority
// ---------------------------------------------------------------------------

/// Central authority for dynamic approval grants shared across runtime,
/// sandbox, and tool executor.
///
/// Cloneable handle around `Arc<Mutex<Vec<ApprovalGrant>>>`. All clones
/// share the same backing store, so a rule approved via the runtime is
/// immediately visible to sandbox checks.
#[derive(Clone)]
pub struct PolicyAuthority {
    grants: Arc<Mutex<Vec<ApprovalGrant>>>,
}

impl Default for PolicyAuthority {
    fn default() -> Self {
        Self::new()
    }
}

impl PolicyAuthority {
    /// Create a new `PolicyAuthority` with an empty grant store.
    pub fn new() -> Self {
        Self {
            grants: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Wrap an existing shared `Arc<Mutex<Vec<ApprovalGrant>>>` as a
    /// `PolicyAuthority`. Used for backward compatibility when the shared
    /// state was created before the authority.
    pub fn from_shared_rules(grants: Arc<Mutex<Vec<ApprovalGrant>>>) -> Self {
        Self { grants }
    }

    /// Access the inner `Arc<Mutex<Vec<ApprovalGrant>>>` for direct
    /// manipulation by callers that need it (e.g., `RuntimeSecurity`).
    pub fn inner(&self) -> &Arc<Mutex<Vec<ApprovalGrant>>> {
        &self.grants
    }

    // -- approval -----------------------------------------------------------

    /// Validate and store an approval grant.
    ///
    /// Rejects overly broad or dangerous rules before adding them.
    /// Bundle validation requires an optional `ToolCatalog`; when absent,
    /// bundle rules pass validation without existence checking (caller
    /// should validate externally).
    pub fn approve(
        &self,
        grant: ApprovalGrant,
        catalog: Option<&dyn ToolCatalog>,
    ) -> Result<(), ValidationError> {
        match &grant.kind {
            ApprovalKind::CommandExact { command } => validate_command_exact_rule(command)?,
            ApprovalKind::CommandPrefix { prefix } => validate_command_prefix_rule(prefix)?,
            ApprovalKind::PathExact { path, .. }
            | ApprovalKind::PathRecursive { root: path, .. } => {
                validate_path_rule(path.to_string_lossy().as_ref())?
            }
            ApprovalKind::Tool { .. } => {} // single tool always valid
            ApprovalKind::Bundle { bundle_name } => {
                if let Some(catalog) = catalog {
                    if !catalog.bundle_exists(bundle_name) {
                        return Err(ValidationError::UnknownBundle(bundle_name.clone()));
                    }
                }
            }
        }
        let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        if !grants.contains(&grant) {
            grants.push(grant);
        }
        Ok(())
    }

    /// Return a snapshot of all current grants.
    pub fn grants(&self) -> Vec<ApprovalGrant> {
        self.grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Number of stored grants.
    pub fn len(&self) -> usize {
        self.grants.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether the grant store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // -- decisions ----------------------------------------------------------

    /// Decide whether a command is allowed, consulting static allowlist
    /// and dynamic grants.
    ///
    /// Dynamic command approvals re-validate the command against dangerous
    /// command patterns — even if a rule was manually inserted bypassing
    /// approval validation, dangerous commands are still blocked.
    pub fn decide_command(&self, command: &str, static_allowlist: &[String]) -> PolicyDecision {
        if command.split_whitespace().next().unwrap_or("").is_empty() {
            return PolicyDecision::Deny(DenialReport::new(
                "empty command".into(),
                RiskClass::Mutate,
                DenialKind::Command {
                    command: command.to_string(),
                    allowed_prefixes: static_allowlist.to_vec(),
                },
            ));
        }

        // Static allowlist check
        let static_allowed = static_allowlist
            .iter()
            .any(|entry| command_matches_allowlist(command, entry));

        if static_allowed {
            return PolicyDecision::Allow;
        }

        // Dynamic grant check
        let grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        let dynamically_allowed = grants.iter().any(|grant| {
            if dangerous_command_pattern(command).is_some() {
                return false;
            }
            match &grant.kind {
                ApprovalKind::CommandExact { command: approved } => {
                    command_matches_exact(command, approved)
                }
                ApprovalKind::CommandPrefix { prefix } => {
                    command_matches_allowlist(command, prefix)
                }
                _ => false,
            }
        });

        if dynamically_allowed {
            return PolicyDecision::Allow;
        }

        PolicyDecision::Deny(DenialReport::new(
            format!(
                "command '{}' does not match any allowlist entry. Allowed: {:?}",
                command.trim(),
                static_allowlist
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<&str>>()
            ),
            RiskClass::Mutate,
            DenialKind::Command {
                command: command.trim().to_string(),
                allowed_prefixes: static_allowlist.to_vec(),
            },
        ))
    }

    /// Decide whether a path operation is allowed, consulting static
    /// allowlist and dynamic grants.
    ///
    /// `check_path` should already be canonicalized by the caller
    /// (sandbox resolves before calling).
    pub fn decide_path(
        &self,
        check_path: &Path,
        operation: PathOperation,
        static_allowlist: &[String],
    ) -> PolicyDecision {
        // Static allowlist check
        if path_matches_static_allowlist(check_path, static_allowlist) {
            return PolicyDecision::Allow;
        }

        // Dynamic grant check
        let grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        let dynamically_allowed = grants.iter().any(|grant| match &grant.kind {
            ApprovalKind::PathExact {
                path,
                operation: op,
            } if *op == operation => path_matches_exact(check_path, path),
            ApprovalKind::PathRecursive {
                root,
                operation: op,
            } if *op == operation => path_matches_recursive(check_path, root),
            _ => false,
        });

        if dynamically_allowed {
            return PolicyDecision::Allow;
        }

        let op_str = match operation {
            PathOperation::Read => "read",
            PathOperation::Write => "write",
        };

        PolicyDecision::Deny(DenialReport::new(
            format!(
                "path '{}' is not in {} allowlist: {:?}",
                check_path.display(),
                op_str,
                static_allowlist
            ),
            match operation {
                PathOperation::Read => RiskClass::Read,
                PathOperation::Write => RiskClass::Mutate,
            },
            DenialKind::Path {
                path: check_path.to_string_lossy().to_string(),
                operation: op_str.to_string(),
                allowlist: static_allowlist.to_vec(),
            },
        ))
    }

    /// Decide whether a tool is allowed, consulting static enabled set
    /// and dynamic grants.
    ///
    /// Bundle approval requires a `ToolCatalog` to resolve bundle membership.
    /// When no catalog is provided, bundle grants never match.
    pub fn decide_tool(
        &self,
        tool_name: &str,
        static_enabled: Option<&BTreeSet<String>>,
        catalog: Option<&dyn ToolCatalog>,
    ) -> PolicyDecision {
        // Static enabled check
        if let Some(enabled) = static_enabled {
            if enabled.contains(tool_name) {
                return PolicyDecision::Allow;
            }
        }

        // Dynamic grant check
        let grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        let dynamically_allowed = grants.iter().any(|grant| match &grant.kind {
            ApprovalKind::Tool {
                tool_name: approved,
            } => approved == tool_name,
            ApprovalKind::Bundle { bundle_name } => catalog
                .map(|c| c.bundle_contains(bundle_name, tool_name))
                .unwrap_or(false),
            _ => false,
        });

        if dynamically_allowed {
            return PolicyDecision::Allow;
        }

        let enabled_tools: Vec<String> = static_enabled
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();

        PolicyDecision::Deny(DenialReport::new(
            format!("tool '{}' is not enabled", tool_name),
            RiskClass::Mutate,
            DenialKind::Tool {
                tool_name: tool_name.to_string(),
                enabled_tools,
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::ApprovalKind;
    use crate::risk::RiskClass;
    use std::path::PathBuf;

    // -- command matching ---------------------------------------------------

    #[test]
    fn test_command_matches_allowlist_single_word() {
        assert!(command_matches_allowlist("grep foo", "grep"));
        assert!(!command_matches_allowlist("find foo", "grep"));
    }

    #[test]
    fn test_command_matches_allowlist_prefix() {
        assert!(command_matches_allowlist("cargo test -p foo", "cargo test"));
        assert!(!command_matches_allowlist("cargo build", "cargo test"));
    }

    #[test]
    fn test_command_matches_allowlist_rejects_shell_control() {
        for command in [
            "cargo test && rm -rf target",
            "cargo test; echo done",
            "cargo test | cat",
            "cargo test $(echo hi)",
            "cargo test > /tmp/out",
        ] {
            assert!(
                !command_matches_allowlist(command, "cargo test"),
                "{command} should not match static allowlist"
            );
        }
    }

    #[test]
    fn test_command_matches_exact_no_extra_args() {
        assert!(command_matches_exact("cargo build", "cargo build"));
        assert!(command_matches_exact("  cargo build  ", "cargo build"));
        assert!(!command_matches_exact(
            "cargo build --release",
            "cargo build"
        ));
        assert!(!command_matches_exact("cargo   build", "cargo build"));
    }

    // -- path matching ------------------------------------------------------

    #[test]
    fn test_is_path_prefix_component_boundary() {
        assert!(is_path_prefix(
            Path::new("/home/user/src/main.rs"),
            Path::new("/home/user/src")
        ));
        assert!(!is_path_prefix(
            Path::new("/home/user/src-hack/file.rs"),
            Path::new("/home/user/src")
        ));
        assert!(!is_path_prefix(
            Path::new("/home/user/src"),
            Path::new("/home/user/src")
        ));
    }

    #[test]
    fn test_path_matches_recursive_component_boundary() {
        assert!(path_matches_recursive(
            Path::new("/home/user/project/file.txt"),
            Path::new("/home/user/project")
        ));
        assert!(!path_matches_recursive(
            Path::new("/home/user/project-hack/file.txt"),
            Path::new("/home/user/project")
        ));
    }

    #[test]
    fn test_path_matches_recursive_relative_root() {
        let relative_root = PathBuf::from("target/lattice-policy-relative-root");
        let check_path = std::env::current_dir()
            .unwrap()
            .join(&relative_root)
            .join("child.txt");

        assert!(path_matches_recursive(&check_path, &relative_root));
    }

    // -- PolicyAuthority: shared state across clones ------------------------

    #[test]
    fn test_policy_authority_shares_state_across_clones() {
        let auth = PolicyAuthority::new();
        let clone = auth.clone();

        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::Tool {
                    tool_name: "read_file".into(),
                },
                risk_class: RiskClass::Read,
            },
            None,
        )
        .unwrap();

        assert_eq!(auth.len(), 1);
        assert_eq!(clone.len(), 1);
    }

    #[test]
    fn test_from_shared_rules_wraps_existing_arc() {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let auth = PolicyAuthority::from_shared_rules(Arc::clone(&shared));

        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::Tool {
                    tool_name: "grep".into(),
                },
                risk_class: RiskClass::Read,
            },
            None,
        )
        .unwrap();

        assert_eq!(shared.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_approve_identical_grant_is_idempotent() {
        let auth = PolicyAuthority::new();
        let grant = ApprovalGrant {
            kind: ApprovalKind::PathRecursive {
                root: PathBuf::from("/tmp/lattice-policy-idempotent"),
                operation: PathOperation::Read,
            },
            risk_class: RiskClass::Read,
        };

        auth.approve(grant.clone(), None).unwrap();
        auth.approve(grant.clone(), None).unwrap();

        assert_eq!(auth.len(), 1);
        assert_eq!(auth.grants(), vec![grant]);
    }

    // -- PolicyAuthority: decide_command ------------------------------------

    #[test]
    fn test_decide_command_static_allows() {
        let auth = PolicyAuthority::new();
        let allowlist = vec!["cargo test".to_string()];
        let result = auth.decide_command("cargo test -p foo", &allowlist);
        assert_eq!(result, PolicyDecision::Allow);
    }

    #[test]
    fn test_decide_command_static_rejects_shell_control_suffix() {
        let auth = PolicyAuthority::new();
        let allowlist = vec!["cargo test".to_string()];
        let result = auth.decide_command("cargo test && echo owned", &allowlist);
        assert!(matches!(result, PolicyDecision::Deny(_)));
    }

    #[test]
    fn test_decide_command_dynamic_allows() {
        let auth = PolicyAuthority::new();
        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::CommandPrefix {
                    prefix: "npm run build".into(),
                },
                risk_class: RiskClass::Mutate,
            },
            None,
        )
        .unwrap();

        let result = auth.decide_command("npm run build --prod", &["cargo test".into()]);
        assert_eq!(result, PolicyDecision::Allow);
    }

    #[test]
    fn test_decide_command_exact_rejects_extra_args() {
        let auth = PolicyAuthority::new();
        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::CommandExact {
                    command: "cargo publish".into(),
                },
                risk_class: RiskClass::Mutate,
            },
            None,
        )
        .unwrap();

        // Exact match OK
        assert_eq!(
            auth.decide_command("cargo publish", &[]),
            PolicyDecision::Allow
        );
        // Extra args NOT OK
        let result = auth.decide_command("cargo publish --dry-run", &[]);
        assert!(matches!(result, PolicyDecision::Deny(_)));
    }

    #[test]
    fn test_decide_command_exact_single_token_allows_only_exact_command() {
        let auth = PolicyAuthority::new();
        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::CommandExact {
                    command: "make".into(),
                },
                risk_class: RiskClass::Mutate,
            },
            None,
        )
        .unwrap();

        assert_eq!(auth.decide_command("make", &[]), PolicyDecision::Allow);
        assert!(matches!(
            auth.decide_command("make test", &[]),
            PolicyDecision::Deny(_)
        ));
    }

    #[test]
    fn test_decide_command_prefix_allows_extra_args() {
        let auth = PolicyAuthority::new();
        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::CommandPrefix {
                    prefix: "cargo publish".into(),
                },
                risk_class: RiskClass::Mutate,
            },
            None,
        )
        .unwrap();

        assert_eq!(
            auth.decide_command("cargo publish --dry-run", &[]),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn test_decide_command_rejects_dangerous_dynamic() {
        let auth = PolicyAuthority::new();
        // Manually insert a dangerous rule (bypassing validate)
        auth.grants.lock().unwrap().push(ApprovalGrant {
            kind: ApprovalKind::CommandPrefix {
                prefix: "rm -rf /tmp".into(),
            },
            risk_class: RiskClass::Destructive,
        });

        let result = auth.decide_command("rm -rf /tmp/foo", &[]);
        assert!(matches!(result, PolicyDecision::Deny(_)));
    }

    // -- PolicyAuthority: decide_path ---------------------------------------

    #[test]
    fn test_decide_path_static_allows() {
        let auth = PolicyAuthority::new();
        let allowlist = vec!["/home/user/project".to_string()];

        let result = auth.decide_path(
            Path::new("/home/user/project/src/main.rs"),
            PathOperation::Read,
            &allowlist,
        );
        assert_eq!(result, PolicyDecision::Allow);
    }

    #[test]
    fn test_decide_path_static_rejects_sibling() {
        let auth = PolicyAuthority::new();
        let allowlist = vec!["/home/user/project".to_string()];

        let result = auth.decide_path(
            Path::new("/home/user/project-hack/file.txt"),
            PathOperation::Read,
            &allowlist,
        );
        assert!(matches!(result, PolicyDecision::Deny(_)));
    }

    #[test]
    fn test_decide_path_dynamic_exact_allows() {
        let auth = PolicyAuthority::new();
        let tmp = std::env::temp_dir().join("lattice-policy-path-exact-test");
        std::fs::create_dir_all(&tmp).ok();
        let file_path = tmp.join("test.txt");
        std::fs::write(&file_path, "hello").ok();

        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::PathExact {
                    path: file_path.clone(),
                    operation: PathOperation::Read,
                },
                risk_class: RiskClass::Read,
            },
            None,
        )
        .unwrap();

        let result = auth.decide_path(&file_path.canonicalize().unwrap(), PathOperation::Read, &[]);
        assert_eq!(
            result,
            PolicyDecision::Allow,
            "exact path match should allow"
        );

        // Sibling should not match
        let sibling = tmp.join("other.txt");
        std::fs::write(&sibling, "nope").ok();
        let result = auth.decide_path(&sibling.canonicalize().unwrap(), PathOperation::Read, &[]);
        assert!(
            matches!(result, PolicyDecision::Deny(_)),
            "sibling path should be denied"
        );
    }

    #[test]
    fn test_decide_path_dynamic_recursive_allows() {
        let auth = PolicyAuthority::new();

        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::PathRecursive {
                    root: PathBuf::from("/tmp/lattice-policy-recursive-test"),
                    operation: PathOperation::Read,
                },
                risk_class: RiskClass::Read,
            },
            None,
        )
        .unwrap();

        let result = auth.decide_path(
            Path::new("/tmp/lattice-policy-recursive-test/sub/file.txt"),
            PathOperation::Read,
            &[],
        );
        assert_eq!(result, PolicyDecision::Allow);
    }

    #[test]
    fn test_decide_path_recursive_rejects_sibling() {
        let auth = PolicyAuthority::new();

        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::PathRecursive {
                    root: PathBuf::from("/tmp/lattice-policy-approved"),
                    operation: PathOperation::Read,
                },
                risk_class: RiskClass::Read,
            },
            None,
        )
        .unwrap();

        let result = auth.decide_path(
            Path::new("/tmp/lattice-policy-approved-hack/file.txt"),
            PathOperation::Read,
            &[],
        );
        assert!(
            matches!(result, PolicyDecision::Deny(_)),
            "component-boundary must reject sibling prefix match"
        );
    }

    // -- PolicyAuthority: decide_tool ---------------------------------------

    struct TestCatalog;
    impl ToolCatalog for TestCatalog {
        fn tool_exists(&self, name: &str) -> bool {
            matches!(name, "read_file" | "write_file" | "workspace_context")
        }
        fn bundle_exists(&self, name: &str) -> bool {
            name == "workspace"
        }
        fn bundle_contains(&self, bundle_name: &str, tool_name: &str) -> bool {
            bundle_name == "workspace" && tool_name == "workspace_context"
        }
    }

    #[test]
    fn test_decide_tool_static_allows() {
        let auth = PolicyAuthority::new();
        let mut enabled = BTreeSet::new();
        enabled.insert("read_file".to_string());

        let result = auth.decide_tool("read_file", Some(&enabled), None);
        assert_eq!(result, PolicyDecision::Allow);
    }

    #[test]
    fn test_decide_tool_dynamic_tool_allows() {
        let auth = PolicyAuthority::new();
        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::Tool {
                    tool_name: "workspace_context".into(),
                },
                risk_class: RiskClass::Read,
            },
            None,
        )
        .unwrap();

        let enabled = BTreeSet::from(["read_file".to_string()]);
        let result = auth.decide_tool("workspace_context", Some(&enabled), None);
        assert_eq!(result, PolicyDecision::Allow);
    }

    #[test]
    fn test_decide_tool_dynamic_bundle_allows() {
        let auth = PolicyAuthority::new();
        auth.approve(
            ApprovalGrant {
                kind: ApprovalKind::Bundle {
                    bundle_name: "workspace".into(),
                },
                risk_class: RiskClass::Read,
            },
            None,
        )
        .unwrap();

        let catalog = TestCatalog;
        let enabled = BTreeSet::from(["read_file".to_string()]);
        let result = auth.decide_tool("workspace_context", Some(&enabled), Some(&catalog));
        assert_eq!(result, PolicyDecision::Allow);
    }

    #[test]
    fn test_decide_tool_denies_unknown() {
        let auth = PolicyAuthority::new();
        let enabled = BTreeSet::from(["read_file".to_string()]);
        let result = auth.decide_tool("bash", Some(&enabled), None);
        assert!(matches!(result, PolicyDecision::Deny(_)));
    }
}
