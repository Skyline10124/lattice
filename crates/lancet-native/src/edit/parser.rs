use std::fmt;

use super::diagnostics::parse_error;
use super::hash::{parse_line_hash, parse_old_hash};
use super::types::{
    HashLine, PatchDiagnostic, PatchDocument, PatchHunk, PatchHunkLine, PatchOperation,
};

const BEGIN_MARKER: &str = "*** Begin Patch";
const END_MARKER: &str = "*** End Patch";
const UPDATE_FILE_PREFIX: &str = "*** Update File:";
const DELETE_FILE_PREFIX: &str = "*** Delete File:";
const OLD_HASH_PREFIX: &str = "*** Old Hash:";
const HUNK_MARKER: &str = "@@";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchParseError {
    pub diagnostics: Vec<PatchDiagnostic>,
}

impl PatchParseError {
    fn one(diagnostic: PatchDiagnostic) -> Self {
        Self {
            diagnostics: vec![diagnostic],
        }
    }
}

impl fmt::Display for PatchParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let messages = self
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        write!(formatter, "{messages}")
    }
}

impl std::error::Error for PatchParseError {}

pub fn parse_patch_document(input: &str) -> Result<PatchDocument, PatchParseError> {
    let lines = normalized_lines(input);
    let mut parser = Parser::new(lines);
    parser.parse_document()
}

fn normalized_lines(input: &str) -> Vec<String> {
    input
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect()
}

struct Parser {
    lines: Vec<String>,
    position: usize,
    operations: Vec<PatchOperation>,
}

impl Parser {
    fn new(lines: Vec<String>) -> Self {
        Self {
            lines,
            position: 0,
            operations: Vec::new(),
        }
    }

    fn parse_document(&mut self) -> Result<PatchDocument, PatchParseError> {
        self.expect_exact(BEGIN_MARKER)?;

        while !self.is_eof() {
            if self.current_is(END_MARKER) {
                self.position += 1;
                if !self.is_eof() {
                    return Err(self.error("patch contains content after *** End Patch"));
                }
                if self.operations.is_empty() {
                    return Err(self.error("patch document must contain at least one operation"));
                }
                return Ok(PatchDocument {
                    operations: std::mem::take(&mut self.operations),
                });
            }

            let operation_index = self.operations.len();
            let operation = self.parse_operation(operation_index)?;
            self.operations.push(operation);
        }

        Err(self.error("unterminated patch document; expected *** End Patch"))
    }

    fn parse_operation(
        &mut self,
        operation_index: usize,
    ) -> Result<PatchOperation, PatchParseError> {
        let Some(line) = self.current() else {
            return Err(self.error("unexpected end of patch document"));
        };

        if let Some(path) = parse_header_path(line, UPDATE_FILE_PREFIX) {
            self.position += 1;
            return self.parse_update_file(path, operation_index);
        }

        if let Some(path) = parse_header_path(line, DELETE_FILE_PREFIX) {
            self.position += 1;
            return self.parse_delete_file(path, operation_index);
        }

        Err(self.error(format!("unexpected patch line: {line}")))
    }

    fn parse_update_file(
        &mut self,
        path: String,
        operation_index: usize,
    ) -> Result<PatchOperation, PatchParseError> {
        let old_hash = self.parse_required_old_hash(&path, operation_index)?;
        let mut hunks = Vec::new();

        while let Some(line) = self.current() {
            if is_operation_or_end(line) {
                break;
            }
            if !line.starts_with(HUNK_MARKER) {
                return Err(self.error_with_operation(
                    "update file operation expected '@@' hunk marker",
                    path.clone(),
                    operation_index,
                ));
            }

            let hunk_index = hunks.len();
            self.position += 1;
            let hunk = self.parse_hunk(&path, operation_index, hunk_index)?;
            hunks.push(hunk);
        }

        if hunks.is_empty() {
            return Err(self.error_with_operation(
                "update file operation must contain at least one hunk",
                path,
                operation_index,
            ));
        }

        Ok(PatchOperation::UpdateFile {
            path,
            old_hash,
            hunks,
        })
    }

    fn parse_delete_file(
        &mut self,
        path: String,
        operation_index: usize,
    ) -> Result<PatchOperation, PatchParseError> {
        let old_hash = self.parse_required_old_hash(&path, operation_index)?;
        if let Some(line) = self.current() {
            if !is_operation_or_end(line) {
                return Err(self.error_with_operation(
                    "delete file operation may only contain an old hash line",
                    path.clone(),
                    operation_index,
                ));
            }
        }

        Ok(PatchOperation::DeleteFile { path, old_hash })
    }

    fn parse_required_old_hash(
        &mut self,
        path: &str,
        operation_index: usize,
    ) -> Result<String, PatchParseError> {
        let Some(line) = self.current().map(|line| line.to_string()) else {
            return Err(self.error_with_operation(
                "operation requires *** Old Hash: blake3:<64 hex>",
                path.to_string(),
                operation_index,
            ));
        };

        let Some(hash) = line.strip_prefix(OLD_HASH_PREFIX) else {
            return Err(self.error_with_operation(
                "operation requires *** Old Hash: blake3:<64 hex>",
                path.to_string(),
                operation_index,
            ));
        };

        let hash = hash.trim();
        self.position += 1;
        parse_old_hash(hash).map_err(|diagnostic| {
            PatchParseError::one(
                diagnostic
                    .with_path(path.to_string())
                    .with_operation_index(operation_index),
            )
        })
    }

    fn parse_hunk(
        &mut self,
        path: &str,
        operation_index: usize,
        hunk_index: usize,
    ) -> Result<PatchHunk, PatchParseError> {
        let mut lines = Vec::new();
        while let Some(line) = self.current() {
            if line.starts_with(HUNK_MARKER) || is_operation_or_end(line) {
                break;
            }

            let Some(parsed) = parse_hunk_line(line) else {
                return Err(PatchParseError::one(
                    parse_error("hunk lines must start with ' ', '-', or '+'")
                        .with_path(path.to_string())
                        .with_operation_index(operation_index)
                        .with_hunk_index(hunk_index),
                ));
            };
            lines.push(parsed);
            self.position += 1;
        }

        if lines.is_empty() {
            return Err(PatchParseError::one(
                parse_error("hunk must contain at least one line")
                    .with_path(path.to_string())
                    .with_operation_index(operation_index)
                    .with_hunk_index(hunk_index),
            ));
        }

        Ok(PatchHunk {
            index: hunk_index,
            lines,
        })
    }

    fn expect_exact(&mut self, expected: &str) -> Result<(), PatchParseError> {
        match self.current() {
            Some(line) if line == expected => {
                self.position += 1;
                Ok(())
            }
            Some(_) => Err(self.error(format!("patch must start with {expected}"))),
            None => Err(self.error(format!("patch must start with {expected}"))),
        }
    }

    fn current(&self) -> Option<&str> {
        self.lines.get(self.position).map(String::as_str)
    }

    fn current_is(&self, expected: &str) -> bool {
        self.current().is_some_and(|line| line == expected)
    }

    fn is_eof(&self) -> bool {
        self.position >= self.lines.len()
    }

    fn error(&self, message: impl Into<String>) -> PatchParseError {
        PatchParseError::one(parse_error(message))
    }

    fn error_with_operation(
        &self,
        message: impl Into<String>,
        path: String,
        operation_index: usize,
    ) -> PatchParseError {
        PatchParseError::one(
            parse_error(message)
                .with_path(path)
                .with_operation_index(operation_index),
        )
    }
}

fn parse_header_path(line: &str, prefix: &str) -> Option<String> {
    let path = line.strip_prefix(prefix)?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

fn is_operation_or_end(line: &str) -> bool {
    line == END_MARKER
        || line.starts_with(UPDATE_FILE_PREFIX)
        || line.starts_with(DELETE_FILE_PREFIX)
}

fn parse_hunk_line(line: &str) -> Option<PatchHunkLine> {
    if let Some(content) = line.strip_prefix(' ') {
        return parse_hashline(content).map(PatchHunkLine::Context);
    }
    if let Some(content) = line.strip_prefix('-') {
        return parse_hashline(content).map(PatchHunkLine::Remove);
    }
    if let Some(content) = line.strip_prefix('+') {
        return Some(PatchHunkLine::Add(content.to_string()));
    }
    None
}

fn parse_hashline(content: &str) -> Option<HashLine> {
    let (hash, text) = content.split_once('\t')?;
    let hash = parse_line_hash(hash.trim()).ok()?;
    Some(HashLine {
        hash,
        text: text.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_A: &str = "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const LINE_A: &str = "blake3:aaaa";
    const LINE_B: &str = "blake3:bbbb";

    #[test]
    fn parse_patch_document_accepts_begin_update_delete() {
        let patch = format!(
            "\
*** Begin Patch
*** Update File: src/lib.rs
*** Old Hash: {HASH_A}
@@
 {LINE_A}\tpub fn existing() {{
-{LINE_B}\t    old();
+    new();
 {LINE_A}\t}}
*** Delete File: src/old.rs
*** Old Hash: {HASH_B}
*** End Patch"
        );

        let document = parse_patch_document(&patch).unwrap();

        assert_eq!(document.operations.len(), 2);
        match &document.operations[0] {
            PatchOperation::UpdateFile {
                path,
                old_hash,
                hunks,
            } => {
                assert_eq!(path, "src/lib.rs");
                assert_eq!(old_hash, HASH_A);
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].context_lines(), vec!["pub fn existing() {", "}"]);
                assert_eq!(hunks[0].removed_lines(), vec!["    old();"]);
                assert_eq!(hunks[0].inserted_lines(), vec!["    new();"]);
            }
            operation => panic!("unexpected operation: {operation:?}"),
        }
        assert!(matches!(
            &document.operations[1],
            PatchOperation::DeleteFile { path, old_hash }
                if path == "src/old.rs" && old_hash == HASH_B
        ));
    }

    #[test]
    fn parse_patch_document_rejects_update_without_old_hash() {
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@
-old
+new
*** End Patch";

        let err = parse_patch_document(patch).unwrap_err();

        assert_eq!(err.diagnostics[0].code.as_str(), "parse_error");
        assert_eq!(err.diagnostics[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(err.diagnostics[0].operation_index, Some(0));
        assert!(err.diagnostics[0].message.contains("*** Old Hash:"));
    }

    #[test]
    fn parse_patch_document_rejects_delete_without_old_hash() {
        let patch = "\
*** Begin Patch
*** Delete File: src/lib.rs
*** End Patch";

        let err = parse_patch_document(patch).unwrap_err();

        assert_eq!(err.diagnostics[0].code.as_str(), "parse_error");
        assert_eq!(err.diagnostics[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(err.diagnostics[0].operation_index, Some(0));
    }

    #[test]
    fn parse_patch_document_rejects_add_file_operation() {
        let patch = "\
*** Begin Patch
*** Add File: src/lib.rs
plain text without add marker
*** End Patch";

        let err = parse_patch_document(patch).unwrap_err();

        assert_eq!(err.diagnostics[0].code.as_str(), "parse_error");
        assert!(err.diagnostics[0].message.contains("*** Add File"));
    }

    #[test]
    fn parse_patch_document_rejects_unterminated_patch_text() {
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
*** Old Hash: blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
@@
 blake3:aaaa\tpub fn old() {}
+pub fn new() {}";

        let err = parse_patch_document(patch).unwrap_err();

        assert_eq!(err.diagnostics[0].code.as_str(), "parse_error");
        assert!(err.diagnostics[0].message.contains("unterminated"));
    }

    #[test]
    fn parse_patch_document_rejects_raw_hex_hashes() {
        let raw_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let patch = format!(
            "\
*** Begin Patch
*** Update File: src/lib.rs
*** Old Hash: {raw_hash}
@@
-old
+new
*** End Patch"
        );

        let err = parse_patch_document(&patch).unwrap_err();

        assert_eq!(err.diagnostics[0].code.as_str(), "parse_error");
        assert_eq!(err.diagnostics[0].path.as_deref(), Some("src/lib.rs"));
        assert!(err.diagnostics[0].message.contains("blake3:<64 hex>"));
    }
}
