use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::approval::ApprovalKind;
use crate::denial::{DenialKind, DenialReport};
use crate::risk::{PathOperation, RiskClass};

/// The kind of operation proposed for approval.
///
/// More constrained than [`ApprovalKind`](crate::approval::ApprovalKind): only the shapes
/// the model can actually propose (exact command, exact path, or tool). No
/// bundles and no recursive/prefix variants — those require explicit user
/// intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalProposalKind {
    /// Propose allowing an exact shell command.
    CommandExact {
        /// The exact command to propose.
        command: String,
    },
    /// Propose allowing filesystem access to an exact path.
    PathExact {
        /// The exact path being proposed.
        path: PathBuf,
        /// The operation proposed (read or write).
        operation: PathOperation,
    },
    /// Propose allowing a specific named tool.
    Tool {
        /// The name of the tool proposed.
        tool_name: String,
    },
}

/// A structured approval proposal emitted by the agent loop when a denial is
/// detected (per D-04). Carries enough information for a CLI or TUI to render
/// a prompt and generate a typed `/approve` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalProposal {
    /// What is being proposed.
    pub kind: ApprovalProposalKind,
    /// Risk classification of the proposed operation.
    pub risk_class: RiskClass,
    /// Human-readable label for display in the approval prompt.
    pub display: String,
    /// The `/approve ...` command string the user can run to approve.
    pub approve_command: String,
}

// ---------------------------------------------------------------------------
// Approval proposal generation from denial reports
// ---------------------------------------------------------------------------

/// Generate a typed approval proposal from a denial report.
///
/// Policy-owned: encodes the mapping from denial context to approvable shapes.
/// Only produces proposals for narrow, auto-derivable shapes:
///
/// | [`DenialKind`] | [`ApprovalProposalKind`] |
/// |---|---|
/// | `Command` | `CommandExact` (exact command match) |
/// | `Tool` | `Tool` |
/// | `Path` (op="read" or "write") | `PathExact` |
///
/// Returns `None` when no proposal can be generated (e.g. a path denial
/// with an unrecognized operation). Never generates `CommandPrefix`,
/// `PathRecursive`, or `Bundle` — those require explicit user intent.
pub fn approval_proposal_for_denial(report: &DenialReport) -> Option<ApprovalProposal> {
    let (kind, subject, rule_type, extra_arg): (
        ApprovalProposalKind,
        String,
        &str,
        Option<String>,
    ) = match &report.kind {
        DenialKind::Command { command, .. } => {
            let cmd = command.trim().to_string();
            (
                ApprovalProposalKind::CommandExact {
                    command: cmd.clone(),
                },
                cmd,
                "command",
                None,
            )
        }
        DenialKind::Tool { tool_name, .. } => {
            let name = tool_name.clone();
            (
                ApprovalProposalKind::Tool {
                    tool_name: name.clone(),
                },
                name,
                "tool",
                None,
            )
        }
        DenialKind::Path {
            path, operation, ..
        } => {
            let op = parse_path_operation(operation)?;
            let op_arg = path_operation_arg(op).to_string();
            (
                ApprovalProposalKind::PathExact {
                    path: PathBuf::from(path),
                    operation: op,
                },
                path.clone(),
                "path",
                Some(op_arg),
            )
        }
    };

    let quoted = quote_approval_arg(&subject);
    let approve_command = if let Some(extra) = &extra_arg {
        format!("/approve {} {} {}", rule_type, quoted, extra)
    } else {
        format!("/approve {} {}", rule_type, quoted)
    };

    let display = match &kind {
        ApprovalProposalKind::CommandExact { command } => {
            format!("Allow command: {}?", command)
        }
        ApprovalProposalKind::Tool { tool_name } => {
            format!("Allow tool: {}?", tool_name)
        }
        ApprovalProposalKind::PathExact { path, operation } => {
            format!(
                "Allow {} of {}?",
                path_operation_arg(*operation),
                path.display()
            )
        }
    };

    Some(ApprovalProposal {
        kind,
        risk_class: report.risk_class,
        display,
        approve_command,
    })
}

/// Generate the JSON marker object (`__approval_proposal__`) that gets
/// injected into enriched denial results for CLI/TUI consumption.
///
/// Produces the shape expected by existing tests and approval UIs:
///
/// ```json
/// {
///   "command": "<subject string (legacy field name)>",
///   "risk_class": "<snake_case>",
///   "approve_command": "/approve ..."
/// }
/// ```
///
/// Returns `None` when [`approval_proposal_for_denial`] would also return
/// `None`.
pub fn approval_proposal_marker_for_denial(report: &DenialReport) -> Option<serde_json::Value> {
    approval_proposal_for_denial(report).map(|proposal| {
        let subject = match &report.kind {
            DenialKind::Command { command, .. } => command.clone(),
            DenialKind::Tool { tool_name, .. } => tool_name.clone(),
            DenialKind::Path { path, .. } => path.clone(),
        };
        serde_json::json!({
            "command": subject,
            "risk_class": proposal.risk_class,
            "approve_command": proposal.approve_command,
        })
    })
}

/// Quote a value for use in an `/approve` command argument.
///
/// Uses JSON string quoting (with proper escape handling) for safe
/// embedding in shell contexts.
pub fn quote_approval_arg(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{}\"", value.replace('"', "\\\"")))
}

/// Parse a stored `/approve ...` command into the narrow approval kind it grants.
pub fn approval_kind_from_approve_command(command: &str) -> Result<ApprovalKind, String> {
    let rest = command
        .trim()
        .strip_prefix("/approve")
        .ok_or_else(|| "approval command must start with /approve".to_string())?
        .trim_start();
    let (rule_type, rest) = take_word(rest)?;
    match rule_type {
        "command" => {
            let (command, rest) = take_approval_arg(rest)?;
            ensure_no_extra_args(rest)?;
            Ok(ApprovalKind::CommandExact { command })
        }
        "tool" => {
            let (tool_name, rest) = take_approval_arg(rest)?;
            ensure_no_extra_args(rest)?;
            Ok(ApprovalKind::Tool { tool_name })
        }
        "path" => {
            let (path, rest) = take_approval_arg(rest)?;
            let (operation, rest) = take_word(rest)?;
            ensure_no_extra_args(rest)?;
            let operation = parse_path_operation(operation)
                .ok_or_else(|| format!("unsupported path approval operation: {operation}"))?;
            Ok(ApprovalKind::PathExact {
                path: PathBuf::from(path),
                operation,
            })
        }
        other => Err(format!("unsupported approval kind: {other}")),
    }
}

fn take_word(input: &str) -> Result<(&str, &str), String> {
    let input = input.trim_start();
    if input.is_empty() {
        return Err("missing approval argument".to_string());
    }
    match input.find(char::is_whitespace) {
        Some(idx) => Ok((&input[..idx], &input[idx..])),
        None => Ok((input, "")),
    }
}

fn take_approval_arg(input: &str) -> Result<(String, &str), String> {
    let input = input.trim_start();
    if input.is_empty() {
        return Err("missing approval subject".to_string());
    }
    if input.starts_with('"') {
        let mut escaped = false;
        for (idx, ch) in input.char_indices().skip(1) {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => {
                    let end = idx + ch.len_utf8();
                    let value = serde_json::from_str::<String>(&input[..end])
                        .map_err(|err| format!("malformed quoted approval subject: {err}"))?;
                    return Ok((value, &input[end..]));
                }
                _ => {}
            }
        }
        return Err("unterminated quoted approval subject".to_string());
    }
    match input.find(char::is_whitespace) {
        Some(idx) => Ok((input[..idx].to_string(), &input[idx..])),
        None => Ok((input.to_string(), "")),
    }
}

fn ensure_no_extra_args(rest: &str) -> Result<(), String> {
    if rest.trim().is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unexpected extra approval arguments: {}",
            rest.trim()
        ))
    }
}

/// Parse a string into a [`PathOperation`], returning `None` for
/// unrecognized operation names (e.g. "delete", "execute").
fn parse_path_operation(op: &str) -> Option<PathOperation> {
    let normalized = op.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "read" => Some(PathOperation::Read),
        "write" => Some(PathOperation::Write),
        _ => None,
    }
}

fn path_operation_arg(operation: PathOperation) -> &'static str {
    match operation {
        PathOperation::Read => "read",
        PathOperation::Write => "write",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::denial::{DenialKind, DenialReport};
    use crate::risk::{PathOperation, RiskClass};
    use std::path::PathBuf;

    // --- ApprovalProposal / ApprovalProposalKind ---

    #[test]
    fn test_approval_proposal_kind_serialization() {
        let kind = ApprovalProposalKind::CommandExact {
            command: "cargo publish --dry-run".to_string(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(
            json.contains("\"type\":\"command_exact\""),
            "CommandExact should serialize with type tag: {}",
            json
        );
        let deserialized: ApprovalProposalKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, kind);
    }

    #[test]
    fn test_approval_proposal_serialization() {
        let proposal = ApprovalProposal {
            kind: ApprovalProposalKind::Tool {
                tool_name: "write_file".to_string(),
            },
            risk_class: RiskClass::Mutate,
            display: "Allow tool write_file?".to_string(),
            approve_command: "/approve tool write_file".to_string(),
        };
        let json = serde_json::to_string(&proposal).unwrap();
        let deserialized: ApprovalProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, proposal);
    }

    // --- Helpers for building denial reports in tests ---

    fn make_command_denial(command: &str) -> DenialReport {
        DenialReport::new(
            format!("command '{command}' is not in the allowlist"),
            RiskClass::Mutate,
            DenialKind::Command {
                command: command.to_string(),
                allowed_prefixes: vec!["cargo test".into(), "cargo build".into()],
            },
        )
    }

    fn make_tool_denial(tool_name: &str) -> DenialReport {
        DenialReport::new(
            format!("tool '{tool_name}' is not enabled"),
            RiskClass::Mutate,
            DenialKind::Tool {
                tool_name: tool_name.to_string(),
                enabled_tools: vec!["read".into(), "grep".into()],
            },
        )
    }

    fn make_path_denial(path: &str, operation: &str) -> DenialReport {
        DenialReport::new(
            format!("path '{path}' is outside the allowlist"),
            RiskClass::Read,
            DenialKind::Path {
                path: path.to_string(),
                operation: operation.to_string(),
                allowlist: vec!["/home/user".into()],
            },
        )
    }

    // --- Approval proposal generation from denial reports ---

    #[test]
    fn test_proposal_command_exact() {
        let report = make_command_denial("cargo publish --dry-run");
        let proposal = approval_proposal_for_denial(&report).expect("should produce a proposal");

        assert_eq!(
            proposal.kind,
            ApprovalProposalKind::CommandExact {
                command: "cargo publish --dry-run".into()
            }
        );
        assert_eq!(proposal.risk_class, RiskClass::Mutate);
        assert_eq!(
            proposal.approve_command,
            "/approve command \"cargo publish --dry-run\""
        );
        assert!(
            proposal.display.contains("cargo publish --dry-run"),
            "display should mention the command: {}",
            proposal.display
        );
    }

    #[test]
    fn test_proposal_command_trims_whitespace() {
        let report = make_command_denial("  cargo build --release  ");
        let proposal = approval_proposal_for_denial(&report).expect("should produce a proposal");

        if let ApprovalProposalKind::CommandExact { command } = &proposal.kind {
            assert_eq!(command, "cargo build --release");
        } else {
            panic!("expected CommandExact");
        }
        assert!(
            proposal
                .approve_command
                .contains("\"cargo build --release\""),
            "approve_command should use trimmed command: {}",
            proposal.approve_command
        );
    }

    #[test]
    fn test_proposal_tool() {
        let report = make_tool_denial("shell");
        let proposal = approval_proposal_for_denial(&report).expect("should produce a proposal");

        assert_eq!(
            proposal.kind,
            ApprovalProposalKind::Tool {
                tool_name: "shell".into()
            }
        );
        assert_eq!(proposal.risk_class, RiskClass::Mutate);
        assert_eq!(proposal.approve_command, "/approve tool \"shell\"");
        assert!(
            proposal.display.contains("shell"),
            "display should mention tool: {}",
            proposal.display
        );
    }

    #[test]
    fn test_proposal_path_read() {
        let report = make_path_denial("/etc/shadow", "read");
        let proposal = approval_proposal_for_denial(&report).expect("should produce a proposal");

        assert_eq!(
            proposal.kind,
            ApprovalProposalKind::PathExact {
                path: PathBuf::from("/etc/shadow"),
                operation: PathOperation::Read,
            }
        );
        assert_eq!(proposal.risk_class, RiskClass::Read);
        assert_eq!(
            proposal.approve_command,
            "/approve path \"/etc/shadow\" read"
        );
    }

    #[test]
    fn test_proposal_path_write() {
        let report = make_path_denial("/home/user/output.txt", "write");
        let proposal = approval_proposal_for_denial(&report).expect("should produce a proposal");

        assert_eq!(
            proposal.kind,
            ApprovalProposalKind::PathExact {
                path: PathBuf::from("/home/user/output.txt"),
                operation: PathOperation::Write,
            }
        );
        assert_eq!(proposal.risk_class, RiskClass::Read);
        assert_eq!(
            proposal.approve_command,
            "/approve path \"/home/user/output.txt\" write"
        );
    }

    #[test]
    fn test_proposal_path_operation_is_normalized() {
        let report = make_path_denial("/home/user/output.txt", " Write ");
        let proposal = approval_proposal_for_denial(&report).expect("should produce a proposal");

        assert_eq!(
            proposal.kind,
            ApprovalProposalKind::PathExact {
                path: PathBuf::from("/home/user/output.txt"),
                operation: PathOperation::Write,
            }
        );
        assert_eq!(
            proposal.approve_command,
            "/approve path \"/home/user/output.txt\" write"
        );
    }

    #[test]
    fn test_proposal_path_invalid_operation_returns_none() {
        let report = make_path_denial("/etc/shadow", "delete");
        assert!(
            approval_proposal_for_denial(&report).is_none(),
            "unrecognized path operation should return None"
        );
        assert!(
            approval_proposal_marker_for_denial(&report).is_none(),
            "marker should also return None for unrecognized operation"
        );
    }

    #[test]
    fn test_proposal_path_unknown_operation() {
        // "execute" is not a recognized PathOperation
        let report = make_path_denial("/bin/some-tool", "execute");
        assert!(approval_proposal_for_denial(&report).is_none());
    }

    #[test]
    fn test_proposal_never_produces_prefix_or_recursive() {
        let report = make_command_denial("cargo test --all");
        let proposal = approval_proposal_for_denial(&report).unwrap();
        // Must be CommandExact, not CommandPrefix
        assert!(matches!(
            proposal.kind,
            ApprovalProposalKind::CommandExact { .. }
        ));

        let report = make_path_denial("/home/user/project", "read");
        let proposal = approval_proposal_for_denial(&report).unwrap();
        // Must be PathExact, not PathRecursive
        assert!(matches!(
            proposal.kind,
            ApprovalProposalKind::PathExact { .. }
        ));
    }

    // --- Quote approval arg ---

    #[test]
    fn test_quote_approval_arg_embeds_quotes() {
        let quoted = quote_approval_arg("cargo publish --dry-run");
        assert_eq!(quoted, "\"cargo publish --dry-run\"");
    }

    #[test]
    fn test_quote_approval_arg_escapes_internal_quotes() {
        let quoted = quote_approval_arg("echo \"hello world\"");
        // serde_json::to_string escapes internal double quotes with backslash
        assert_eq!(quoted, "\"echo \\\"hello world\\\"\"");
    }

    #[test]
    fn test_quote_approval_arg_single_word() {
        let quoted = quote_approval_arg("shell");
        assert_eq!(quoted, "\"shell\"");
    }

    #[test]
    fn test_quote_in_approve_command() {
        // Verify the full approve_command pipeline handles quotes correctly
        let report = make_command_denial("echo \"hello\"");
        let proposal = approval_proposal_for_denial(&report).unwrap();
        // The approve_command should contain escaped quotes
        assert_eq!(
            proposal.approve_command,
            "/approve command \"echo \\\"hello\\\"\""
        );
    }

    #[test]
    fn parse_stored_approve_command_command_tool_and_path() {
        assert_eq!(
            approval_kind_from_approve_command("/approve command \"cargo publish --dry-run\"")
                .unwrap(),
            ApprovalKind::CommandExact {
                command: "cargo publish --dry-run".to_string(),
            }
        );
        assert_eq!(
            approval_kind_from_approve_command("/approve tool \"write_file\"").unwrap(),
            ApprovalKind::Tool {
                tool_name: "write_file".to_string(),
            }
        );
        assert_eq!(
            approval_kind_from_approve_command("/approve path \"/tmp/demo.txt\" write").unwrap(),
            ApprovalKind::PathExact {
                path: PathBuf::from("/tmp/demo.txt"),
                operation: PathOperation::Write,
            }
        );
    }

    #[test]
    fn parse_stored_approve_command_rejects_malformed_shapes() {
        assert!(approval_kind_from_approve_command("approve command \"x\"").is_err());
        assert!(approval_kind_from_approve_command("/approve bundle \"code-agent\"").is_err());
        assert!(
            approval_kind_from_approve_command("/approve path \"/tmp/demo.txt\" delete").is_err()
        );
        assert!(approval_kind_from_approve_command("/approve tool \"bash\" extra").is_err());
    }

    // --- Marker JSON shape ---

    #[test]
    fn test_marker_json_shape_command() {
        let report = make_command_denial("cargo publish --dry-run");
        let marker = approval_proposal_marker_for_denial(&report).expect("should produce a marker");

        assert_eq!(marker["command"], "cargo publish --dry-run");
        assert_eq!(marker["risk_class"], "mutate");
        assert_eq!(
            marker["approve_command"],
            "/approve command \"cargo publish --dry-run\""
        );
        // Verify no extra fields
        let obj = marker.as_object().unwrap();
        assert_eq!(obj.len(), 3);
    }

    #[test]
    fn test_marker_json_shape_tool() {
        let report = make_tool_denial("write_file");
        let marker = approval_proposal_marker_for_denial(&report).expect("should produce a marker");

        assert_eq!(marker["command"], "write_file");
        assert_eq!(marker["risk_class"], "mutate");
        assert_eq!(marker["approve_command"], "/approve tool \"write_file\"");
    }

    #[test]
    fn test_marker_json_shape_path() {
        let report = make_path_denial("/etc/hosts", "write");
        let marker = approval_proposal_marker_for_denial(&report).expect("should produce a marker");

        assert_eq!(marker["command"], "/etc/hosts");
        assert_eq!(marker["risk_class"], "read");
        assert_eq!(
            marker["approve_command"],
            "/approve path \"/etc/hosts\" write"
        );
    }

    #[test]
    fn test_marker_no_passthrough_for_invalid_path_op() {
        let report = make_path_denial("/tmp/foo", "delete");
        assert!(approval_proposal_marker_for_denial(&report).is_none());
    }
}
