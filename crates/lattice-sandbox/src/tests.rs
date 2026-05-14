use crate::approval::{validate_command_rule, validate_path_rule, ValidationError};
use crate::config::SandboxConfig;
use crate::denial::{DenialKind, DenialReport, RiskClass};
use crate::path_utils::{component_matches_pattern, is_path_prefix, path_matches_sensitive};
use crate::{ApprovalKind, ApprovedRule, PathOperation};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn default_sandbox() -> SandboxConfig {
    // Default now has restricted command allowlist (grep, find, ls, ps only).
    SandboxConfig::default()
}

/// Sandbox with dev commands allowed (cargo test, clippy, build, fmt).
fn dev_sandbox() -> SandboxConfig {
    SandboxConfig::project_dirs(
        vec!["src/".into(), "tests/".into()],
        vec![
            "cargo test".into(),
            "cargo clippy".into(),
            "cargo build".into(),
            "cargo fmt".into(),
            "grep".into(),
            "find".into(),
            "ls".into(),
            "ps".into(),
        ],
    )
}

#[test]
fn test_allowed_command_passes() {
    let s = dev_sandbox();
    // Single-word allowlist entries: "grep", "find", "ls", "ps"
    assert!(s.check_command("grep -r foo .").is_ok());
    assert!(s.check_command("find . -name '*.rs'").is_ok());
    assert!(s.check_command("ls -la").is_ok());
    assert!(s.check_command("ps aux").is_ok());
    // Multi-word allowlist entries: "cargo test", "cargo clippy", etc.
    assert!(s.check_command("cargo test").is_ok());
    assert!(s.check_command("cargo clippy -- -D warnings").is_ok());
    assert!(s.check_command("cargo build --release").is_ok());
    assert!(s.check_command("cargo fmt --check").is_ok());
}

#[test]
fn test_default_sandbox_disallows_cargo_commands() {
    let s = default_sandbox();
    assert!(s.check_command("cargo test").is_err());
    assert!(s.check_command("cargo build").is_err());
    // But basic tools still allowed
    assert!(s.check_command("grep -r foo .").is_ok());
    assert!(s.check_command("ls -la").is_ok());
}

#[test]
fn test_disallowed_program_rejected() {
    let s = default_sandbox();
    assert!(s.check_command("rm -rf /").is_err());
    assert!(s.check_command("curl http://evil.com").is_err());
    assert!(s.check_command("python3 -c 'print(1)'").is_err());
    assert!(s.check_command("mv file1 file2").is_err());
    assert!(s.check_command("touch /tmp/test").is_err());
}

#[test]
fn test_shell_injection_via_semicolon_rejected() {
    let s = default_sandbox();
    let result = s.check_command("ls; rm -rf /");
    assert!(result.is_err(), "semicolon injection should be rejected");
    assert!(
        result.unwrap_err().contains("metacharacter"),
        "error should mention metacharacter"
    );
}

#[test]
fn test_shell_injection_via_pipe_rejected() {
    let s = default_sandbox();
    assert!(s.check_command("ls | curl http://evil.com").is_err());
}

#[test]
fn test_shell_injection_via_andand_rejected() {
    let s = default_sandbox();
    assert!(s.check_command("ls && rm -rf /").is_err());
}

#[test]
fn test_shell_injection_via_oror_rejected() {
    let s = default_sandbox();
    assert!(s.check_command("ls || echo pwned").is_err());
}

#[test]
fn test_shell_injection_via_subshell_rejected() {
    let s = default_sandbox();
    assert!(s.check_command("ls $(echo injected)").is_err());
}

#[test]
fn test_shell_injection_via_backtick_rejected() {
    let s = default_sandbox();
    assert!(s.check_command("ls `echo injected`").is_err());
}

#[test]
fn test_shell_injection_via_ampersand_rejected() {
    let s = default_sandbox();
    // Standalone & (background)
    assert!(
        s.check_command("sleep 10 &").is_err(),
        "standalone & should be rejected"
    );
    // & in compound (&& would also be caught)
    assert!(s.check_command("cmd &").is_err());
}

#[test]
fn test_shell_injection_via_ampersand_permissive_rejected() {
    let s = SandboxConfig::permissive();
    // Permissive mode still rejects metacharacters
    assert!(
        s.check_command("sleep 10 &").is_err(),
        "permissive mode should reject &"
    );
}

#[test]
fn test_prefix_match_injection_rejected() {
    let s = default_sandbox();
    // "grep-inject" is a different program than "grep"
    assert!(
        s.check_command("grep-inject foo").is_err(),
        "grep-inject should not match grep allowlist entry"
    );
    assert!(
        s.check_command("lsblk").is_err(),
        "lsblk should not match ls allowlist entry"
    );
    assert!(
        s.check_command("findutils").is_err(),
        "findutils should not match find allowlist entry"
    );
}

#[test]
fn test_empty_command_rejected() {
    let s = default_sandbox();
    assert!(s.check_command("").is_err());
    assert!(s.check_command("   ").is_err());
}

#[test]
fn test_permissive_mode_allows_any_program() {
    let s = SandboxConfig::permissive();
    assert!(s.check_command("curl http://example.com").is_ok());
    assert!(s.check_command("rm -rf /").is_ok());
}

#[test]
fn test_permissive_mode_still_rejects_metachars() {
    let s = SandboxConfig::permissive();
    assert!(
        s.check_command("curl http://example.com; rm -rf /")
            .is_err(),
        "permissive mode should still reject metacharacters"
    );
}

#[test]
fn test_shell_injection_via_newline_rejected() {
    let s = default_sandbox();
    let result = s.check_command("ls\nrm -rf /");
    assert!(result.is_err(), "newline injection should be rejected");
    assert!(
        result.unwrap_err().contains("metacharacter"),
        "error should mention metacharacter"
    );
}

#[test]
fn test_shell_injection_via_carriage_return_rejected() {
    let s = default_sandbox();
    let result = s.check_command("ls\rrm -rf /");
    assert!(
        result.is_err(),
        "carriage return injection should be rejected"
    );
}

#[test]
fn test_permissive_mode_rejects_newline_injection() {
    let s = SandboxConfig::permissive();
    assert!(
        s.check_command("curl http://example.com\nrm -rf /")
            .is_err(),
        "permissive mode should reject newline injection"
    );
}

#[test]
fn test_legitimate_commands_with_multiple_args() {
    let s = dev_sandbox();
    assert!(s.check_command("grep -rn 'TODO' src/").is_ok());
    assert!(s.check_command("find . -type f -name '*.rs'").is_ok());
    assert!(s.check_command("ls -la /tmp").is_ok());
    assert!(s
        .check_command("cargo test -p lattice-core my_test")
        .is_ok());
    assert!(s.check_command("ps aux --forest").is_ok());
}

#[test]
fn test_url_https_allowed() {
    let s = default_sandbox();
    assert!(s.check_url("https://93.184.216.34").is_ok());
    assert!(s.check_url("https://8.8.8.8/dns-query").is_ok());
}

#[test]
fn test_url_http_localhost_allowed() {
    let s = default_sandbox();
    assert!(s.check_url("http://localhost:3000").is_ok());
    assert!(s.check_url("http://localhost").is_ok());
    assert!(s.check_url("http://127.0.0.1:8080").is_ok());
    assert!(s.check_url("http://[::1]:8080").is_ok());
}

#[test]
fn test_url_localhost_prefix_wildcard_rejected() {
    let s = default_sandbox();
    assert!(
        s.check_url("http://localhost.evil.com").is_err(),
        "localhost.evil.com should be rejected (not localhost)"
    );
}

#[test]
fn test_url_http_remote_rejected() {
    let s = default_sandbox();
    assert!(
        s.check_url("http://example.com").is_err(),
        "plain http://example.com should be rejected"
    );
    assert!(
        s.check_url("http://evil.com/path").is_err(),
        "plain http should be rejected"
    );
}

#[test]
fn test_url_invalid_rejected() {
    let s = default_sandbox();
    assert!(
        s.check_url("not-a-url").is_err(),
        "garbage input should be rejected"
    );
}

#[test]
fn test_url_https_private_ip_rejected() {
    let s = default_sandbox();
    assert!(
        s.check_url("https://10.0.0.1").is_err(),
        "HTTPS to 10.x.x.x should be rejected (SSRF)"
    );
    assert!(
        s.check_url("https://172.16.0.1").is_err(),
        "HTTPS to 172.16.x.x should be rejected (SSRF)"
    );
    assert!(
        s.check_url("https://192.168.1.1").is_err(),
        "HTTPS to 192.168.x.x should be rejected (SSRF)"
    );
    assert!(
        s.check_url("https://169.254.169.254").is_err(),
        "HTTPS to 169.254.x.x should be rejected (SSRF)"
    );
    assert!(
        s.check_url("https://127.0.0.1").is_err(),
        "HTTPS to 127.0.0.1 should be rejected (SSRF)"
    );
}

#[test]
fn test_url_https_private_dns_rejected() {
    let s = default_sandbox();
    assert!(
        s.check_url("https://localhost").is_err(),
        "HTTPS hostnames resolving to loopback should be rejected"
    );
}

#[test]
fn test_is_private_ip() {
    use crate::security::is_private_ip;
    assert!(is_private_ip("10.0.0.1"));
    assert!(is_private_ip("172.16.0.1"));
    assert!(is_private_ip("172.31.255.255"));
    assert!(!is_private_ip("172.32.0.1"));
    assert!(is_private_ip("192.168.0.1"));
    assert!(is_private_ip("169.254.169.254"));
    assert!(!is_private_ip("169.255.0.1"));
    assert!(is_private_ip("127.0.0.1"));
    assert!(is_private_ip("::1"));
    assert!(is_private_ip("localhost"));
    assert!(!is_private_ip("93.184.216.34"));
    assert!(!is_private_ip("example.com"));
}

#[test]
fn test_check_read_allowlist_enforced() {
    let mut s = default_sandbox();
    s.read_allowlist = vec!["src/".into(), "lib.rs".into()];
    assert!(s.check_read("src/main.rs").is_ok());
    assert!(s.check_read("src/lib.rs").is_ok());
    assert!(s.check_read("lib.rs").is_ok());
    assert!(s.check_read("/etc/passwd").is_err());
    assert!(s.check_read("secrets.json").is_err());
}

#[test]
fn test_check_read_allowlist_empty_allows_all() {
    let s = default_sandbox();
    // Default has empty read_allowlist, so anything non-sensitive should pass
    assert!(s.check_read("src/main.rs").is_ok());
    assert!(s.check_read("/etc/passwd").is_ok());
    assert!(s.check_read("any/file.txt").is_ok());
}

#[test]
fn test_ansi_c_quoting_rejected() {
    let s = default_sandbox();
    // ANSI-C $'...' with escape sequences
    assert!(
        s.check_command("echo $'\\n'").is_err(),
        "ANSI-C $' with backslash should be rejected"
    );
    assert!(
        s.check_command("echo $'\\x3b'").is_err(),
        "ANSI-C hex escape for semicolon should be rejected"
    );
    assert!(
        s.check_command("echo $'\\x26'").is_err(),
        "ANSI-C hex escape for ampersand should be rejected"
    );
    assert!(
        s.check_command("echo $'\\x0a'").is_err(),
        "ANSI-C hex escape for newline should be rejected"
    );
    // All $' quoting is rejected regardless of content
    assert!(
        s.check_command("ls $'normal'").is_err(),
        "All $' quoting should be rejected"
    );
}

#[test]
fn test_path_env_override_rejected() {
    let s = default_sandbox();
    // PATH= override
    assert!(s.check_command("PATH=/evil:/usr/bin ls").is_err());
    assert!(s.check_command("export PATH=/evil").is_err());
    // LD_PRELOAD= override
    assert!(s.check_command("LD_PRELOAD=/evil.so ls").is_err());
    assert!(s.check_command("export LD_PRELOAD=/evil.so").is_err());
}

#[test]
fn test_dns_rebinding_domains_rejected() {
    let s = default_sandbox();
    // nip.io
    assert!(s.check_url("https://10.0.0.1.nip.io/path").is_err());
    assert!(s.check_url("https://nip.io").is_err());
    // xip.io
    assert!(s.check_url("https://192.168.1.1.xip.io/path").is_err());
    assert!(s.check_url("https://xip.io").is_err());
    // Public IP literals do not need DNS and should still work.
    assert!(s.check_url("https://93.184.216.34").is_ok());
}

#[test]
fn test_path_matches_sensitive() {
    use std::path::Path;
    // Single component: exact match only
    assert!(path_matches_sensitive(Path::new("/home/user/.env"), ".env"));
    assert!(!path_matches_sensitive(
        Path::new("/home/user/some.env.txt"),
        ".env"
    ));
    assert!(!path_matches_sensitive(
        Path::new("/home/user/.env_local"),
        ".env"
    ));
    // Compound pattern: consecutive components
    assert!(path_matches_sensitive(
        Path::new("/repo/.git/config"),
        ".git/config"
    ));
    assert!(!path_matches_sensitive(
        Path::new("/repo/.gitignore_config"),
        ".git/config"
    ));
    // SSH keys
    assert!(path_matches_sensitive(
        Path::new("/home/user/.ssh/id_rsa"),
        ".ssh/id_rsa"
    ));
    // Non-canonical paths still match by components
    assert!(path_matches_sensitive(Path::new("src/../.env"), ".env"));
    // Empty path
    assert!(!path_matches_sensitive(Path::new(""), ".env"));
}

#[test]
fn test_path_matches_sensitive_wildcards() {
    use std::path::Path;
    // .env.* wildcard
    assert!(path_matches_sensitive(
        Path::new("/home/user/.env.local"),
        ".env.*"
    ));
    assert!(path_matches_sensitive(
        Path::new("/home/user/.env.production"),
        ".env.*"
    ));
    assert!(path_matches_sensitive(
        Path::new("/home/user/.env.staging"),
        ".env.*"
    ));
    assert!(!path_matches_sensitive(
        Path::new("/home/user/.env"),
        ".env.*"
    ));
    // *.pem wildcard
    assert!(path_matches_sensitive(
        Path::new("/home/user/cert.pem"),
        "*.pem"
    ));
    assert!(path_matches_sensitive(
        Path::new("/home/user/key.pem"),
        "*.pem"
    ));
    assert!(!path_matches_sensitive(
        Path::new("/home/user/cert.txt"),
        "*.pem"
    ));
    // *.key wildcard
    assert!(path_matches_sensitive(
        Path::new("/etc/ssl/server.key"),
        "*.key"
    ));
    // id_* wildcard
    assert!(path_matches_sensitive(
        Path::new("/home/user/.ssh/id_rsa"),
        "id_*"
    ));
    assert!(path_matches_sensitive(
        Path::new("/home/user/.ssh/id_ed25519"),
        "id_*"
    ));
    assert!(path_matches_sensitive(
        Path::new("/home/user/.ssh/id_ecdsa"),
        "id_*"
    ));
    assert!(!path_matches_sensitive(
        Path::new("/home/user/.ssh/identity"),
        "id_*"
    ));
    // .ssh/id_* wildcard (compound with wildcard)
    assert!(path_matches_sensitive(
        Path::new("/home/user/.ssh/id_rsa"),
        ".ssh/id_*"
    ));
    assert!(path_matches_sensitive(
        Path::new("/home/user/.ssh/id_ecdsa"),
        ".ssh/id_*"
    ));
}

#[test]
fn test_component_matches_pattern() {
    // Exact match (no wildcard)
    assert!(component_matches_pattern(".env", ".env"));
    assert!(!component_matches_pattern(".env.local", ".env"));
    // Prefix wildcard
    assert!(component_matches_pattern("cert.pem", "*.pem"));
    assert!(component_matches_pattern("key.pem", "*.pem"));
    assert!(!component_matches_pattern("cert.txt", "*.pem"));
    // Suffix wildcard
    assert!(component_matches_pattern("id_rsa", "id_*"));
    assert!(component_matches_pattern("id_ed25519", "id_*"));
    assert!(!component_matches_pattern("identity", "id_*"));
    // Middle wildcard
    assert!(component_matches_pattern(".env.local", ".env.*"));
    assert!(component_matches_pattern(".env.production", ".env.*"));
    // Multiple wildcards: "id_*" matches "id_rsa" but not "other_id_rsa"
    assert!(component_matches_pattern("id_rsa", "id_*"));
    assert!(!component_matches_pattern("other_id_rsa", "id_*"));
}

#[test]
fn test_command_matches_allowlist_token_prefix() {
    let s = dev_sandbox();
    // Dev allowlist: "cargo test", "cargo clippy", "cargo build", "cargo fmt", "grep", "find", "ls", "ps"

    // Multi-word entries: "cargo test" allows "cargo test" + args, not "cargo run"
    assert!(s.check_command("cargo test").is_ok());
    assert!(s.check_command("cargo test -p lattice-core").is_ok());
    assert!(s.check_command("cargo build").is_ok());
    assert!(s.check_command("cargo build --release").is_ok());
    assert!(s.check_command("cargo clippy").is_ok());
    assert!(s.check_command("cargo fmt").is_ok());

    // These should be REJECTED — not matching any multi-word allowlist entry
    assert!(s.check_command("cargo run").is_err());
    assert!(s.check_command("cargo install").is_err());
    assert!(s.check_command("cargo publish").is_err());

    // Single-word entries: "grep" allows any grep command
    assert!(s.check_command("grep").is_ok());
    assert!(s.check_command("grep -rn 'TODO' src/").is_ok());
    assert!(s.check_command("ls -la").is_ok());
    assert!(s.check_command("find . -name '*.rs'").is_ok());
}

#[test]
fn test_allowlist_component_boundary_not_byte_prefix() {
    let mut s = default_sandbox();
    s.write_allowlist = vec!["src/".into()];
    // "src-hack/" should NOT match "src/" allowlist entry
    assert!(
        s.check_write("src-hack/foo.rs").is_err(),
        "src-hack should not match src/ allowlist (byte-prefix attack)"
    );
    // "src/main.rs" SHOULD match
    assert!(
        s.check_write("src/main.rs").is_ok(),
        "src/main.rs should match src/ allowlist"
    );
}

#[test]
fn test_is_path_prefix_component_boundary() {
    use std::path::Path;
    assert!(is_path_prefix(
        Path::new("/home/user/src/main.rs"),
        Path::new("/home/user/src")
    ));
    assert!(!is_path_prefix(
        Path::new("/home/user/src-hack/foo.rs"),
        Path::new("/home/user/src")
    ));
    assert!(!is_path_prefix(
        Path::new("/home/user/src"),
        Path::new("/home/user/src")
    ));
    assert!(is_path_prefix(Path::new("/a/b/c"), Path::new("/a/b")));
    assert!(!is_path_prefix(Path::new("/a/bc"), Path::new("/a/b")));
}

#[test]
fn test_sandbox_config_has_max_http_response_size() {
    let mut config = SandboxConfig::default();
    assert_eq!(config.max_http_response_size, 10 * 1024 * 1024);
    // Verify max_http_response_size and max_read_size are independent fields:
    // changing one must not affect the other.
    let original_read_size = config.max_read_size;
    config.max_http_response_size = 0;
    assert_eq!(
        config.max_read_size, original_read_size,
        "max_read_size should not change when max_http_response_size is modified"
    );
}

#[test]
fn test_default_sandbox_has_no_hook() {
    let s = SandboxConfig::default();
    assert!(s.hook.is_none(), "default sandbox should have no hook");
    // Backward compat: existing behavior unchanged
    assert!(s.check_command("ls; rm -rf /").is_err());
    assert!(s
        .check_command("ls; rm -rf /")
        .unwrap_err()
        .contains("metacharacter"));
}

// ============================================================================
// Plan 12-04: Integration tests for denial-approval flow
// ============================================================================

#[test]
fn test_end_to_end_denial_and_approval() {
    let approved_rules = Arc::new(Mutex::new(Vec::new()));
    let config = SandboxConfig {
        command_allowlist: vec!["cargo test".into()],
        approved_rules: Some(Arc::clone(&approved_rules)),
        ..SandboxConfig::default()
    };

    // Phase 1: deny "cargo publish" — not in static allowlist
    let result = config.check_command("cargo publish");
    assert!(result.is_err(), "cargo publish should be denied");

    // Parse the error as JSON — must be a DenialReport
    let err_str = result.unwrap_err();
    let v: serde_json::Value = serde_json::from_str(&err_str).expect("denial should be valid JSON");
    assert_eq!(v["__denial_report__"], true, "sentinel must be present");
    assert_eq!(v["risk_class"], "mutate", "risk_class should be mutate");
    assert_eq!(v["kind"]["type"], "command", "kind.type should be command");

    // Phase 2: approve the rule
    approved_rules.lock().unwrap().push(ApprovedRule {
        kind: ApprovalKind::CommandPrefix {
            prefix: "cargo publish".into(),
        },
        risk_class: RiskClass::Mutate,
    });

    // Phase 3: re-execute — should now pass via approved rule
    assert!(
        config.check_command("cargo publish --dry-run").is_ok(),
        "approved rule should allow cargo publish"
    );
}

#[test]
fn test_dangerous_blacklist_not_bypassable() {
    // Part 1: validate_command_rule rejects dangerous prefixes
    let val_result = validate_command_rule("rm -rf /tmp/build");
    assert!(
        matches!(val_result, Err(ValidationError::DangerousPattern { .. })),
        "validate_command_rule should reject rm -rf pattern: {:?}",
        val_result
    );

    // Part 2: even if validation is bypassed and the rule is inserted
    // directly, the defensive check in check_command_typed (D-19) must
    // still block dangerous commands.
    let approved_rules = Arc::new(Mutex::new(vec![ApprovedRule {
        kind: ApprovalKind::CommandPrefix {
            prefix: "rm -rf /tmp".into(),
        },
        risk_class: RiskClass::Destructive,
    }]));
    let config = SandboxConfig {
        command_allowlist: vec!["cargo test".into()],
        approved_rules: Some(Arc::clone(&approved_rules)),
        ..SandboxConfig::default()
    };

    let result = config.check_command("rm -rf /tmp/build");
    assert!(
        result.is_err(),
        "D-19: dangerous command must still be denied even with manually-inserted rule"
    );
}

#[test]
fn test_path_denial_and_approval() {
    let approved_rules = Arc::new(Mutex::new(Vec::new()));
    let config = SandboxConfig {
        read_allowlist: vec!["/home/user/project".into()],
        approved_rules: Some(Arc::clone(&approved_rules)),
        ..SandboxConfig::default()
    };

    // Phase 1: deny read outside allowlist
    let result = config.check_read("/etc/passwd");
    assert!(
        result.is_err(),
        "/etc/passwd should be denied (not in read allowlist)"
    );
    let err_str = result.unwrap_err();
    let v: serde_json::Value = serde_json::from_str(&err_str).expect("denial should be valid JSON");
    assert_eq!(v["kind"]["type"], "path", "kind.type should be path");
    assert_eq!(v["risk_class"], "read", "risk_class should be read");

    // Phase 2: approve path rule
    approved_rules.lock().unwrap().push(ApprovedRule {
        kind: ApprovalKind::PathRecursive {
            root: PathBuf::from("/etc/passwd"),
            operation: PathOperation::Read,
        },
        risk_class: RiskClass::Read,
    });

    // Phase 3: re-check — now allowed
    assert!(
        config.check_read("/etc/passwd").is_ok(),
        "approved path rule should allow read"
    );
}

#[test]
fn test_write_denial_produces_mutate_risk() {
    let config = SandboxConfig {
        write_allowlist: vec!["/home/user/project".into()],
        ..SandboxConfig::default()
    };

    let result = config.check_write("/tmp/output.txt");
    assert!(
        result.is_err(),
        "/tmp/output.txt should be denied (not in write allowlist)"
    );
    let err_str = result.unwrap_err();
    let v: serde_json::Value = serde_json::from_str(&err_str).expect("denial should be valid JSON");
    assert_eq!(
        v["risk_class"], "mutate",
        "write denial risk_class should be mutate"
    );
    assert_eq!(v["kind"]["type"], "path", "kind.type should be path");
}

#[test]
fn test_tool_denial_report_format() {
    let report = DenialReport::new(
        "tool 'shell' is not enabled".into(),
        RiskClass::Mutate,
        DenialKind::Tool {
            tool_name: "shell".into(),
            enabled_tools: vec!["read".into(), "grep".into(), "write_file".into()],
        },
    );

    let json = report.to_json();
    let v: serde_json::Value =
        serde_json::from_str(&json).expect("tool denial report should be valid JSON");

    assert_eq!(v["__denial_report__"], true);
    assert_eq!(v["kind"]["type"], "tool");
    assert_eq!(v["kind"]["tool_name"], "shell");
    assert!(
        v["kind"]["enabled_tools"].as_array().unwrap().len() == 3,
        "enabled_tools should have 3 entries"
    );
}

#[test]
fn test_malformed_approval_rejected() {
    // Empty command
    let r = validate_command_rule("");
    assert!(
        matches!(r, Err(ValidationError::TooFewTokens { got: 0, .. })),
        "empty command should be TooFewTokens with got=0: {:?}",
        r
    );

    // Single-token command
    let r = validate_command_rule("git");
    assert!(
        matches!(r, Err(ValidationError::TooFewTokens { got: 1, .. })),
        "single-token command should be TooFewTokens with got=1: {:?}",
        r
    );

    // Blocked top-level path
    let r = validate_path_rule("/");
    assert!(
        matches!(r, Err(ValidationError::BlockedTopLevel { .. })),
        "/ should be BlockedTopLevel: {:?}",
        r
    );

    // Path too shallow (single component)
    let r = validate_path_rule("usr");
    assert!(
        matches!(r, Err(ValidationError::PathTooShallow { got: 1, .. })),
        "single-component path should be PathTooShallow with got=1: {:?}",
        r
    );
}
