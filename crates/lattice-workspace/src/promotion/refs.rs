// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Atomic three-ref promotion transaction via `git update-ref --stdin -z`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use regex::Regex;
use thiserror::Error;

/// Strict ASCII identifier for session and turn path components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefComponent(String);

/// A Git ref name validated with `git check-ref-format`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRefName(String);

/// The three refs updated together during promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionRefs {
    pub project_ref: ValidatedRefName,
    pub session_ref: ValidatedRefName,
    pub candidate_ref: ValidatedRefName,
}

/// Inputs for one atomic three-ref transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTransactionRequest {
    pub refs: PromotionRefs,
    pub old_project_oid: String,
    pub old_session_oid: Option<String>,
    pub new_candidate_oid: String,
    pub new_session_oid: String,
    /// If Some, the candidate ref already exists with this OID and the
    /// transaction verifies it (update old=oid new=oid) instead of creating.
    pub existing_candidate_oid: Option<String>,
}

/// Observed ref targets after a successful transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTransactionOutcome {
    pub project_oid: String,
    pub session_oid: String,
    pub candidate_oid: String,
}

/// Errors specific to ref validation and transaction execution.
#[derive(Debug, Error)]
pub enum RefTransactionError {
    #[error("invalid ref component `{field}`: {reason}")]
    InvalidComponent { field: &'static str, reason: String },
    #[error("invalid ref `{refname}`: {reason}")]
    InvalidRefName { refname: String, reason: String },
    #[error("invalid oid `{field}`: {reason}")]
    InvalidOid { field: &'static str, reason: String },
    #[error("protocol field contains NUL byte")]
    NulInProtocolField,
    #[error("git update-ref failed: {context}: {stderr}")]
    GitUpdateRef { context: String, stderr: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl RefComponent {
    /// Parse and validate a session or turn identifier.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, RefTransactionError> {
        reject_injection(field, raw)?;
        if raw.is_empty() {
            return Err(RefTransactionError::InvalidComponent {
                field,
                reason: "must not be empty".into(),
            });
        }
        if raw.len() > 128 {
            return Err(RefTransactionError::InvalidComponent {
                field,
                reason: "exceeds 128 characters".into(),
            });
        }
        static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let re =
            RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$").expect("regex"));
        if !re.is_match(raw) {
            return Err(RefTransactionError::InvalidComponent {
                field,
                reason: "must match [A-Za-z0-9][A-Za-z0-9._-]{0,127}".into(),
            });
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ValidatedRefName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate_full(refname: &str) -> Result<Self, RefTransactionError> {
        reject_injection("refname", refname)?;
        git_check_ref_format(refname)?;
        Ok(Self(refname.to_owned()))
    }
}

impl PromotionRefs {
    /// Construct the three refs from a project branch and validated IDs.
    pub fn new(
        branch: &str,
        session_id: &RefComponent,
        turn_id: &RefComponent,
    ) -> Result<Self, RefTransactionError> {
        validate_branch_component(branch)?;
        let project_ref = format!("refs/heads/{branch}");
        let session_ref = format!("refs/lattice/sessions/{}/{}", session_id.as_str(), branch);
        let candidate_ref = format!(
            "refs/lattice/candidates/{}/{}",
            session_id.as_str(),
            turn_id.as_str()
        );
        Ok(Self {
            project_ref: ValidatedRefName::validate_full(&project_ref)?,
            session_ref: ValidatedRefName::validate_full(&session_ref)?,
            candidate_ref: ValidatedRefName::validate_full(&candidate_ref)?,
        })
    }
}

impl RefTransactionRequest {
    pub fn new(
        refs: PromotionRefs,
        old_project_oid: impl Into<String>,
        old_session_oid: Option<String>,
        new_candidate_oid: impl Into<String>,
        new_session_oid: impl Into<String>,
    ) -> Result<Self, RefTransactionError> {
        Self::with_existing_candidate(
            refs,
            old_project_oid,
            old_session_oid,
            new_candidate_oid,
            new_session_oid,
            None,
        )
    }

    /// Create with an existing candidate OID for verify-in-transaction semantics.
    pub fn with_existing_candidate(
        refs: PromotionRefs,
        old_project_oid: impl Into<String>,
        old_session_oid: Option<String>,
        new_candidate_oid: impl Into<String>,
        new_session_oid: impl Into<String>,
        existing_candidate_oid: Option<String>,
    ) -> Result<Self, RefTransactionError> {
        let old_project_oid = old_project_oid.into();
        let new_candidate_oid = new_candidate_oid.into();
        let new_session_oid = new_session_oid.into();
        validate_git_oid("old_project_oid", &old_project_oid)?;
        validate_git_oid("new_candidate_oid", &new_candidate_oid)?;
        validate_git_oid("new_session_oid", &new_session_oid)?;
        if let Some(old) = old_session_oid.as_deref() {
            validate_git_oid("old_session_oid", old)?;
        }
        if let Some(existing) = existing_candidate_oid.as_deref() {
            validate_git_oid("existing_candidate_oid", existing)?;
        }
        Ok(Self {
            refs,
            old_project_oid,
            old_session_oid,
            new_candidate_oid,
            new_session_oid,
            existing_candidate_oid,
        })
    }
}

/// Build the NUL-delimited `git update-ref --stdin -z` payload for `request`.
pub fn build_update_ref_z_payload(
    request: &RefTransactionRequest,
) -> Result<Vec<u8>, RefTransactionError> {
    for field in [
        request.refs.project_ref.as_str(),
        request.refs.session_ref.as_str(),
        request.refs.candidate_ref.as_str(),
        request.old_project_oid.as_str(),
        request.new_candidate_oid.as_str(),
        request.new_session_oid.as_str(),
    ] {
        reject_nul(field)?;
    }
    if let Some(old) = request.old_session_oid.as_deref() {
        reject_nul(old)?;
    }
    if let Some(existing) = request.existing_candidate_oid.as_deref() {
        reject_nul(existing)?;
    }

    let mut payload = Vec::new();
    push_z(&mut payload, &["start"]);

    push_z(
        &mut payload,
        &[
            &format!("update {}", request.refs.project_ref.as_str()),
            &request.new_candidate_oid,
            &request.old_project_oid,
        ],
    );

    match request.old_session_oid.as_deref() {
        Some(old_session) => push_z(
            &mut payload,
            &[
                &format!("update {}", request.refs.session_ref.as_str()),
                &request.new_session_oid,
                old_session,
            ],
        ),
        None => push_z(
            &mut payload,
            &[
                &format!("create {}", request.refs.session_ref.as_str()),
                &request.new_session_oid,
            ],
        ),
    }

    match request.existing_candidate_oid.as_deref() {
        Some(existing_oid) => push_z(
            &mut payload,
            &[
                &format!("update {}", request.refs.candidate_ref.as_str()),
                &request.new_candidate_oid,
                existing_oid,
            ],
        ),
        None => push_z(
            &mut payload,
            &[
                &format!("create {}", request.refs.candidate_ref.as_str()),
                &request.new_candidate_oid,
            ],
        ),
    }

    push_z(&mut payload, &["prepare"]);
    push_z(&mut payload, &["commit"]);
    Ok(payload)
}

/// Execute the three-ref transaction in `repository_path`.
pub fn run_ref_transaction(
    repository_path: &Path,
    request: &RefTransactionRequest,
) -> Result<RefTransactionOutcome, RefTransactionError> {
    let payload = build_update_ref_z_payload(request)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args(["update-ref", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RefTransactionError::Io)?;

    let mut child = output;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&payload).map_err(RefTransactionError::Io)?;
    }

    let output = child.wait_with_output().map_err(RefTransactionError::Io)?;

    if !output.status.success() {
        return Err(RefTransactionError::GitUpdateRef {
            context: "update-ref transaction".into(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    resolve_transaction_outcome(repository_path, request)
}

fn resolve_transaction_outcome(
    repository_path: &Path,
    request: &RefTransactionRequest,
) -> Result<RefTransactionOutcome, RefTransactionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args([
            "rev-parse",
            request.refs.project_ref.as_str(),
            request.refs.session_ref.as_str(),
            request.refs.candidate_ref.as_str(),
        ])
        .output()
        .map_err(RefTransactionError::Io)?;

    if !output.status.success() {
        return Err(RefTransactionError::GitUpdateRef {
            context: "rev-parse transaction refs".into(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let project_oid = lines
        .next()
        .ok_or_else(|| RefTransactionError::GitUpdateRef {
            context: "rev-parse project ref".into(),
            stderr: "missing project oid".into(),
        })?
        .to_owned();
    let session_oid = lines
        .next()
        .ok_or_else(|| RefTransactionError::GitUpdateRef {
            context: "rev-parse session ref".into(),
            stderr: "missing session oid".into(),
        })?
        .to_owned();
    let candidate_oid = lines
        .next()
        .ok_or_else(|| RefTransactionError::GitUpdateRef {
            context: "rev-parse candidate ref".into(),
            stderr: "missing candidate oid".into(),
        })?
        .to_owned();

    Ok(RefTransactionOutcome {
        project_oid,
        session_oid,
        candidate_oid,
    })
}

fn push_z(out: &mut Vec<u8>, fields: &[&str]) {
    for (idx, field) in fields.iter().enumerate() {
        if idx > 0 {
            out.push(0);
        }
        out.extend_from_slice(field.as_bytes());
    }
    out.push(0);
}

fn reject_nul(value: &str) -> Result<(), RefTransactionError> {
    if value.contains('\0') {
        Err(RefTransactionError::NulInProtocolField)
    } else {
        Ok(())
    }
}

fn reject_injection(field: &'static str, raw: &str) -> Result<(), RefTransactionError> {
    if raw.contains('\0') || raw.contains('\n') || raw.contains('\r') {
        return Err(RefTransactionError::InvalidComponent {
            field,
            reason: "must not contain control characters".into(),
        });
    }
    if raw.starts_with('-') {
        return Err(RefTransactionError::InvalidComponent {
            field,
            reason: "must not start with '-'".into(),
        });
    }
    if raw.contains("..") {
        return Err(RefTransactionError::InvalidComponent {
            field,
            reason: "must not contain '..'".into(),
        });
    }
    if raw.contains("@{") {
        return Err(RefTransactionError::InvalidComponent {
            field,
            reason: "must not contain '@{'".into(),
        });
    }
    if raw.contains('\\') {
        return Err(RefTransactionError::InvalidComponent {
            field,
            reason: "must not contain backslash".into(),
        });
    }
    Ok(())
}

fn validate_branch_component(branch: &str) -> Result<(), RefTransactionError> {
    reject_injection("branch", branch)?;
    if branch.is_empty() {
        return Err(RefTransactionError::InvalidComponent {
            field: "branch",
            reason: "must not be empty".into(),
        });
    }
    git_check_ref_format(&format!("refs/heads/{branch}"))
}

fn validate_git_oid(field: &'static str, oid: &str) -> Result<(), RefTransactionError> {
    // Accept 40- (SHA-1) or 64- (SHA-256) character hex OIDs.
    let len = oid.len();
    let is_hex = !oid.is_empty() && oid.chars().all(|c| c.is_ascii_hexdigit());
    if is_hex && (len == 40 || len == 64) {
        Ok(())
    } else {
        Err(RefTransactionError::InvalidOid {
            field,
            reason: "expected 40 (SHA-1) or 64 (SHA-256) hex characters".into(),
        })
    }
}

fn git_check_ref_format(refname: &str) -> Result<(), RefTransactionError> {
    let output = Command::new("git")
        .args(["check-ref-format", refname])
        .output()
        .map_err(RefTransactionError::Io)?;

    if output.status.success() {
        Ok(())
    } else {
        Err(RefTransactionError::InvalidRefName {
            refname: refname.to_owned(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}
