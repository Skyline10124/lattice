// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Guard L1 lexical pattern and host-resolved path-fact validation.

use std::path::{Component, Path, PathBuf};

use lancet_protocol::error::{CepErrorData, GuardErrorData, ValidationErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};

/// Normalized project-relative write-deny pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WritePathPattern {
    ExactFile(PathBuf),
    DirectoryGlob(PathBuf),
}

/// Normalize an exact file or `directory/**` pattern.
pub fn normalize_write_path_pattern(pattern: &str) -> Result<WritePathPattern, CepError> {
    if pattern.is_empty() {
        return Err(schema_error("path pattern must not be empty"));
    }
    if pattern.contains('\0') {
        return Err(schema_error("path pattern must not contain NUL"));
    }
    if pattern.contains("..") {
        return Err(schema_error("path pattern must not contain '..' traversal"));
    }
    if Path::new(pattern).is_absolute() {
        return Err(schema_error("path pattern must be project-root-relative"));
    }
    if pattern.starts_with('!') {
        return Err(schema_error("path pattern negation is not supported"));
    }

    if let Some(base) = pattern.strip_suffix("/**") {
        if base.is_empty() {
            return Err(schema_error(
                "directory glob requires a directory before '/**'",
            ));
        }
        return Ok(WritePathPattern::DirectoryGlob(normalize_lexical(base)?));
    }
    if pattern.contains(['*', '?', '[', ']', '{', '}']) {
        return Err(schema_error("unsupported path glob syntax"));
    }
    if let Some(base) = pattern.strip_suffix('/') {
        if base.is_empty() {
            return Err(schema_error("directory prefix must not be empty"));
        }
        return Ok(WritePathPattern::DirectoryGlob(normalize_lexical(base)?));
    }

    let normalized = normalize_lexical(pattern)?;
    if !pattern.contains('/') && Path::new(pattern).extension().is_none() {
        Ok(WritePathPattern::DirectoryGlob(normalized))
    } else {
        Ok(WritePathPattern::ExactFile(normalized))
    }
}

/// Pure coverage check shared by policy validation and Guard L1.
pub fn write_pattern_covers_relative(pattern: &str, relative_path: &str) -> Result<bool, CepError> {
    if relative_path.contains('\0')
        || relative_path.contains("..")
        || Path::new(relative_path).is_absolute()
    {
        return Err(guard_denied(
            "target path is not a safe project-relative path",
        ));
    }
    let target = normalize_lexical(relative_path)?;
    Ok(match normalize_write_path_pattern(pattern)? {
        WritePathPattern::ExactFile(expected) => target == expected,
        WritePathPattern::DirectoryGlob(ref directory) if directory.as_os_str() == "." => true,
        WritePathPattern::DirectoryGlob(directory) => {
            target == directory || target.starts_with(directory)
        }
    })
}

fn normalize_lexical(path: &str) -> Result<PathBuf, CepError> {
    let mut result = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(schema_error(
                    "path must be project-root-relative without '..'",
                ));
            }
        }
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    Ok(result)
}

/// Host-resolved filesystem facts checked before an operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathFacts {
    pub normalized_path: String,
    pub exists: bool,
    pub kind: Option<PathKind>,
    pub parent_exists: bool,
    pub parent_kind: Option<PathKind>,
    pub canonical_under_workspace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    Symlink,
}

/// Validate host facts and the write-deny blacklist.
pub fn validate_path_facts(
    path: &str,
    facts: &PathFacts,
    denied_write_patterns: &[String],
) -> Result<(), CepError> {
    if path.contains('\0') || path.contains("..") || Path::new(path).is_absolute() {
        return Err(guard_denied("path is malformed"));
    }
    let normalized = normalize_lexical(path)?;
    if facts.normalized_path != normalized.to_string_lossy() {
        return Err(guard_denied("path facts do not match requested path"));
    }
    if !facts.canonical_under_workspace {
        return Err(guard_denied("path escapes workspace containment"));
    }
    for pattern in denied_write_patterns {
        if write_pattern_covers_relative(pattern, &facts.normalized_path)? {
            return Err(guard_denied("path matches a denied write pattern"));
        }
    }
    Ok(())
}

fn schema_error(reason: &str) -> CepError {
    CepError::new(
        CepErrorCode::SchemaMismatch,
        reason,
        CepErrorData::Validation(ValidationErrorData {
            field: "path_pattern".into(),
            reason: reason.into(),
        }),
        Retriability::Permanent,
    )
}

fn guard_denied(reason: &str) -> CepError {
    CepError::new(
        CepErrorCode::GuardDenied,
        reason,
        CepErrorData::Guard(GuardErrorData {
            method: "guard.l1".into(),
            reason: reason.into(),
        }),
        Retriability::Permanent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_glob_covers_descendants_but_exact_file_does_not() {
        assert!(write_pattern_covers_relative("src/**", "src/lib.rs").unwrap());
        assert!(!write_pattern_covers_relative("src/main.rs", "src/lib.rs").unwrap());
    }

    #[test]
    fn malformed_paths_fail_closed() {
        assert!(normalize_write_path_pattern("../outside").is_err());
        assert!(write_pattern_covers_relative("src/**", "/etc/passwd").is_err());
    }

    #[test]
    fn denied_fact_is_rejected() {
        let facts = PathFacts {
            normalized_path: ".git/config".into(),
            exists: true,
            kind: Some(PathKind::File),
            parent_exists: true,
            parent_kind: Some(PathKind::Directory),
            canonical_under_workspace: true,
        };
        assert!(validate_path_facts(".git/config", &facts, &[".git/**".into()]).is_err());
    }
}
