use super::types::{PatchDiagnostic, PatchErrorCode};

impl PatchErrorCode {
    #[cfg(test)]
    pub fn as_str(self) -> &'static str {
        match self {
            PatchErrorCode::ParseError => "parse_error",
            PatchErrorCode::BoundaryDenied => "boundary_denied",
            PatchErrorCode::MissingFile => "missing_file",
            PatchErrorCode::DuplicatePath => "duplicate_path",
            PatchErrorCode::HashMismatch => "hash_mismatch",
            PatchErrorCode::HunkMissing => "hunk_missing",
            PatchErrorCode::HunkAmbiguous => "hunk_ambiguous",
            PatchErrorCode::IoError => "io_error",
            PatchErrorCode::RollbackFailed => "rollback_failed",
        }
    }
}

impl PatchDiagnostic {
    pub fn new(code: PatchErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            operation_index: None,
            hunk_index: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_operation_index(mut self, operation_index: usize) -> Self {
        self.operation_index = Some(operation_index);
        self
    }

    pub fn with_hunk_index(mut self, hunk_index: usize) -> Self {
        self.hunk_index = Some(hunk_index);
        self
    }
}

pub fn parse_error(message: impl Into<String>) -> PatchDiagnostic {
    PatchDiagnostic::new(PatchErrorCode::ParseError, message)
}
