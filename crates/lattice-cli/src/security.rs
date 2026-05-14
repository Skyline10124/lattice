use std::path::{Path, PathBuf};

use anyhow::Result;
use lattice_agent::TaskState;
use lattice_runtime::RuntimeSecurityConfig;
use lattice_sandbox::{
    approval_kind_from_approve_command, ApprovedRule, RiskClass, ValidationError,
};

use crate::config::SecurityConfig;

pub(crate) type RuntimeSecurity = lattice_runtime::RuntimeSecurity;

pub(crate) fn build_runtime_security(
    config: &SecurityConfig,
    workdir: &Path,
) -> Result<RuntimeSecurity> {
    Ok(lattice_runtime::build_runtime_security(
        &runtime_security_config(config),
        workdir,
    )?)
}

pub(crate) fn default_runtime_security(workdir: &Path) -> Result<RuntimeSecurity> {
    build_runtime_security(&SecurityConfig::default(), workdir)
}

pub(crate) async fn reap_audit(security: &RuntimeSecurity) {
    security.reap_audit().await;
}

/// Approve a rule via the shared RuntimeSecurity instance.
/// This is the CLI entry point for /approve commands.
#[allow(dead_code)]
pub(crate) fn approve_rule(
    security: &RuntimeSecurity,
    rule: ApprovedRule,
) -> Result<(), ValidationError> {
    security.approve_rule(rule)
}

#[allow(dead_code)]
pub(crate) fn approve_proposal(
    security: &RuntimeSecurity,
    task_state: &mut TaskState,
    selector: &str,
) -> Result<String> {
    let selector = selector.trim();
    if selector != "last" && !is_approval_id(selector) {
        anyhow::bail!("unknown approval selector '{selector}'; use 'last' or an ap-N id");
    }

    let proposal = if selector == "last" {
        task_state.latest_pending_approval()
    } else {
        task_state.find_pending_approval(selector)
    }
    .cloned()
    .ok_or_else(|| anyhow::anyhow!("no pending approval proposal matches '{selector}'"))?;

    let risk_class = parse_risk_class(&proposal.risk_class)?;
    let kind = approval_kind_from_approve_command(&proposal.approve_command)
        .map_err(|err| anyhow::anyhow!("malformed approval command for {}: {err}", proposal.id))?;
    let rule = ApprovedRule { kind, risk_class };
    security
        .approve_rule(rule)
        .map_err(|err| anyhow::anyhow!("approval rejected for {}: {err}", proposal.id))?;

    if !task_state.mark_approval_resolved(&proposal.id) {
        anyhow::bail!("proposal {} was already resolved", proposal.id);
    }

    Ok(format!("Approved proposal {}", proposal.id))
}

#[allow(dead_code)]
fn is_approval_id(selector: &str) -> bool {
    selector
        .strip_prefix("ap-")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
}

#[allow(dead_code)]
fn parse_risk_class(value: &str) -> Result<RiskClass> {
    match value.trim() {
        "read" => Ok(RiskClass::Read),
        "mutate" => Ok(RiskClass::Mutate),
        "destructive" => Ok(RiskClass::Destructive),
        other => anyhow::bail!("unknown approval risk class '{other}'"),
    }
}

pub(crate) fn runtime_security_config(config: &SecurityConfig) -> RuntimeSecurityConfig {
    RuntimeSecurityConfig {
        sandbox_mode: config.sandbox_mode.clone(),
        read_allowlist: config.read_allowlist.clone(),
        write_allowlist: config.write_allowlist.clone(),
        command_allowlist: config.command_allowlist.clone(),
        hook_chain: config.hook_chain,
        landlock: config.landlock,
        audit: config.audit,
        audit_dir: config.audit_dir.as_deref().map(PathBuf::from),
        max_command_timeout: config.max_command_timeout,
        max_read_size: config.max_read_size,
        max_write_size: config.max_write_size,
        max_http_response_size: config.max_http_response_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_sandbox::ApprovalKind;

    #[test]
    fn project_mode_restricts_paths_to_workdir() {
        let workdir = std::env::current_dir().unwrap();
        let security = build_runtime_security(&SecurityConfig::default(), &workdir).unwrap();

        assert_eq!(security.mode_label, "project");
        assert_eq!(
            security.sandbox.read_allowlist,
            vec![std::fs::canonicalize(&workdir)
                .unwrap()
                .to_string_lossy()
                .to_string()]
        );
        assert!(security
            .sandbox
            .command_allowlist
            .contains(&"cargo test".to_string()));
        assert!(security.sandbox.hook.is_some());
    }

    #[test]
    fn custom_command_allowlist_overrides_project_defaults() {
        let workdir = std::env::current_dir().unwrap();
        let config = SecurityConfig {
            command_allowlist: vec!["just test".into()],
            ..SecurityConfig::default()
        };
        let security = build_runtime_security(&config, &workdir).unwrap();

        assert_eq!(security.sandbox.command_allowlist, vec!["just test"]);
    }

    #[test]
    fn permissive_mode_clears_command_allowlist() {
        let workdir = std::env::current_dir().unwrap();
        let config = SecurityConfig {
            sandbox_mode: "permissive".into(),
            ..SecurityConfig::default()
        };
        let security = build_runtime_security(&config, &workdir).unwrap();

        assert!(security.sandbox.command_allowlist.is_empty());
        assert_eq!(security.mode_label, "permissive");
    }

    #[test]
    fn invalid_mode_errors() {
        let workdir = std::env::current_dir().unwrap();
        let config = SecurityConfig {
            sandbox_mode: "unknown".into(),
            ..SecurityConfig::default()
        };

        let err = match build_runtime_security(&config, &workdir) {
            Ok(_) => panic!("invalid mode should fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unknown security.sandbox_mode"));
    }

    #[test]
    fn test_runtime_security_has_empty_approved_rules() {
        let workdir = std::env::current_dir().unwrap();
        let security = build_runtime_security(&SecurityConfig::default(), &workdir).unwrap();
        assert!(
            security.approved_rules.lock().unwrap().is_empty(),
            "new RuntimeSecurity should have empty approved_rules"
        );
    }

    #[test]
    fn test_approve_rule_via_cli_wrapper() {
        let workdir = std::env::current_dir().unwrap();
        let security = build_runtime_security(&SecurityConfig::default(), &workdir).unwrap();

        let rule = ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "cargo test".to_string(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        };

        approve_rule(&security, rule).expect("approve_rule should succeed for valid command");
        let rules = security.approved_rules.lock().unwrap();
        assert_eq!(
            rules.len(),
            1,
            "should have 1 approved rule after approve_rule"
        );
        assert_eq!(
            rules[0].kind,
            ApprovalKind::CommandPrefix {
                prefix: "cargo test".to_string(),
            },
            "approved rule kind should match"
        );
    }

    #[test]
    fn approve_proposal_resolves_last_and_marks_state() {
        let workdir = std::env::current_dir().unwrap();
        let security = build_runtime_security(&SecurityConfig::default(), &workdir).unwrap();
        let mut task_state = TaskState::new();
        task_state.record_approval_proposal(
            "Allow command".into(),
            "/approve command \"cargo publish --dry-run\"".into(),
            "mutate".into(),
        );

        let message = approve_proposal(&security, &mut task_state, "last").unwrap();

        assert_eq!(message, "Approved proposal ap-1");
        assert!(task_state.approval_proposals[0].resolved);
        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
    }

    #[test]
    fn approve_proposal_resolves_explicit_id() {
        let workdir = std::env::current_dir().unwrap();
        let security = build_runtime_security(&SecurityConfig::default(), &workdir).unwrap();
        let mut task_state = TaskState::new();
        task_state.record_approval_proposal(
            "Allow tool".into(),
            "/approve tool \"write_file\"".into(),
            "mutate".into(),
        );
        task_state.record_approval_proposal(
            "Allow path".into(),
            "/approve path \"/tmp/lattice-demo.txt\" read".into(),
            "read".into(),
        );

        approve_proposal(&security, &mut task_state, "ap-1").unwrap();

        assert!(task_state.approval_proposals[0].resolved);
        assert!(!task_state.approval_proposals[1].resolved);
        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
    }

    #[test]
    fn approve_proposal_rejects_unknown_or_resolved_selectors_without_mutation() {
        let workdir = std::env::current_dir().unwrap();
        let security = build_runtime_security(&SecurityConfig::default(), &workdir).unwrap();
        let mut task_state = TaskState::new();
        task_state.record_approval_proposal(
            "Allow tool".into(),
            "/approve tool \"write_file\"".into(),
            "mutate".into(),
        );

        assert!(approve_proposal(&security, &mut task_state, "ap-999").is_err());
        assert_eq!(security.approved_rules.lock().unwrap().len(), 0);
        assert!(!task_state.approval_proposals[0].resolved);

        approve_proposal(&security, &mut task_state, "ap-1").unwrap();
        assert!(approve_proposal(&security, &mut task_state, "ap-1").is_err());
        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
        assert!(approve_proposal(&security, &mut task_state, "wat").is_err());
    }

    #[test]
    fn approve_proposal_keeps_identical_grants_idempotent() {
        let workdir = std::env::current_dir().unwrap();
        let security = build_runtime_security(&SecurityConfig::default(), &workdir).unwrap();
        let mut task_state = TaskState::new();

        task_state.record_approval_proposal(
            "Allow path".into(),
            "/approve path \"/tmp/lattice-demo.txt\" read".into(),
            "read".into(),
        );
        approve_proposal(&security, &mut task_state, "last").unwrap();

        task_state.record_approval_proposal(
            "Allow path again".into(),
            "/approve path \"/tmp/lattice-demo.txt\" read".into(),
            "read".into(),
        );
        approve_proposal(&security, &mut task_state, "last").unwrap();

        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
        assert!(task_state
            .approval_proposals
            .iter()
            .all(|proposal| proposal.resolved));
    }

    #[test]
    fn approve_proposal_keeps_non_equivalent_pending_proposals_approvable() {
        let workdir = std::env::current_dir().unwrap();
        let security = build_runtime_security(&SecurityConfig::default(), &workdir).unwrap();
        let mut task_state = TaskState::new();

        task_state.record_approval_proposal(
            "Allow path read".into(),
            "/approve path \"/tmp/lattice-demo.txt\" read".into(),
            "read".into(),
        );
        task_state.record_approval_proposal(
            "Allow path write".into(),
            "/approve path \"/tmp/lattice-demo.txt\" write".into(),
            "mutate".into(),
        );

        assert!(task_state.find_pending_approval("ap-1").is_some());
        assert!(task_state.find_pending_approval("ap-2").is_some());

        approve_proposal(&security, &mut task_state, "ap-1").unwrap();
        assert!(task_state.approval_proposals[0].resolved);
        assert!(!task_state.approval_proposals[1].resolved);

        approve_proposal(&security, &mut task_state, "ap-2").unwrap();
        assert!(task_state
            .approval_proposals
            .iter()
            .all(|proposal| proposal.resolved));
        assert_eq!(security.approved_rules.lock().unwrap().len(), 2);
    }
}
