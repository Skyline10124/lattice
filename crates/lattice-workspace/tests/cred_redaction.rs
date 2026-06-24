// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Debug redaction tests for `CredentialSpec`.
//!
//! These tests verify that the manual `Debug` implementation on `CredentialSpec`
//! does not expose secret values.  Pattern matches `crates/lattice-profile/
//! src/schema.rs` lines 1488-1514 (absent-string assertion pattern).
//!
//! Validation rows: (`env_var_debug_does_not_contain_value`),
//! (`file_cred_debug_does_not_contain_content`).

use lattice_workspace::{CredentialSpec, EgressPolicy, ImageOrTemplate, UpperSpec};

#[test]
fn env_var_debug_does_not_contain_value() {
    let cred = CredentialSpec::EnvVar {
        name: "OPENAI_KEY".into(),
        value: "sk-super-secret-token-xyz-9999".into(),
    };
    let debug_str = format!("{:?}", cred);
    assert!(
        !debug_str.contains("sk-super-secret-token-xyz-9999"),
        "CredentialSpec::EnvVar Debug must not expose the value; got: {debug_str}"
    );
    assert!(
        debug_str.contains("[REDACTED]"),
        "CredentialSpec::EnvVar Debug must contain [REDACTED]; got: {debug_str}"
    );
    assert!(
        debug_str.contains("OPENAI_KEY"),
        "CredentialSpec::EnvVar Debug must show the key name; got: {debug_str}"
    );
}

#[test]
fn file_cred_debug_does_not_contain_content() {
    let cred = CredentialSpec::File {
        content: b"-----BEGIN RSA PRIVATE KEY-----\nMIIE...".to_vec(),
        guest_path: "/run/secrets/signing-key".into(),
    };
    let debug_str = format!("{:?}", cred);
    assert!(
        !debug_str.contains("BEGIN RSA PRIVATE KEY"),
        "CredentialSpec::File Debug must not expose content; got: {debug_str}"
    );
    assert!(
        debug_str.contains("[REDACTED]"),
        "CredentialSpec::File Debug must contain [REDACTED]; got: {debug_str}"
    );
    assert!(
        debug_str.contains("/run/secrets/signing-key"),
        "CredentialSpec::File Debug must show guest_path; got: {debug_str}"
    );
}

#[test]
fn upper_spec_debug_via_credential_list() {
    // UpperSpec contains Vec<CredentialSpec>; its Debug must also redact.
    let spec = UpperSpec {
        image_or_template: ImageOrTemplate::Image(
            "docker.m.daocloud.io/library/alpine:3.19".into(),
        ),
        baseline_oid: "a".repeat(40),
        host_repo_path: std::path::PathBuf::from("."),
        egress: EgressPolicy::Blocked,
        credentials: vec![CredentialSpec::EnvVar {
            name: "SECRET".into(),
            value: "top-secret-value-do-not-log".into(),
        }],
        timeout_secs: 30,
        cpus: 1,
        memory_mb: 256,
    };
    let debug_str = format!("{:?}", spec);
    assert!(
        !debug_str.contains("top-secret-value-do-not-log"),
        "UpperSpec Debug must not expose credential values; got: {debug_str}"
    );
    assert!(
        debug_str.contains("[REDACTED]"),
        "UpperSpec Debug via CredentialSpec must contain [REDACTED]; got: {debug_str}"
    );
}

/// Composite guard test — matches the `test_cred_debug_redaction` CI filter.
///
/// Asserts that BOTH `EnvVar` and `File` credential variants redact their
/// secret values in `Debug` output.
#[test]
fn test_cred_debug_redaction() {
    // EnvVar: value must be redacted.
    let env_cred = CredentialSpec::EnvVar {
        name: "CI_SECRET_KEY".into(),
        value: "sk-super-secret-token-xyz-9999".into(),
    };
    let env_debug = format!("{env_cred:?}");
    assert!(
        !env_debug.contains("sk-super-secret-token-xyz-9999"),
        "EnvVar secret must be redacted; got: {env_debug}"
    );
    assert!(
        env_debug.contains("[REDACTED]"),
        "EnvVar debug must contain [REDACTED]; got: {env_debug}"
    );

    // File: content must be redacted.
    let file_cred = CredentialSpec::File {
        content: b"-----BEGIN RSA PRIVATE KEY-----\nMIIE...".to_vec(),
        guest_path: "/run/secrets/key.pem".into(),
    };
    let file_debug = format!("{file_cred:?}");
    assert!(
        !file_debug.contains("BEGIN RSA PRIVATE KEY"),
        "File content must be redacted; got: {file_debug}"
    );
    assert!(
        file_debug.contains("[REDACTED]"),
        "File debug must contain [REDACTED]; got: {file_debug}"
    );
}
