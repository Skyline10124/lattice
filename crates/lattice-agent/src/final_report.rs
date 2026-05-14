use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use crate::state::{TaskState, VerificationStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalReport {
    pub changed_files: Vec<FinalChangedFile>,
    pub verification: Vec<FinalVerification>,
    pub residual_risk: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalChangedFile {
    pub path: String,
    pub operation: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalVerification {
    pub command: String,
    pub status: VerificationStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusSnapshot {
    pub changed_paths: Vec<String>,
}

impl FinalReport {
    pub fn from_task_state(state: &TaskState, git: Option<&GitStatusSnapshot>) -> Self {
        let mut changed_files = Vec::new();
        let mut seen_changed = BTreeSet::new();
        for changed in &state.changed_files {
            if seen_changed.insert(changed.path.clone()) {
                changed_files.push(FinalChangedFile {
                    path: changed.path.clone(),
                    operation: changed.operation.clone(),
                    source: "task_state".into(),
                });
            }
        }

        let mut verification: Vec<FinalVerification> = state
            .verifications
            .iter()
            .map(|record| FinalVerification {
                command: record.command.clone(),
                status: record.status.clone(),
                detail: record.output_trimmed.clone(),
            })
            .collect();
        if !changed_files.is_empty() && verification.is_empty() {
            verification.push(FinalVerification {
                command: "verification".into(),
                status: VerificationStatus::NotRun,
                detail: "No verification command recorded after changes.".into(),
            });
        }

        let failed_commands: Vec<String> = state
            .verifications
            .iter()
            .filter(|record| record.status == VerificationStatus::Failed)
            .map(|record| record.command.clone())
            .collect();
        let mut residual_risk = Vec::new();
        for item in &state.unverified_work {
            residual_risk.push(format!("Unverified work remains: {item}"));
        }
        for command in &failed_commands {
            residual_risk.push(format!("Verification failed: {command}"));
        }
        for proposal in state
            .approval_proposals
            .iter()
            .filter(|proposal| !proposal.resolved)
        {
            residual_risk.push(format!(
                "Pending approval remains: {} {}",
                proposal.id, proposal.display
            ));
        }
        for failure in &state.tool_failures {
            if is_resolved_approval_failure(failure) {
                continue;
            }
            let represented = failed_commands
                .iter()
                .any(|command| failure.message.contains(command));
            if !represented {
                residual_risk.push(format!(
                    "Tool failure remains: {} {}",
                    failure.tool_name, failure.message
                ));
            }
        }
        if let Some(git) = git {
            for path in &git.changed_paths {
                if !seen_changed.contains(path) {
                    residual_risk.push(format!(
                        "Untracked or unrelated dirty path not in task state: {path}"
                    ));
                }
            }
        }

        Self {
            changed_files,
            verification,
            residual_risk,
        }
    }

    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("## Changed Files\n");
        if self.changed_files.is_empty() {
            out.push_str("- None\n");
        } else {
            for changed in &self.changed_files {
                out.push_str(&format!(
                    "- {} [{}] (source: {})\n",
                    changed.path, changed.operation, changed.source
                ));
            }
        }

        out.push_str("\n## Verification\n");
        if self.verification.is_empty() {
            out.push_str("- None\n");
        } else {
            for verification in &self.verification {
                out.push_str(&format!(
                    "- [{}] `{}` - {}\n",
                    verification_status_label(&verification.status),
                    verification.command,
                    verification.detail
                ));
            }
        }

        out.push_str("\n## Residual Risk\n");
        if self.residual_risk.is_empty() {
            out.push_str("- None\n");
        } else {
            for risk in &self.residual_risk {
                out.push_str(&format!("- {risk}\n"));
            }
        }
        out
    }
}

fn is_resolved_approval_failure(failure: &crate::state::ToolFailureRecord) -> bool {
    failure.resolved && failure.approval_proposal_id.is_some()
}

pub fn git_status_snapshot(root: &Path) -> Option<GitStatusSnapshot> {
    let output = Command::new("git")
        .args(["status", "--short"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let changed_paths = stdout
        .lines()
        .filter_map(parse_git_status_path)
        .take(50)
        .collect();
    Some(GitStatusSnapshot { changed_paths })
}

fn parse_git_status_path(line: &str) -> Option<String> {
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    let path = path
        .split(" -> ")
        .last()
        .unwrap_or(path)
        .trim()
        .trim_matches('"');
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn verification_status_label(status: &VerificationStatus) -> &'static str {
    match status {
        VerificationStatus::Passed => "passed",
        VerificationStatus::Failed => "failed",
        VerificationStatus::NotRun => "not_run",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_report_renders_required_sections_in_order() {
        let report = FinalReport::from_task_state(&TaskState::new(), None);
        let markdown = report.render_markdown();

        let changed = markdown.find("## Changed Files").unwrap();
        let verification = markdown.find("## Verification").unwrap();
        let risk = markdown.find("## Residual Risk").unwrap();

        assert!(changed < verification);
        assert!(verification < risk);
    }

    #[test]
    fn final_report_changed_file_without_verification_renders_not_run() {
        let mut state = TaskState::new();
        state.record_changed_file("src/lib.rs".into(), "update".into(), None);

        let report = FinalReport::from_task_state(&state, None);
        let markdown = report.render_markdown();

        assert!(markdown.contains("not_run"));
        assert!(markdown.contains("No verification command recorded after changes."));
        assert!(!markdown.contains("[passed]"));
    }

    #[test]
    fn final_report_failed_verification_adds_residual_risk() {
        let mut state = TaskState::new();
        state.record_verification_failure("cargo test".into(), "FAILED".into());

        let report = FinalReport::from_task_state(&state, None);
        let markdown = report.render_markdown();

        assert!(markdown.contains("[failed] `cargo test`"));
        assert!(markdown.contains("Verification failed: cargo test"));
    }

    #[test]
    fn final_report_unrelated_git_dirty_path_is_only_residual_risk() {
        let mut state = TaskState::new();
        state.record_changed_file("src/lib.rs".into(), "update".into(), None);
        let git = GitStatusSnapshot {
            changed_paths: vec!["src/lib.rs".into(), "notes.txt".into()],
        };

        let report = FinalReport::from_task_state(&state, Some(&git));
        let markdown = report.render_markdown();

        assert_eq!(report.changed_files.len(), 1);
        assert_eq!(report.changed_files[0].path, "src/lib.rs");
        assert!(markdown.contains("Untracked or unrelated dirty path not in task state: notes.txt"));
        assert!(!report
            .changed_files
            .iter()
            .any(|changed| changed.path == "notes.txt"));
    }

    #[test]
    fn final_report_empty_sections_render_none() {
        let report = FinalReport::from_task_state(&TaskState::new(), None);
        let markdown = report.render_markdown();

        assert_eq!(markdown.matches("- None").count(), 3);
    }

    #[test]
    fn final_report_pending_approval_is_residual_risk() {
        let mut state = TaskState::new();
        state.record_approval_proposal(
            "Allow tool".into(),
            "/approve tool \"write_file\"".into(),
            "mutate".into(),
        );

        let report = FinalReport::from_task_state(&state, None);

        assert!(report
            .residual_risk
            .iter()
            .any(|risk| risk == "Pending approval remains: ap-1 Allow tool"));
    }

    #[test]
    fn final_report_skips_resolved_approval_blocker_failure() {
        let mut state = TaskState::new();
        let proposal_id = state.record_approval_proposal(
            "Allow command".into(),
            "/approve command \"cargo publish\"".into(),
            "mutate".into(),
        );
        state.record_approval_blocker_failure(
            "denied".into(),
            "denied".into(),
            "blocked pending approval ap-1".into(),
            proposal_id.clone(),
        );

        let unresolved = FinalReport::from_task_state(&state, None);
        assert!(unresolved
            .residual_risk
            .iter()
            .any(|risk| risk == "Pending approval remains: ap-1 Allow command"));
        assert!(unresolved.residual_risk.iter().any(|risk| {
            risk.contains("Tool failure remains: denied blocked pending approval ap-1")
        }));

        assert!(state.mark_approval_resolved(&proposal_id));

        let resolved = FinalReport::from_task_state(&state, None);
        assert!(!resolved
            .residual_risk
            .iter()
            .any(|risk| risk.contains("ap-1")));
        assert!(!resolved
            .residual_risk
            .iter()
            .any(|risk| risk.contains("Tool failure remains")));
    }

    #[test]
    fn final_report_parse_git_status_path_handles_short_status() {
        assert_eq!(
            parse_git_status_path(" M src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            parse_git_status_path("R  old.rs -> src/new.rs").as_deref(),
            Some("src/new.rs")
        );
    }
}
