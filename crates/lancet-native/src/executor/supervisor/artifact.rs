//! Overlay artifact manifest scanning and adjudication.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::enforcement::guard_l1::check_write_scope;
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_catalog::method::RequiredCapability;
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::error::{CepErrorData, ExecutionErrorData, GuardErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};

use super::overlay::{
    CleanupOutcome, OverlayCleanupReport, OverlayDisposition, VerifyDiscardReport,
    VerifyLayerStatus,
};
use super::snapshot::{
    is_snapshot_skipped_path, recheck_lower_baseline, LowerBaseline, LowerConsistencyStatus,
    SnapshotEntry, SnapshotFileKind,
};

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

const HASH_PREFIX: &str = "blake3:";
const WHITEOUT_PREFIX: &str = ".wh.";
const OPAQUE_MARKER: &str = ".wh..wh..opq";
static PREPARED_SET_COUNTER: AtomicU64 = AtomicU64::new(0);
static ARTIFACT_COMMIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
static TEST_COMMIT_SECTION_ACTIVE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_COMMIT_SECTION_MAX: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_COMMIT_SECTION_DELAY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArtifactManifest {
    pub(crate) entries: Vec<ArtifactEntry>,
}

impl ArtifactManifest {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.path().to_string())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactEntry {
    Regular(RegularArtifactEntry),
    Directory(DirectoryArtifactEntry),
    Symlink(SymlinkArtifactEntry),
    Delete(DeleteArtifactEntry),
}

impl ArtifactEntry {
    pub(crate) fn path(&self) -> &str {
        match self {
            ArtifactEntry::Regular(entry) => &entry.path,
            ArtifactEntry::Directory(entry) => &entry.path,
            ArtifactEntry::Symlink(entry) => &entry.path,
            ArtifactEntry::Delete(entry) => &entry.path,
        }
    }

    pub(crate) fn file_kind(&self) -> ArtifactFileKind {
        match self {
            ArtifactEntry::Regular(_) => ArtifactFileKind::RegularFile,
            ArtifactEntry::Directory(_) => ArtifactFileKind::Directory,
            ArtifactEntry::Symlink(_) => ArtifactFileKind::Symlink,
            ArtifactEntry::Delete(entry) => {
                entry.lower_kind.unwrap_or(ArtifactFileKind::WhiteoutDelete)
            }
        }
    }

    pub(crate) fn operation(&self) -> ArtifactOperation {
        match self {
            ArtifactEntry::Regular(entry) => entry.operation,
            ArtifactEntry::Directory(entry) => entry.operation,
            ArtifactEntry::Symlink(entry) => entry.operation,
            ArtifactEntry::Delete(entry) => entry.operation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactOperation {
    Create,
    Update,
    Delete,
}

impl ArtifactOperation {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            ArtifactOperation::Create => "create",
            ArtifactOperation::Update => "update",
            ArtifactOperation::Delete => "delete",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactFileKind {
    RegularFile,
    Directory,
    Symlink,
    WhiteoutDelete,
    Other,
}

impl ArtifactFileKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            ArtifactFileKind::RegularFile => "regular_file",
            ArtifactFileKind::Directory => "directory",
            ArtifactFileKind::Symlink => "symlink",
            ArtifactFileKind::WhiteoutDelete => "whiteout_delete",
            ArtifactFileKind::Other => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegularArtifactEntry {
    pub(crate) path: String,
    pub(crate) operation: ArtifactOperation,
    pub(crate) content_hash: String,
    pub(crate) size_bytes: u64,
    pub(crate) mode: u32,
    pub(crate) lower_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectoryArtifactEntry {
    pub(crate) path: String,
    pub(crate) operation: ArtifactOperation,
    pub(crate) mode: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SymlinkArtifactEntry {
    pub(crate) path: String,
    pub(crate) operation: ArtifactOperation,
    pub(crate) target: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeleteArtifactEntry {
    pub(crate) path: String,
    pub(crate) operation: ArtifactOperation,
    pub(crate) lower_kind: Option<ArtifactFileKind>,
    pub(crate) lower_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RejectedArtifactKind {
    pub(crate) path: String,
    pub(crate) kind: UnsupportedArtifactKind,
    pub(crate) reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum UnsupportedArtifactKind {
    Device,
    Fifo,
    Socket,
    Hardlink,
    Xattr,
    OpaqueDirectory,
    OtherSpecial,
}

impl UnsupportedArtifactKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            UnsupportedArtifactKind::Device => "device",
            UnsupportedArtifactKind::Fifo => "fifo",
            UnsupportedArtifactKind::Socket => "socket",
            UnsupportedArtifactKind::Hardlink => "hardlink",
            UnsupportedArtifactKind::Xattr => "xattr",
            UnsupportedArtifactKind::OpaqueDirectory => "opaque_directory",
            UnsupportedArtifactKind::OtherSpecial => "other_special",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactScanReport {
    pub(crate) manifest: ArtifactManifest,
    pub(crate) rejected_kinds: Vec<RejectedArtifactKind>,
}

pub(crate) fn scan_overlay_upper(
    upper: &Path,
    lower_baseline: &LowerBaseline,
) -> Result<ArtifactScanReport, CepError> {
    let mut scanner = ArtifactScanner {
        upper,
        lower_baseline,
        manifest_by_path: BTreeMap::new(),
        delete_paths: BTreeSet::new(),
        rejected_kinds: Vec::new(),
        explicit_upper_paths: BTreeSet::new(),
    };
    scanner.scan_dir(upper)?;
    scanner.emit_opaque_deletes();

    Ok(ArtifactScanReport {
        manifest: ArtifactManifest {
            entries: scanner.manifest_by_path.into_values().collect(),
        },
        rejected_kinds: scanner.rejected_kinds,
    })
}

struct ArtifactScanner<'a> {
    upper: &'a Path,
    lower_baseline: &'a LowerBaseline,
    manifest_by_path: BTreeMap<String, ArtifactEntry>,
    delete_paths: BTreeSet<String>,
    rejected_kinds: Vec<RejectedArtifactKind>,
    explicit_upper_paths: BTreeSet<String>,
}

impl ArtifactScanner<'_> {
    fn scan_dir(&mut self, current: &Path) -> Result<(), CepError> {
        let dir_entries = std::fs::read_dir(current).map_err(|err| {
            CepError::internal_error(format!(
                "failed to read overlay upper directory '{}': {err}",
                current.display()
            ))
        })?;

        for entry in dir_entries {
            let entry = entry.map_err(|err| {
                CepError::internal_error(format!("failed to read overlay upper entry: {err}"))
            })?;
            let path = entry.path();
            let relative = relative_path(self.upper, &path)?;
            if is_snapshot_skipped_path(&relative) {
                continue;
            }
            if self.record_marker_whiteout(&relative)? {
                continue;
            }
            self.scan_entry(&path, relative)?;
        }
        Ok(())
    }

    fn scan_entry(&mut self, path: &Path, relative: String) -> Result<(), CepError> {
        let metadata = std::fs::symlink_metadata(path).map_err(|err| {
            CepError::internal_error(format!(
                "failed to read artifact metadata for '{}': {err}",
                path.display()
            ))
        })?;

        if is_whiteout_artifact(path, &metadata)? {
            self.record_delete(relative);
            return Ok(());
        }

        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            self.explicit_upper_paths.insert(relative.clone());
            self.manifest_by_path.insert(
                relative.clone(),
                ArtifactEntry::Symlink(SymlinkArtifactEntry {
                    operation: operation_for(&relative, self.lower_baseline),
                    target: std::fs::read_link(path)
                        .map_err(|err| {
                            CepError::internal_error(format!(
                                "failed to read artifact symlink '{}': {err}",
                                path.display()
                            ))
                        })?
                        .to_string_lossy()
                        .to_string(),
                    path: relative,
                }),
            );
            return Ok(());
        }

        if file_type.is_dir() {
            if is_opaque_directory(path)? {
                self.record_opaque_descendant_deletes(&relative);
            }
            self.explicit_upper_paths.insert(relative.clone());
            self.manifest_by_path.insert(
                relative.clone(),
                ArtifactEntry::Directory(DirectoryArtifactEntry {
                    path: relative.clone(),
                    operation: operation_for(&relative, self.lower_baseline),
                    mode: normalized_mode(&metadata).unwrap_or(0o755),
                }),
            );
            self.scan_dir(path)?;
            return Ok(());
        }

        #[cfg(unix)]
        if let Some(kind) = classify_unix_special(
            file_type.is_fifo(),
            file_type.is_socket(),
            file_type.is_char_device() || file_type.is_block_device(),
        ) {
            let reason = match kind {
                UnsupportedArtifactKind::Device => "device artifact",
                UnsupportedArtifactKind::Fifo => "fifo artifact",
                UnsupportedArtifactKind::Socket => "socket artifact",
                _ => "unsupported special artifact kind",
            };
            self.reject(relative, kind, reason);
            return Ok(());
        }

        if file_type.is_file() {
            if has_unsupported_xattrs(path)? {
                self.reject(
                    relative,
                    UnsupportedArtifactKind::Xattr,
                    "unsupported extended attributes",
                );
                return Ok(());
            }
            #[cfg(unix)]
            if metadata.nlink() > 1 {
                self.reject(
                    relative,
                    UnsupportedArtifactKind::Hardlink,
                    "hardlinked regular file artifact",
                );
                return Ok(());
            }

            self.explicit_upper_paths.insert(relative.clone());
            let content = std::fs::read(path).map_err(|err| {
                CepError::internal_error(format!(
                    "failed to hash artifact file '{}': {err}",
                    path.display()
                ))
            })?;
            self.manifest_by_path.insert(
                relative.clone(),
                ArtifactEntry::Regular(RegularArtifactEntry {
                    path: relative.clone(),
                    operation: operation_for(&relative, self.lower_baseline),
                    content_hash: format!("{HASH_PREFIX}{}", blake3::hash(&content).to_hex()),
                    size_bytes: metadata.len(),
                    mode: normalized_mode(&metadata).unwrap_or(0o644),
                    lower_hash: lower_hash(&relative, self.lower_baseline),
                }),
            );
            return Ok(());
        }

        self.reject(
            relative,
            UnsupportedArtifactKind::OtherSpecial,
            "unsupported special artifact kind",
        );
        Ok(())
    }

    fn record_marker_whiteout(&mut self, relative: &str) -> Result<bool, CepError> {
        let Some(file_name) = Path::new(relative)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            return Ok(false);
        };
        if file_name == OPAQUE_MARKER {
            let dir = Path::new(relative)
                .parent()
                .map(path_to_manifest_string)
                .unwrap_or_default();
            self.record_opaque_descendant_deletes(&dir);
            return Ok(true);
        }
        if let Some(deleted_name) = file_name.strip_prefix(WHITEOUT_PREFIX) {
            let delete_path = Path::new(relative)
                .parent()
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(deleted_name);
            self.record_delete(path_to_manifest_string(&delete_path));
            return Ok(true);
        }
        Ok(false)
    }

    fn record_opaque_descendant_deletes(&mut self, directory: &str) {
        let prefix = if directory.is_empty() {
            String::new()
        } else {
            format!("{directory}/")
        };
        let lower_paths = self
            .lower_baseline
            .entries
            .keys()
            .filter(|path| {
                if directory.is_empty() {
                    true
                } else {
                    path.as_str() == directory || path.starts_with(&prefix)
                }
            })
            .cloned()
            .collect::<Vec<_>>();
        for path in lower_paths {
            if !self.explicit_upper_paths.contains(&path) {
                self.record_delete(path);
            }
        }
    }

    fn emit_opaque_deletes(&mut self) {
        let delete_paths = self.delete_paths.iter().cloned().collect::<Vec<_>>();
        for path in delete_paths {
            self.manifest_by_path
                .entry(path.clone())
                .or_insert_with(|| {
                    ArtifactEntry::Delete(DeleteArtifactEntry {
                        lower_kind: lower_kind(&path, self.lower_baseline),
                        lower_hash: lower_hash(&path, self.lower_baseline),
                        operation: ArtifactOperation::Delete,
                        path,
                    })
                });
        }
    }

    fn record_delete(&mut self, path: String) {
        self.delete_paths.insert(path);
    }

    fn reject(&mut self, path: String, kind: UnsupportedArtifactKind, reason: &str) {
        self.rejected_kinds.push(RejectedArtifactKind {
            path,
            kind,
            reason: reason.to_string(),
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactAdjudicationStatus {
    Accepted,
    Denied,
}

impl ArtifactAdjudicationStatus {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            ArtifactAdjudicationStatus::Accepted => "accepted",
            ArtifactAdjudicationStatus::Denied => "denied",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactAdjudicationReport {
    pub(crate) artifact_manifest_status: ArtifactAdjudicationStatus,
    pub(crate) artifact_adjudication: String,
    pub(crate) overlay_disposition: OverlayDisposition,
    pub(crate) rejected_kinds: Vec<RejectedArtifactKind>,
    pub(crate) denied_paths: Vec<String>,
    pub(crate) observed_effects: Vec<ObservedEffect>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactAdjudicationFailure {
    pub(crate) report: ArtifactAdjudicationReport,
    pub(crate) error: CepError,
}

impl ArtifactAdjudicationFailure {
    pub(crate) fn into_error(self) -> CepError {
        self.error
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn adjudicate_artifact_manifest(
    manifest: &ArtifactManifest,
    rejected_kinds: &[RejectedArtifactKind],
    project_root: &str,
    write_path_patterns: &[String],
    authorized_region: &AuthorizedEffectRegion,
) -> Result<ArtifactAdjudicationReport, ArtifactAdjudicationFailure> {
    let observed_effects = manifest_observed_effects(manifest, project_root);

    if !rejected_kinds.is_empty() {
        let report = denied_report(
            "denied_unsupported_kind",
            rejected_kinds.to_vec(),
            Vec::new(),
            observed_effects,
        );
        return Err(ArtifactAdjudicationFailure {
            report,
            error: guard_denied("artifact manifest contains unsupported artifact kinds"),
        });
    }

    let mut out_of_scope_paths = Vec::new();
    for entry in &manifest.entries {
        if let Err(err) = check_write_scope(entry.path(), project_root, write_path_patterns) {
            if err.code == CepErrorCode::ReplanRequired {
                out_of_scope_paths.push(entry.path().to_string());
            } else {
                let report = denied_report(
                    "denied_path_escape",
                    Vec::new(),
                    vec![entry.path().to_string()],
                    observed_effects,
                );
                return Err(ArtifactAdjudicationFailure { report, error: err });
            }
        }
    }
    if !out_of_scope_paths.is_empty() {
        let report = denied_report(
            "denied_write_scope",
            Vec::new(),
            out_of_scope_paths.clone(),
            observed_effects,
        );
        return Err(ArtifactAdjudicationFailure {
            report,
            error: replan_required(
                "artifact paths outside active WritePath scope",
                out_of_scope_paths,
            ),
        });
    }

    let effect_region_misses = observed_effects
        .iter()
        .filter(|effect| !authorized_region.contains(effect))
        .map(observed_effect_path)
        .collect::<Vec<_>>();
    if !effect_region_misses.is_empty() {
        let report = denied_report(
            "denied_effect_region",
            Vec::new(),
            effect_region_misses.clone(),
            observed_effects,
        );
        return Err(ArtifactAdjudicationFailure {
            report,
            error: replan_required(
                "artifact effects outside authorized effect region",
                effect_region_misses,
            ),
        });
    }

    Ok(ArtifactAdjudicationReport {
        artifact_manifest_status: ArtifactAdjudicationStatus::Accepted,
        artifact_adjudication: "accepted".to_string(),
        overlay_disposition: OverlayDisposition::Committed,
        rejected_kinds: Vec::new(),
        denied_paths: Vec::new(),
        observed_effects,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ArtifactApplyOrder {
    DeleteOrWhiteout,
    DirectoryCreate,
    CreateOrUpdate,
}

impl ArtifactApplyOrder {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            ArtifactApplyOrder::DeleteOrWhiteout => "delete_or_whiteout",
            ArtifactApplyOrder::DirectoryCreate => "directory_create",
            ArtifactApplyOrder::CreateOrUpdate => "create_or_update",
        }
    }
}

#[derive(Debug)]
pub(crate) struct PreparedArtifactSet {
    pub(crate) entries: Vec<PreparedArtifactEntry>,
    pub(crate) staging_root: PathBuf,
    pub(crate) pre_verify_lower_baseline: LowerBaseline,
}

impl PreparedArtifactSet {
    pub(crate) fn staging_root(&self) -> &Path {
        &self.staging_root
    }
}

impl Drop for PreparedArtifactSet {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.staging_root);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PreparedArtifactEntry {
    Regular(PreparedRegularArtifactEntry),
    Directory(PreparedDirectoryArtifactEntry),
    Symlink(PreparedSymlinkArtifactEntry),
    Delete(PreparedDeleteArtifactEntry),
}

impl PreparedArtifactEntry {
    pub(crate) fn path(&self) -> &str {
        match self {
            PreparedArtifactEntry::Regular(entry) => &entry.path,
            PreparedArtifactEntry::Directory(entry) => &entry.path,
            PreparedArtifactEntry::Symlink(entry) => &entry.path,
            PreparedArtifactEntry::Delete(entry) => &entry.path,
        }
    }

    pub(crate) fn apply_order(&self) -> ArtifactApplyOrder {
        match self {
            PreparedArtifactEntry::Delete(_) => ArtifactApplyOrder::DeleteOrWhiteout,
            PreparedArtifactEntry::Directory(_) => ArtifactApplyOrder::DirectoryCreate,
            PreparedArtifactEntry::Regular(_) | PreparedArtifactEntry::Symlink(_) => {
                ArtifactApplyOrder::CreateOrUpdate
            }
        }
    }

    pub(crate) fn kind(&self) -> ArtifactFileKind {
        match self {
            PreparedArtifactEntry::Regular(_) => ArtifactFileKind::RegularFile,
            PreparedArtifactEntry::Directory(_) => ArtifactFileKind::Directory,
            PreparedArtifactEntry::Symlink(_) => ArtifactFileKind::Symlink,
            PreparedArtifactEntry::Delete(entry) => {
                entry.lower_kind.unwrap_or(ArtifactFileKind::WhiteoutDelete)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedRegularArtifactEntry {
    pub(crate) path: String,
    pub(crate) operation: ArtifactOperation,
    pub(crate) staged_path: PathBuf,
    pub(crate) content_hash: String,
    pub(crate) size_bytes: u64,
    pub(crate) mode: u32,
    pub(crate) lower_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedDirectoryArtifactEntry {
    pub(crate) path: String,
    pub(crate) operation: ArtifactOperation,
    pub(crate) mode: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedSymlinkArtifactEntry {
    pub(crate) path: String,
    pub(crate) operation: ArtifactOperation,
    pub(crate) target: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedDeleteArtifactEntry {
    pub(crate) path: String,
    pub(crate) lower_kind: Option<ArtifactFileKind>,
    pub(crate) lower_hash: Option<String>,
}

pub(crate) fn prepare_artifact_commit_set(
    accepted_manifest: ArtifactManifest,
    artifact_upper: &Path,
    pre_verify_lower_baseline: &LowerBaseline,
) -> Result<PreparedArtifactSet, CepError> {
    let staging_root = create_private_staging_root(artifact_upper)?;
    let mut prepared_entries = Vec::with_capacity(accepted_manifest.entries.len());

    for (index, entry) in accepted_manifest.entries.into_iter().enumerate() {
        let prepared = match entry {
            ArtifactEntry::Regular(entry) => {
                let upper_path = checked_artifact_path(artifact_upper, &entry.path)?;
                let content = std::fs::read(&upper_path).map_err(|err| {
                    cleanup_and_error(
                        &staging_root,
                        format!(
                            "failed to read accepted artifact '{}': {err}",
                            upper_path.display()
                        ),
                    )
                })?;
                let actual_hash = format!("{HASH_PREFIX}{}", blake3::hash(&content).to_hex());
                if actual_hash != entry.content_hash {
                    return Err(cleanup_and_error(
                        &staging_root,
                        format!(
                            "accepted artifact '{}' hash mismatch: manifest {}, actual {}",
                            entry.path, entry.content_hash, actual_hash
                        ),
                    ));
                }
                let staged_path = staging_root.join(format!("{index:016}.artifact"));
                let mut staged = std::fs::File::create(&staged_path).map_err(|err| {
                    cleanup_and_error(
                        &staging_root,
                        format!(
                            "failed to create prepared artifact '{}': {err}",
                            staged_path.display()
                        ),
                    )
                })?;
                staged.write_all(&content).map_err(|err| {
                    cleanup_and_error(
                        &staging_root,
                        format!(
                            "failed to write prepared artifact '{}': {err}",
                            staged_path.display()
                        ),
                    )
                })?;
                PreparedArtifactEntry::Regular(PreparedRegularArtifactEntry {
                    path: entry.path,
                    operation: entry.operation,
                    staged_path,
                    content_hash: entry.content_hash,
                    size_bytes: entry.size_bytes,
                    mode: entry.mode & 0o777,
                    lower_hash: entry.lower_hash,
                })
            }
            ArtifactEntry::Directory(entry) => {
                PreparedArtifactEntry::Directory(PreparedDirectoryArtifactEntry {
                    path: entry.path,
                    operation: entry.operation,
                    mode: entry.mode & 0o777,
                })
            }
            ArtifactEntry::Symlink(entry) => {
                PreparedArtifactEntry::Symlink(PreparedSymlinkArtifactEntry {
                    path: entry.path,
                    operation: entry.operation,
                    target: entry.target,
                })
            }
            ArtifactEntry::Delete(entry) => {
                PreparedArtifactEntry::Delete(PreparedDeleteArtifactEntry {
                    path: entry.path,
                    lower_kind: entry.lower_kind,
                    lower_hash: entry.lower_hash,
                })
            }
        };
        prepared_entries.push(prepared);
    }

    prepared_entries.sort_by_key(prepared_apply_sort_key);

    Ok(PreparedArtifactSet {
        entries: prepared_entries,
        staging_root,
        pre_verify_lower_baseline: pre_verify_lower_baseline.clone(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitOutcome {
    NotStarted,
    Committed,
    Denied,
    Partial,
}

impl CommitOutcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            CommitOutcome::NotStarted => "not_started",
            CommitOutcome::Committed => "committed",
            CommitOutcome::Denied => "denied",
            CommitOutcome::Partial => "partial",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppliedArtifactEntry {
    pub(crate) path: String,
    pub(crate) kind: ArtifactFileKind,
    pub(crate) operation: ArtifactOperation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FailedArtifactEntry {
    pub(crate) path: String,
    pub(crate) kind: ArtifactFileKind,
    pub(crate) error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactCommitReport {
    pub(crate) commit_outcome: CommitOutcome,
    pub(crate) lower_consistency: String,
    pub(crate) artifact_adjudication: String,
    pub(crate) overlay_disposition: OverlayDisposition,
    pub(crate) applied_entries: Vec<AppliedArtifactEntry>,
    pub(crate) failed_entry: Option<FailedArtifactEntry>,
    pub(crate) changed_paths: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct ArtifactCommitFailure {
    pub(crate) report: ArtifactCommitReport,
    pub(crate) error: CepError,
}

impl ArtifactCommitFailure {
    pub(crate) fn into_error(self) -> CepError {
        self.error
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn commit_artifact_manifest(
    project_root: &Path,
    prepared: PreparedArtifactSet,
    artifact_cleanup: &OverlayCleanupReport,
    verify_cleanup: &VerifyDiscardReport,
) -> Result<ArtifactCommitReport, ArtifactCommitFailure> {
    if let Err(reason) = validate_commit_cleanup_gates(artifact_cleanup, verify_cleanup) {
        let report = ArtifactCommitReport {
            commit_outcome: CommitOutcome::NotStarted,
            lower_consistency: "not_checked".to_string(),
            artifact_adjudication: "denied_cleanup_failed".to_string(),
            overlay_disposition: OverlayDisposition::Denied,
            applied_entries: Vec::new(),
            failed_entry: Some(FailedArtifactEntry {
                path: String::new(),
                kind: ArtifactFileKind::Other,
                error: reason.clone(),
            }),
            changed_paths: Vec::new(),
        };
        return Err(ArtifactCommitFailure {
            report,
            error: execution_failed(reason),
        });
    }

    let _guard = artifact_commit_lock().lock().map_err(|_| {
        let reason = "artifact commit lock poisoned".to_string();
        ArtifactCommitFailure {
            report: ArtifactCommitReport {
                commit_outcome: CommitOutcome::NotStarted,
                lower_consistency: "not_checked".to_string(),
                artifact_adjudication: "denied_commit_lock".to_string(),
                overlay_disposition: OverlayDisposition::Denied,
                applied_entries: Vec::new(),
                failed_entry: Some(FailedArtifactEntry {
                    path: String::new(),
                    kind: ArtifactFileKind::Other,
                    error: reason.clone(),
                }),
                changed_paths: Vec::new(),
            },
            error: execution_failed(reason),
        }
    })?;
    test_commit_section_enter();
    let result = commit_artifact_manifest_locked(project_root, &prepared);
    test_commit_section_exit();
    result
}

#[allow(clippy::result_large_err)]
fn commit_artifact_manifest_locked(
    project_root: &Path,
    prepared: &PreparedArtifactSet,
) -> Result<ArtifactCommitReport, ArtifactCommitFailure> {
    let lower_recheck = recheck_lower_baseline(project_root, &prepared.pre_verify_lower_baseline)
        .map_err(|error| ArtifactCommitFailure {
        report: ArtifactCommitReport {
            commit_outcome: CommitOutcome::NotStarted,
            lower_consistency: "check_failed".to_string(),
            artifact_adjudication: "denied_lower_check_failed".to_string(),
            overlay_disposition: OverlayDisposition::Denied,
            applied_entries: Vec::new(),
            failed_entry: Some(FailedArtifactEntry {
                path: String::new(),
                kind: ArtifactFileKind::Other,
                error: error.message.clone(),
            }),
            changed_paths: Vec::new(),
        },
        error,
    })?;
    if lower_recheck.status == LowerConsistencyStatus::DriftDetected {
        let report = ArtifactCommitReport {
            commit_outcome: CommitOutcome::Denied,
            lower_consistency: lower_recheck.status.label().to_string(),
            artifact_adjudication: "denied_lower_drift".to_string(),
            overlay_disposition: OverlayDisposition::Denied,
            applied_entries: Vec::new(),
            failed_entry: None,
            changed_paths: lower_recheck.changed_paths.clone(),
        };
        return Err(ArtifactCommitFailure {
            report,
            error: lower_drift_error(lower_recheck.changed_paths),
        });
    }

    apply_prepared_artifact_set(project_root, prepared)
}

#[allow(clippy::result_large_err)]
pub(crate) fn apply_prepared_artifact_set(
    project_root: &Path,
    prepared: &PreparedArtifactSet,
) -> Result<ArtifactCommitReport, ArtifactCommitFailure> {
    let mut applied_entries = Vec::new();

    for entry in &prepared.entries {
        if let Err(error) = apply_prepared_entry(project_root, entry) {
            let failed_entry = FailedArtifactEntry {
                path: entry.path().to_string(),
                kind: entry.kind(),
                error,
            };
            let report = ArtifactCommitReport {
                commit_outcome: CommitOutcome::Partial,
                lower_consistency: LowerConsistencyStatus::Unchanged.label().to_string(),
                artifact_adjudication: "accepted".to_string(),
                overlay_disposition: OverlayDisposition::Committed,
                applied_entries,
                failed_entry: Some(failed_entry.clone()),
                changed_paths: Vec::new(),
            };
            return Err(ArtifactCommitFailure {
                report,
                error: execution_failed(format!(
                    "artifact commit partially applied before failing at '{}': {}",
                    failed_entry.path, failed_entry.error
                )),
            });
        }
        applied_entries.push(AppliedArtifactEntry {
            path: entry.path().to_string(),
            kind: entry.kind(),
            operation: prepared_entry_operation(entry),
        });
    }

    Ok(ArtifactCommitReport {
        commit_outcome: CommitOutcome::Committed,
        lower_consistency: LowerConsistencyStatus::Unchanged.label().to_string(),
        artifact_adjudication: "accepted".to_string(),
        overlay_disposition: OverlayDisposition::Committed,
        applied_entries,
        failed_entry: None,
        changed_paths: Vec::new(),
    })
}

pub(crate) fn manifest_observed_effects(
    manifest: &ArtifactManifest,
    project_root: &str,
) -> Vec<ObservedEffect> {
    manifest
        .entries
        .iter()
        .map(|entry| ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(Path::new(project_root).join(entry.path())),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        })
        .collect()
}

fn prepared_apply_sort_key(entry: &PreparedArtifactEntry) -> (u8, usize, String) {
    match entry.apply_order() {
        ArtifactApplyOrder::DeleteOrWhiteout => (
            0,
            usize::MAX - path_depth(entry.path()),
            path_sort(entry.path()),
        ),
        ArtifactApplyOrder::DirectoryCreate => {
            (1, path_depth(entry.path()), path_sort(entry.path()))
        }
        ArtifactApplyOrder::CreateOrUpdate => (2, 0, path_sort(entry.path())),
    }
}

fn path_depth(path: &str) -> usize {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .count()
}

fn path_sort(path: &str) -> String {
    path.to_string()
}

fn checked_artifact_path(root: &Path, relative: &str) -> Result<PathBuf, CepError> {
    if relative.contains('\0') {
        return Err(CepError::internal_error(format!(
            "artifact path '{relative}' contains NUL"
        )));
    }
    let path = Path::new(relative);
    if path.is_absolute() || relative.split('/').any(|segment| segment == "..") {
        return Err(CepError::internal_error(format!(
            "accepted artifact path '{relative}' is not project-relative"
        )));
    }
    Ok(root.join(path))
}

fn create_private_staging_root(artifact_upper: &Path) -> Result<PathBuf, CepError> {
    let candidate_parent = std::env::temp_dir().join("lattice-prepared-artifacts");
    if candidate_parent == artifact_upper || candidate_parent.starts_with(artifact_upper) {
        return Err(CepError::internal_error(
            "prepared artifact staging path must be outside artifact upper",
        ));
    }
    std::fs::create_dir_all(&candidate_parent).map_err(|err| {
        CepError::internal_error(format!(
            "failed to create prepared artifact staging parent '{}': {err}",
            candidate_parent.display()
        ))
    })?;
    for _ in 0..16 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let counter = PREPARED_SET_COUNTER.fetch_add(1, Ordering::Relaxed);
        let staging_root =
            candidate_parent.join(format!("{}-{now:x}-{counter:x}", std::process::id()));
        match std::fs::create_dir(&staging_root) {
            Ok(()) => return Ok(staging_root),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(CepError::internal_error(format!(
                    "failed to create prepared artifact staging root '{}': {err}",
                    staging_root.display()
                )));
            }
        }
    }
    Err(CepError::internal_error(
        "failed to allocate unique prepared artifact staging root",
    ))
}

fn cleanup_and_error(staging_root: &Path, reason: String) -> CepError {
    let _ = std::fs::remove_dir_all(staging_root);
    CepError::internal_error(reason)
}

fn validate_commit_cleanup_gates(
    artifact_cleanup: &OverlayCleanupReport,
    verify_cleanup: &VerifyDiscardReport,
) -> Result<(), String> {
    if !artifact_cleanup.child_cleanup_succeeded {
        return Err("artifact commit denied because child cleanup failed".to_string());
    }
    if artifact_cleanup.outcome != CleanupOutcome::Succeeded {
        return Err(format!(
            "artifact commit denied because artifact overlay cleanup failed: {:?}",
            artifact_cleanup.failure_kind
        ));
    }
    let verify_cleanup_succeeded = verify_cleanup.verify_layer == VerifyLayerStatus::NotRun
        || (verify_cleanup.verify_layer == VerifyLayerStatus::Discarded
            && verify_cleanup.cleanup_outcome == Some(CleanupOutcome::Succeeded));
    if !verify_cleanup_succeeded {
        return Err(format!(
            "artifact commit denied because verify cleanup did not succeed: {}",
            verify_cleanup.verify_layer.label()
        ));
    }
    Ok(())
}

fn artifact_commit_lock() -> &'static Mutex<()> {
    ARTIFACT_COMMIT_LOCK.get_or_init(|| Mutex::new(()))
}

fn apply_prepared_entry(project_root: &Path, entry: &PreparedArtifactEntry) -> Result<(), String> {
    match entry {
        PreparedArtifactEntry::Regular(entry) => apply_regular_entry(project_root, entry),
        PreparedArtifactEntry::Directory(entry) => apply_directory_entry(project_root, entry),
        PreparedArtifactEntry::Symlink(entry) => apply_symlink_entry(project_root, entry),
        PreparedArtifactEntry::Delete(entry) => apply_delete_entry(project_root, entry),
    }
}

fn prepared_entry_operation(entry: &PreparedArtifactEntry) -> ArtifactOperation {
    match entry {
        PreparedArtifactEntry::Regular(entry) => entry.operation,
        PreparedArtifactEntry::Directory(entry) => entry.operation,
        PreparedArtifactEntry::Symlink(entry) => entry.operation,
        PreparedArtifactEntry::Delete(_) => ArtifactOperation::Delete,
    }
}

fn target_path(project_root: &Path, relative: &str) -> Result<PathBuf, String> {
    checked_artifact_path(project_root, relative).map_err(|error| error.message)
}

fn ensure_parent_for_target(project_root: &Path, relative: &str) -> Result<PathBuf, String> {
    let target = target_path(project_root, relative)?;
    if let Some(parent) = target.parent() {
        ensure_existing_components_are_not_symlinks(project_root, relative)?;
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create parent '{}': {err}", parent.display()))?;
    }
    Ok(target)
}

fn ensure_existing_components_are_not_symlinks(
    project_root: &Path,
    relative: &str,
) -> Result<(), String> {
    let mut current = project_root.to_path_buf();
    let path = Path::new(relative);
    for component in path
        .components()
        .take(path.components().count().saturating_sub(1))
    {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "refusing to write through symlink parent '{}'",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(format!(
                    "failed to inspect parent component '{}': {err}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}

fn apply_regular_entry(
    project_root: &Path,
    entry: &PreparedRegularArtifactEntry,
) -> Result<(), String> {
    let target = ensure_parent_for_target(project_root, &entry.path)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "refusing to write regular artifact through symlink '{}'",
                target.display()
            ));
        }
    }
    let content = std::fs::read(&entry.staged_path).map_err(|err| {
        format!(
            "failed to read prepared artifact '{}': {err}",
            entry.staged_path.display()
        )
    })?;
    let actual_hash = format!("{HASH_PREFIX}{}", blake3::hash(&content).to_hex());
    if actual_hash != entry.content_hash {
        return Err(format!(
            "prepared artifact '{}' hash mismatch before apply",
            entry.path
        ));
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&target)
        .map_err(|err| format!("failed to open target '{}': {err}", target.display()))?;
    file.write_all(&content)
        .map_err(|err| format!("failed to write target '{}': {err}", target.display()))?;
    set_mode_if_supported(&target, entry.mode)
}

fn apply_directory_entry(
    project_root: &Path,
    entry: &PreparedDirectoryArtifactEntry,
) -> Result<(), String> {
    let target = target_path(project_root, &entry.path)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "refusing to create directory through symlink '{}'",
                target.display()
            ));
        }
    }
    ensure_existing_components_are_not_symlinks(project_root, &entry.path)?;
    std::fs::create_dir_all(&target)
        .map_err(|err| format!("failed to create directory '{}': {err}", target.display()))?;
    set_mode_if_supported(&target, entry.mode)
}

fn apply_symlink_entry(
    project_root: &Path,
    entry: &PreparedSymlinkArtifactEntry,
) -> Result<(), String> {
    let target = ensure_parent_for_target(project_root, &entry.path)?;
    delete_target_path_if_present(&target)?;
    create_symlink(&entry.target, &target)
}

fn apply_delete_entry(
    project_root: &Path,
    entry: &PreparedDeleteArtifactEntry,
) -> Result<(), String> {
    let target = target_path(project_root, &entry.path)?;
    delete_target_path_if_present(&target)
}

fn delete_target_path_if_present(target: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(format!(
                "failed to inspect target '{}': {err}",
                target.display()
            ));
        }
    };
    if metadata.file_type().is_dir() {
        std::fs::remove_dir_all(target)
            .map_err(|err| format!("failed to delete directory '{}': {err}", target.display()))
    } else {
        std::fs::remove_file(target)
            .map_err(|err| format!("failed to delete file '{}': {err}", target.display()))
    }
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|err| format!("failed to create symlink '{}': {err}", link.display()))
}

#[cfg(not(unix))]
fn create_symlink(_target: &str, link: &Path) -> Result<(), String> {
    Err(format!(
        "symlink artifact apply is unsupported on this platform for '{}'",
        link.display()
    ))
}

#[cfg(unix)]
fn set_mode_if_supported(path: &Path, mode: u32) -> Result<(), String> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777))
        .map_err(|err| format!("failed to set mode for '{}': {err}", path.display()))
}

#[cfg(not(unix))]
fn set_mode_if_supported(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

fn execution_failed(reason: impl Into<String>) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        reason.into(),
        CepErrorData::Execution(ExecutionErrorData {
            command: "artifact.commit".to_string(),
            exit_code: None,
            stderr: None,
        }),
        Retriability::Transient,
    )
}

fn lower_drift_error(changed_paths: Vec<String>) -> CepError {
    CepError::new(
        CepErrorCode::ReplanRequired,
        format!("artifact lower drift detected before commit: {changed_paths:?}"),
        CepErrorData::Guard(GuardErrorData {
            method: "artifact.commit".into(),
            reason: format!("ReplanRequired: lower drift: {changed_paths:?}"),
        }),
        Retriability::RequiresReplan,
    )
}

#[cfg(test)]
fn test_commit_section_enter() {
    TEST_COMMIT_SECTION_ACTIVE.fetch_add(1, Ordering::SeqCst);
    let active = TEST_COMMIT_SECTION_ACTIVE.load(Ordering::SeqCst);
    TEST_COMMIT_SECTION_MAX.fetch_max(active, Ordering::SeqCst);
    if TEST_COMMIT_SECTION_DELAY.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

#[cfg(test)]
fn test_commit_section_exit() {
    TEST_COMMIT_SECTION_ACTIVE.fetch_sub(1, Ordering::SeqCst);
}

#[cfg(not(test))]
fn test_commit_section_enter() {}

#[cfg(not(test))]
fn test_commit_section_exit() {}

fn observed_effect_path(effect: &ObservedEffect) -> String {
    match &effect.scope {
        ObservedScope::Path(path) => path.to_string_lossy().to_string(),
        scope => format!("{scope:?}"),
    }
}

fn denied_report(
    artifact_adjudication: &str,
    rejected_kinds: Vec<RejectedArtifactKind>,
    denied_paths: Vec<String>,
    observed_effects: Vec<ObservedEffect>,
) -> ArtifactAdjudicationReport {
    ArtifactAdjudicationReport {
        artifact_manifest_status: ArtifactAdjudicationStatus::Denied,
        artifact_adjudication: artifact_adjudication.to_string(),
        overlay_disposition: OverlayDisposition::Denied,
        rejected_kinds,
        denied_paths,
        observed_effects,
    }
}

fn guard_denied(reason: &str) -> CepError {
    CepError::new(
        CepErrorCode::GuardDenied,
        reason,
        CepErrorData::Guard(GuardErrorData {
            method: "artifact.adjudication".into(),
            reason: reason.to_string(),
        }),
        Retriability::Permanent,
    )
}

fn replan_required(reason: &str, paths: Vec<String>) -> CepError {
    CepError::new(
        CepErrorCode::ReplanRequired,
        format!("{reason}: {paths:?}"),
        CepErrorData::Guard(GuardErrorData {
            method: "artifact.adjudication".into(),
            reason: format!("ReplanRequired: {reason}: {paths:?}"),
        }),
        Retriability::RequiresReplan,
    )
}

fn operation_for(path: &str, lower_baseline: &LowerBaseline) -> ArtifactOperation {
    if lower_baseline.entries.contains_key(path) {
        ArtifactOperation::Update
    } else {
        ArtifactOperation::Create
    }
}

fn lower_hash(path: &str, lower_baseline: &LowerBaseline) -> Option<String> {
    lower_baseline
        .entries
        .get(path)
        .and_then(|entry| entry.content_hash.clone())
}

fn lower_kind(path: &str, lower_baseline: &LowerBaseline) -> Option<ArtifactFileKind> {
    lower_baseline
        .entries
        .get(path)
        .map(snapshot_kind_to_artifact_kind)
}

fn snapshot_kind_to_artifact_kind(entry: &SnapshotEntry) -> ArtifactFileKind {
    match entry.file_kind {
        SnapshotFileKind::RegularFile => ArtifactFileKind::RegularFile,
        SnapshotFileKind::Directory => ArtifactFileKind::Directory,
        SnapshotFileKind::Symlink => ArtifactFileKind::Symlink,
        SnapshotFileKind::Other => ArtifactFileKind::Other,
    }
}

fn relative_path(base: &Path, path: &Path) -> Result<String, CepError> {
    path.strip_prefix(base)
        .map(path_to_manifest_string)
        .map_err(|err| {
            CepError::internal_error(format!(
                "failed to compute overlay upper relative path for '{}': {err}",
                path.display()
            ))
        })
}

fn path_to_manifest_string(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

fn normalized_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        if metadata.is_file() || metadata.is_dir() {
            Some(metadata.permissions().mode() & 0o777)
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    {
        if metadata.permissions().readonly() {
            Some(0o555)
        } else {
            Some(0o755)
        }
    }
}

#[cfg(unix)]
fn classify_unix_special(
    is_fifo: bool,
    is_socket: bool,
    is_device: bool,
) -> Option<UnsupportedArtifactKind> {
    if is_fifo {
        Some(UnsupportedArtifactKind::Fifo)
    } else if is_socket {
        Some(UnsupportedArtifactKind::Socket)
    } else if is_device {
        Some(UnsupportedArtifactKind::Device)
    } else {
        None
    }
}

fn is_whiteout_artifact(path: &Path, metadata: &std::fs::Metadata) -> Result<bool, CepError> {
    #[cfg(unix)]
    {
        let file_type = metadata.file_type();
        if file_type.is_char_device() && metadata.rdev() == 0 {
            return Ok(true);
        }
        if file_type.is_file() && has_xattr(path, "trusted.overlay.whiteout")? {
            return Ok(true);
        }
    }
    let _ = path;
    let _ = metadata;
    Ok(false)
}

fn is_opaque_directory(path: &Path) -> Result<bool, CepError> {
    #[cfg(unix)]
    {
        if let Some(value) = read_xattr(path, "trusted.overlay.opaque")? {
            return Ok(value == b"y");
        }
    }
    let _ = path;
    Ok(false)
}

fn has_unsupported_xattrs(path: &Path) -> Result<bool, CepError> {
    #[cfg(target_os = "linux")]
    {
        let names = list_xattrs(path)?;
        Ok(names
            .iter()
            .any(|name| name != "trusted.overlay.whiteout" && name != "trusted.overlay.opaque"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Ok(false)
    }
}

#[cfg(unix)]
fn has_xattr(path: &Path, name: &str) -> Result<bool, CepError> {
    Ok(read_xattr(path, name)?.is_some())
}

#[cfg(target_os = "linux")]
fn list_xattrs(path: &Path) -> Result<Vec<String>, CepError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        CepError::internal_error(format!("artifact path '{}' contains NUL", path.display()))
    })?;
    unsafe {
        let size = libc::llistxattr(path.as_ptr(), std::ptr::null_mut(), 0);
        if size < 0 {
            let err = std::io::Error::last_os_error();
            if matches!(
                err.raw_os_error(),
                Some(libc::ENOTSUP) | Some(libc::ENODATA) | Some(libc::EPERM)
            ) {
                return Ok(Vec::new());
            }
            return Err(CepError::internal_error(format!(
                "failed to list artifact xattrs: {err}"
            )));
        }
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; size as usize];
        let read = libc::llistxattr(path.as_ptr(), buf.as_mut_ptr().cast(), buf.len());
        if read < 0 {
            return Err(CepError::internal_error(format!(
                "failed to read artifact xattr names: {}",
                std::io::Error::last_os_error()
            )));
        }
        buf.truncate(read as usize);
        Ok(buf
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).to_string())
            .collect())
    }
}

#[cfg(unix)]
fn read_xattr(path: &Path, name: &str) -> Result<Option<Vec<u8>>, CepError> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path_c = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            CepError::internal_error(format!("artifact path '{}' contains NUL", path.display()))
        })?;
        let name_c = CString::new(name).map_err(|_| {
            CepError::internal_error(format!("artifact xattr name '{name}' contains NUL"))
        })?;
        unsafe {
            let size = libc::lgetxattr(path_c.as_ptr(), name_c.as_ptr(), std::ptr::null_mut(), 0);
            if size < 0 {
                let err = std::io::Error::last_os_error();
                if matches!(
                    err.raw_os_error(),
                    Some(libc::ENODATA) | Some(libc::ENOTSUP) | Some(libc::EPERM)
                ) {
                    return Ok(None);
                }
                return Err(CepError::internal_error(format!(
                    "failed to read artifact xattr '{name}': {err}"
                )));
            }
            let mut buf = vec![0u8; size as usize];
            let read = libc::lgetxattr(
                path_c.as_ptr(),
                name_c.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            );
            if read < 0 {
                return Err(CepError::internal_error(format!(
                    "failed to read artifact xattr '{name}': {}",
                    std::io::Error::last_os_error()
                )));
            }
            buf.truncate(read as usize);
            Ok(Some(buf))
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        let _ = name;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::super::overlay::OverlayMode;
    use super::super::snapshot::capture_lower_baseline;
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn empty_baseline() -> LowerBaseline {
        LowerBaseline {
            entries: BTreeMap::new(),
        }
    }

    fn regular_manifest(path: &str) -> ArtifactManifest {
        ArtifactManifest {
            entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                path: path.to_string(),
                operation: ArtifactOperation::Create,
                content_hash: "blake3:test".to_string(),
                size_bytes: 4,
                mode: 0o644,
                lower_hash: None,
            })],
        }
    }

    fn hash_content(content: &[u8]) -> String {
        format!("{HASH_PREFIX}{}", blake3::hash(content).to_hex())
    }

    fn successful_artifact_cleanup() -> OverlayCleanupReport {
        OverlayCleanupReport {
            mode: OverlayMode::Artifact,
            disposition: OverlayDisposition::Committed,
            outcome: CleanupOutcome::Succeeded,
            failure_kind: None,
            path_label: None,
            reason: None,
            child_cleanup_succeeded: true,
        }
    }

    fn successful_verify_cleanup() -> VerifyDiscardReport {
        VerifyDiscardReport {
            verify_layer: VerifyLayerStatus::Discarded,
            cleanup_outcome: Some(CleanupOutcome::Succeeded),
            failure_kind: None,
            path_label: None,
            reason: None,
            child_cleanup_succeeded: true,
        }
    }

    fn mutation_region(root: &Path, prefix: &str) -> AuthorizedEffectRegion {
        AuthorizedEffectRegion {
            kinds: BTreeSet::from([ImpactKind::WorkspaceMutation]),
            write_paths: Vec::new(),
            write_path_prefixes: vec![root.join(prefix)],
            read_paths: Vec::new(),
            observation_roots: Vec::new(),
            hosts: Vec::new(),
            session_state_allowed: false,
            command_allowed: false,
        }
    }

    #[test]
    fn artifact_manifest_scans_regular_create_hash_mode_and_order() {
        let upper = TempDir::new().unwrap();
        std::fs::create_dir_all(upper.path().join("b")).unwrap();
        std::fs::write(upper.path().join("z.txt"), "z").unwrap();
        std::fs::write(upper.path().join("a.txt"), "a").unwrap();
        std::fs::write(upper.path().join("b/nested.txt"), "nested").unwrap();

        let report = scan_overlay_upper(upper.path(), &empty_baseline()).unwrap();

        assert!(report.rejected_kinds.is_empty());
        assert_eq!(
            report.manifest.paths(),
            vec!["a.txt", "b", "b/nested.txt", "z.txt"]
        );
        let ArtifactEntry::Regular(entry) = &report.manifest.entries[0] else {
            panic!("expected regular entry");
        };
        assert_eq!(entry.operation, ArtifactOperation::Create);
        assert_eq!(entry.size_bytes, 1);
        assert!(entry.content_hash.starts_with(HASH_PREFIX));
        assert_eq!(entry.mode & 0o777, entry.mode);
    }

    #[test]
    fn artifact_manifest_records_regular_update_lower_hash_and_type_changes() {
        let lower = TempDir::new().unwrap();
        std::fs::write(lower.path().join("updated.txt"), "old").unwrap();
        std::fs::create_dir(lower.path().join("type-change")).unwrap();
        let baseline = capture_lower_baseline(lower.path()).unwrap();

        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("updated.txt"), "new").unwrap();
        std::fs::write(upper.path().join("type-change"), "file").unwrap();

        let report = scan_overlay_upper(upper.path(), &baseline).unwrap();

        let updated = report
            .manifest
            .entries
            .iter()
            .find(|entry| entry.path() == "updated.txt")
            .unwrap();
        let ArtifactEntry::Regular(updated) = updated else {
            panic!("expected regular update");
        };
        assert_eq!(updated.operation, ArtifactOperation::Update);
        assert_eq!(
            updated.lower_hash.as_ref(),
            baseline.entries["updated.txt"].content_hash.as_ref()
        );

        assert_eq!(
            report
                .manifest
                .entries
                .iter()
                .find(|entry| entry.path() == "type-change")
                .unwrap()
                .operation(),
            ArtifactOperation::Update
        );
    }

    #[cfg(unix)]
    #[test]
    fn artifact_manifest_records_directory_and_symlink_creates_without_following_symlink() {
        let upper = TempDir::new().unwrap();
        std::fs::create_dir(upper.path().join("dir")).unwrap();
        symlink("/definitely/not/read", upper.path().join("link")).unwrap();

        let report = scan_overlay_upper(upper.path(), &empty_baseline()).unwrap();

        assert!(matches!(
            &report.manifest.entries[0],
            ArtifactEntry::Directory(DirectoryArtifactEntry {
                path,
                operation: ArtifactOperation::Create,
                ..
            }) if path == "dir"
        ));
        assert!(matches!(
            &report.manifest.entries[1],
            ArtifactEntry::Symlink(SymlinkArtifactEntry {
                path,
                operation: ArtifactOperation::Create,
                target,
            }) if path == "link" && target == "/definitely/not/read"
        ));
    }

    #[test]
    fn artifact_manifest_records_marker_whiteout_delete_with_lower_kind_and_hash() {
        let lower = TempDir::new().unwrap();
        std::fs::write(lower.path().join("deleted.txt"), "delete me").unwrap();
        let baseline = capture_lower_baseline(lower.path()).unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join(".wh.deleted.txt"), "").unwrap();

        let report = scan_overlay_upper(upper.path(), &baseline).unwrap();

        assert_eq!(report.manifest.entries.len(), 1);
        let ArtifactEntry::Delete(entry) = &report.manifest.entries[0] else {
            panic!("expected delete entry");
        };
        assert_eq!(entry.path, "deleted.txt");
        assert_eq!(entry.operation, ArtifactOperation::Delete);
        assert_eq!(entry.lower_kind, Some(ArtifactFileKind::RegularFile));
        assert_eq!(
            entry.lower_hash.as_ref(),
            baseline.entries["deleted.txt"].content_hash.as_ref()
        );
    }

    #[test]
    fn artifact_manifest_records_opaque_marker_deletes_for_lower_descendants() {
        let lower = TempDir::new().unwrap();
        std::fs::create_dir(lower.path().join("dir")).unwrap();
        std::fs::write(lower.path().join("dir/old.txt"), "old").unwrap();
        let baseline = capture_lower_baseline(lower.path()).unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::create_dir(upper.path().join("dir")).unwrap();
        std::fs::write(upper.path().join("dir/.wh..wh..opq"), "").unwrap();

        let report = scan_overlay_upper(upper.path(), &baseline).unwrap();

        assert!(report.manifest.entries.iter().any(
            |entry| matches!(entry, ArtifactEntry::Delete(delete) if delete.path == "dir/old.txt")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_manifest_rejects_fifo_socket_and_hardlink_kinds() {
        use std::os::unix::net::UnixListener;

        let upper = TempDir::new().unwrap();
        let fifo = upper.path().join("pipe");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        unsafe {
            libc::mkfifo(fifo_c.as_ptr(), 0o600);
        }
        let _socket = UnixListener::bind(upper.path().join("sock")).unwrap();
        std::fs::write(upper.path().join("original"), "content").unwrap();
        std::fs::hard_link(upper.path().join("original"), upper.path().join("hardlink")).unwrap();

        let report = scan_overlay_upper(upper.path(), &empty_baseline()).unwrap();

        let rejected = report
            .rejected_kinds
            .iter()
            .map(|rejected| (rejected.path.as_str(), rejected.kind))
            .collect::<BTreeSet<_>>();
        assert!(rejected.contains(&("pipe", UnsupportedArtifactKind::Fifo)));
        assert!(rejected.contains(&("sock", UnsupportedArtifactKind::Socket)));
        assert!(rejected.contains(&("original", UnsupportedArtifactKind::Hardlink)));
        assert!(rejected.contains(&("hardlink", UnsupportedArtifactKind::Hardlink)));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_manifest_classifies_device_without_privileged_fixture() {
        assert_eq!(
            classify_unix_special(false, false, true),
            Some(UnsupportedArtifactKind::Device)
        );
        assert_eq!(
            classify_unix_special(true, false, true),
            Some(UnsupportedArtifactKind::Fifo)
        );
        assert_eq!(
            classify_unix_special(false, true, true),
            Some(UnsupportedArtifactKind::Socket)
        );
    }

    #[cfg(unix)]
    #[test]
    fn artifact_manifest_records_normalized_mode() {
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("exec.sh"), "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(upper.path().join("exec.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o4755);
        std::fs::set_permissions(upper.path().join("exec.sh"), perms).unwrap();

        let report = scan_overlay_upper(upper.path(), &empty_baseline()).unwrap();

        let ArtifactEntry::Regular(entry) = &report.manifest.entries[0] else {
            panic!("expected regular entry");
        };
        assert_eq!(entry.mode, 0o755);
    }

    #[test]
    fn artifact_adjudication_accepts_manifest_with_write_scope_and_effect_region() {
        let project = TempDir::new().unwrap();
        std::fs::create_dir(project.path().join("src")).unwrap();
        let manifest = regular_manifest("src/generated.txt");
        let region = mutation_region(project.path(), "src");

        let report = adjudicate_artifact_manifest(
            &manifest,
            &[],
            project.path().to_str().unwrap(),
            &["src/**".to_string()],
            &region,
        )
        .unwrap();

        assert_eq!(
            report.artifact_manifest_status,
            ArtifactAdjudicationStatus::Accepted
        );
        assert_eq!(report.artifact_adjudication, "accepted");
        assert_eq!(report.observed_effects.len(), 1);
    }

    #[test]
    fn artifact_adjudication_denies_unsupported_kind_before_scope_checks() {
        let project = TempDir::new().unwrap();
        let manifest = regular_manifest("../escape.txt");
        let rejected = vec![RejectedArtifactKind {
            path: "fifo".to_string(),
            kind: UnsupportedArtifactKind::Fifo,
            reason: "fifo artifact".to_string(),
        }];

        let failure = adjudicate_artifact_manifest(
            &manifest,
            &rejected,
            project.path().to_str().unwrap(),
            &[],
            &AuthorizedEffectRegion::empty(),
        )
        .unwrap_err();

        assert_eq!(failure.error.code, CepErrorCode::GuardDenied);
        assert_eq!(
            failure.report.artifact_manifest_status,
            ArtifactAdjudicationStatus::Denied
        );
        assert_eq!(
            failure.report.artifact_adjudication,
            "denied_unsupported_kind"
        );
        assert_eq!(
            failure.report.overlay_disposition,
            OverlayDisposition::Denied
        );
        assert_eq!(failure.report.rejected_kinds, rejected);
    }

    #[test]
    fn artifact_adjudication_denies_path_traversal_and_absolute_escape_as_guard_denied() {
        let project = TempDir::new().unwrap();
        for path in ["../escape.txt", "/tmp/escape.txt"] {
            let failure = adjudicate_artifact_manifest(
                &regular_manifest(path),
                &[],
                project.path().to_str().unwrap(),
                &["src/**".to_string()],
                &AuthorizedEffectRegion::empty(),
            )
            .unwrap_err();

            assert_eq!(failure.error.code, CepErrorCode::GuardDenied);
            assert_eq!(failure.report.artifact_adjudication, "denied_path_escape");
        }
    }

    #[cfg(unix)]
    #[test]
    fn artifact_adjudication_denies_symlink_path_escape_as_guard_denied() {
        let project = TempDir::new().unwrap();
        std::fs::create_dir(project.path().join("src")).unwrap();
        symlink("/tmp", project.path().join("src/link")).unwrap();
        let region = mutation_region(project.path(), "src");

        let failure = adjudicate_artifact_manifest(
            &regular_manifest("src/link"),
            &[],
            project.path().to_str().unwrap(),
            &["src/**".to_string()],
            &region,
        )
        .unwrap_err();

        assert_eq!(failure.error.code, CepErrorCode::GuardDenied);
        assert_eq!(failure.report.artifact_adjudication, "denied_path_escape");
    }

    #[test]
    fn artifact_adjudication_preserves_guard_l1_write_path_exact_and_prefix_behavior() {
        let project = TempDir::new().unwrap();
        std::fs::create_dir(project.path().join("src")).unwrap();
        let exact_region = AuthorizedEffectRegion {
            kinds: BTreeSet::from([ImpactKind::WorkspaceMutation]),
            write_paths: vec![project.path().join("src/exact.txt")],
            write_path_prefixes: Vec::new(),
            read_paths: Vec::new(),
            observation_roots: Vec::new(),
            hosts: Vec::new(),
            session_state_allowed: false,
            command_allowed: false,
        };

        let exact = adjudicate_artifact_manifest(
            &regular_manifest("src/exact.txt"),
            &[],
            project.path().to_str().unwrap(),
            &["src/exact.txt".to_string()],
            &exact_region,
        );
        assert!(exact.is_ok());

        let prefix_region = mutation_region(project.path(), "src");
        let prefix = adjudicate_artifact_manifest(
            &regular_manifest("src/nested/generated.txt"),
            &[],
            project.path().to_str().unwrap(),
            &["src/**".to_string()],
            &prefix_region,
        );
        assert!(prefix.is_ok());
    }

    #[test]
    fn artifact_adjudication_denies_write_path_miss_as_replan() {
        let project = TempDir::new().unwrap();
        std::fs::create_dir(project.path().join("src")).unwrap();
        std::fs::create_dir(project.path().join("outside")).unwrap();
        let region = mutation_region(project.path(), "outside");

        let failure = adjudicate_artifact_manifest(
            &regular_manifest("outside/generated.txt"),
            &[],
            project.path().to_str().unwrap(),
            &["src/**".to_string()],
            &region,
        )
        .unwrap_err();

        assert_eq!(failure.error.code, CepErrorCode::ReplanRequired);
        assert_eq!(failure.report.artifact_adjudication, "denied_write_scope");
        assert_eq!(failure.report.denied_paths, vec!["outside/generated.txt"]);
    }

    #[test]
    fn artifact_adjudication_uses_effect_region_for_every_observed_effect() {
        let project = TempDir::new().unwrap();
        std::fs::create_dir(project.path().join("src")).unwrap();

        let failure = adjudicate_artifact_manifest(
            &regular_manifest("src/generated.txt"),
            &[],
            project.path().to_str().unwrap(),
            &["src/**".to_string()],
            &AuthorizedEffectRegion::empty(),
        )
        .unwrap_err();

        assert_eq!(failure.error.code, CepErrorCode::ReplanRequired);
        assert_eq!(failure.report.artifact_adjudication, "denied_effect_region");
        assert_eq!(failure.report.observed_effects.len(), 1);
        assert!(matches!(
            &failure.report.observed_effects[0].scope,
            ObservedScope::Path(path) if path.ends_with("src/generated.txt")
        ));
    }

    #[test]
    fn artifact_prepare_commit_set_materializes_regular_bytes_and_rechecks_hashes() {
        let project = TempDir::new().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::create_dir_all(upper.path().join("src")).unwrap();
        std::fs::write(upper.path().join("src/generated.txt"), "accepted").unwrap();
        let baseline = capture_lower_baseline(project.path()).unwrap();
        let manifest = ArtifactManifest {
            entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                path: "src/generated.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: hash_content(b"accepted"),
                size_bytes: 8,
                mode: 0o644,
                lower_hash: None,
            })],
        };

        let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();

        assert!(!prepared.staging_root().starts_with(project.path()));
        assert!(!prepared.staging_root().starts_with(upper.path()));
        let PreparedArtifactEntry::Regular(entry) = &prepared.entries[0] else {
            panic!("expected prepared regular entry");
        };
        assert_eq!(std::fs::read(&entry.staged_path).unwrap(), b"accepted");
        assert_eq!(entry.content_hash, hash_content(b"accepted"));
        assert_eq!(entry.mode, 0o644);
    }

    #[test]
    fn artifact_prepare_commit_set_rejects_regular_hash_mismatch_before_commit() {
        let project = TempDir::new().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("generated.txt"), "actual").unwrap();
        let baseline = capture_lower_baseline(project.path()).unwrap();
        let manifest = ArtifactManifest {
            entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                path: "generated.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: hash_content(b"manifest"),
                size_bytes: 6,
                mode: 0o644,
                lower_hash: None,
            })],
        };

        let error = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap_err();

        assert_eq!(error.code, CepErrorCode::ExecutionFailed);
        assert!(error.message.contains("hash mismatch"));
    }

    #[test]
    fn artifact_commit_order_sorts_deletes_dirs_then_files_and_symlinks() {
        let project = TempDir::new().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::create_dir_all(upper.path().join("a/nested")).unwrap();
        std::fs::create_dir_all(upper.path().join("b")).unwrap();
        std::fs::write(upper.path().join("a-file.txt"), "a").unwrap();
        std::fs::write(upper.path().join("z-file.txt"), "z").unwrap();
        #[cfg(unix)]
        symlink("target", upper.path().join("m-link")).unwrap();
        let baseline = capture_lower_baseline(project.path()).unwrap();
        let mut entries = vec![
            ArtifactEntry::Regular(RegularArtifactEntry {
                path: "z-file.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: hash_content(b"z"),
                size_bytes: 1,
                mode: 0o644,
                lower_hash: None,
            }),
            ArtifactEntry::Directory(DirectoryArtifactEntry {
                path: "a/nested".to_string(),
                operation: ArtifactOperation::Create,
                mode: 0o755,
            }),
            ArtifactEntry::Delete(DeleteArtifactEntry {
                path: "old/deep/file.txt".to_string(),
                operation: ArtifactOperation::Delete,
                lower_kind: Some(ArtifactFileKind::RegularFile),
                lower_hash: Some("blake3:old".to_string()),
            }),
            ArtifactEntry::Directory(DirectoryArtifactEntry {
                path: "b".to_string(),
                operation: ArtifactOperation::Create,
                mode: 0o755,
            }),
            ArtifactEntry::Regular(RegularArtifactEntry {
                path: "a-file.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: hash_content(b"a"),
                size_bytes: 1,
                mode: 0o644,
                lower_hash: None,
            }),
            ArtifactEntry::Delete(DeleteArtifactEntry {
                path: "old/file.txt".to_string(),
                operation: ArtifactOperation::Delete,
                lower_kind: Some(ArtifactFileKind::RegularFile),
                lower_hash: Some("blake3:old".to_string()),
            }),
        ];
        #[cfg(unix)]
        entries.push(ArtifactEntry::Symlink(SymlinkArtifactEntry {
            path: "m-link".to_string(),
            operation: ArtifactOperation::Create,
            target: "target".to_string(),
        }));

        let prepared =
            prepare_artifact_commit_set(ArtifactManifest { entries }, upper.path(), &baseline)
                .unwrap();

        let paths = prepared
            .entries
            .iter()
            .map(|entry| (entry.apply_order().label(), entry.path().to_string()))
            .collect::<Vec<_>>();
        let mut expected = vec![
            ("delete_or_whiteout", "old/deep/file.txt".to_string()),
            ("delete_or_whiteout", "old/file.txt".to_string()),
            ("directory_create", "b".to_string()),
            ("directory_create", "a/nested".to_string()),
            ("create_or_update", "a-file.txt".to_string()),
        ];
        #[cfg(unix)]
        expected.push(("create_or_update", "m-link".to_string()));
        expected.push(("create_or_update", "z-file.txt".to_string()));
        assert_eq!(paths, expected);
    }

    #[test]
    fn artifact_prepare_commit_set_drops_staged_content_on_denial_paths() {
        let project = TempDir::new().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
        let baseline = capture_lower_baseline(project.path()).unwrap();
        let manifest = ArtifactManifest {
            entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                path: "generated.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: hash_content(b"accepted"),
                size_bytes: 8,
                mode: 0o644,
                lower_hash: None,
            })],
        };

        let staging_root = {
            let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
            let root = prepared.staging_root().to_path_buf();
            assert!(root.exists());
            root
        };

        assert!(!staging_root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn artifact_prepare_commit_set_uses_manifest_normalized_modes_and_rejects_xattrs() {
        let project = TempDir::new().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("exec.sh"), "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(upper.path().join("exec.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o7755);
        std::fs::set_permissions(upper.path().join("exec.sh"), perms).unwrap();
        let baseline = capture_lower_baseline(project.path()).unwrap();
        let report = scan_overlay_upper(upper.path(), &baseline).unwrap();
        let prepared =
            prepare_artifact_commit_set(report.manifest, upper.path(), &baseline).unwrap();
        let PreparedArtifactEntry::Regular(entry) = &prepared.entries[0] else {
            panic!("expected prepared regular entry");
        };
        assert_eq!(entry.mode, 0o755);
        assert!(!format!("{entry:?}").contains("uid"));
        assert!(!format!("{entry:?}").contains("timestamp"));

        #[cfg(target_os = "linux")]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;

            let xattr_path = upper.path().join("xattr.txt");
            std::fs::write(&xattr_path, "x").unwrap();
            let path_c = CString::new(xattr_path.as_os_str().as_bytes()).unwrap();
            let name_c = CString::new("user.lattice_test").unwrap();
            let value = b"blocked";
            let rc = unsafe {
                libc::lsetxattr(
                    path_c.as_ptr(),
                    name_c.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            };
            if rc == 0 {
                let report = scan_overlay_upper(upper.path(), &baseline).unwrap();
                assert!(report.rejected_kinds.iter().any(|rejected| {
                    rejected.path == "xattr.txt" && rejected.kind == UnsupportedArtifactKind::Xattr
                }));
            }
        }
    }

    #[test]
    fn artifact_commit_lower_drift_blocks_commit_before_workspace_mutation() {
        let project = TempDir::new().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
        let baseline = capture_lower_baseline(project.path()).unwrap();
        let manifest = ArtifactManifest {
            entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                path: "generated.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: hash_content(b"accepted"),
                size_bytes: 8,
                mode: 0o644,
                lower_hash: None,
            })],
        };
        let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
        std::fs::write(project.path().join("external.txt"), "drift").unwrap();

        let failure = commit_artifact_manifest(
            project.path(),
            prepared,
            &successful_artifact_cleanup(),
            &successful_verify_cleanup(),
        )
        .unwrap_err();

        assert_eq!(failure.error.code, CepErrorCode::ReplanRequired);
        assert_eq!(failure.report.commit_outcome, CommitOutcome::Denied);
        assert_eq!(failure.report.lower_consistency, "drift_detected");
        assert_eq!(failure.report.artifact_adjudication, "denied_lower_drift");
        assert_eq!(
            failure.report.overlay_disposition,
            OverlayDisposition::Denied
        );
        assert!(!project.path().join("generated.txt").exists());
        assert_eq!(failure.report.changed_paths, vec!["external.txt"]);
    }

    #[test]
    fn artifact_commit_blocks_cleanup_failures_before_lower_recheck() {
        let project = TempDir::new().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("generated.txt"), "accepted").unwrap();
        let baseline = capture_lower_baseline(project.path()).unwrap();
        let manifest = ArtifactManifest {
            entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                path: "generated.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: hash_content(b"accepted"),
                size_bytes: 8,
                mode: 0o644,
                lower_hash: None,
            })],
        };
        let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
        let mut cleanup = successful_artifact_cleanup();
        cleanup.outcome = CleanupOutcome::Failed;

        let failure = commit_artifact_manifest(
            project.path(),
            prepared,
            &cleanup,
            &successful_verify_cleanup(),
        )
        .unwrap_err();

        assert_eq!(failure.error.code, CepErrorCode::ExecutionFailed);
        assert_eq!(failure.report.commit_outcome, CommitOutcome::NotStarted);
        assert_eq!(failure.report.lower_consistency, "not_checked");
        assert_eq!(
            failure.report.artifact_adjudication,
            "denied_cleanup_failed"
        );
        assert!(!project.path().join("generated.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn artifact_commit_applies_successful_create_update_delete_directory_and_symlink() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("updated.txt"), "old").unwrap();
        std::fs::write(project.path().join("deleted.txt"), "delete").unwrap();
        let baseline = capture_lower_baseline(project.path()).unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("created.txt"), "created").unwrap();
        std::fs::write(upper.path().join("updated.txt"), "new").unwrap();
        let manifest = ArtifactManifest {
            entries: vec![
                ArtifactEntry::Regular(RegularArtifactEntry {
                    path: "created.txt".to_string(),
                    operation: ArtifactOperation::Create,
                    content_hash: hash_content(b"created"),
                    size_bytes: 7,
                    mode: 0o644,
                    lower_hash: None,
                }),
                ArtifactEntry::Regular(RegularArtifactEntry {
                    path: "updated.txt".to_string(),
                    operation: ArtifactOperation::Update,
                    content_hash: hash_content(b"new"),
                    size_bytes: 3,
                    mode: 0o755,
                    lower_hash: baseline.entries["updated.txt"].content_hash.clone(),
                }),
                ArtifactEntry::Delete(DeleteArtifactEntry {
                    path: "deleted.txt".to_string(),
                    operation: ArtifactOperation::Delete,
                    lower_kind: Some(ArtifactFileKind::RegularFile),
                    lower_hash: baseline.entries["deleted.txt"].content_hash.clone(),
                }),
                ArtifactEntry::Directory(DirectoryArtifactEntry {
                    path: "new-dir/nested".to_string(),
                    operation: ArtifactOperation::Create,
                    mode: 0o755,
                }),
                ArtifactEntry::Symlink(SymlinkArtifactEntry {
                    path: "new-link".to_string(),
                    operation: ArtifactOperation::Create,
                    target: "created.txt".to_string(),
                }),
            ],
        };
        let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();

        let report = commit_artifact_manifest(
            project.path(),
            prepared,
            &successful_artifact_cleanup(),
            &successful_verify_cleanup(),
        )
        .unwrap();

        assert_eq!(report.commit_outcome, CommitOutcome::Committed);
        assert_eq!(report.lower_consistency, "unchanged");
        assert_eq!(
            std::fs::read_to_string(project.path().join("created.txt")).unwrap(),
            "created"
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("updated.txt")).unwrap(),
            "new"
        );
        assert!(!project.path().join("deleted.txt").exists());
        assert!(project.path().join("new-dir/nested").is_dir());
        assert_eq!(
            std::fs::read_link(project.path().join("new-link")).unwrap(),
            PathBuf::from("created.txt")
        );
        assert_eq!(report.applied_entries.len(), 5);
    }

    #[test]
    fn artifact_commit_reports_partial_apply_failure_without_clearing_applied_entries() {
        let project = TempDir::new().unwrap();
        std::fs::create_dir(project.path().join("z-blocked")).unwrap();
        let baseline = capture_lower_baseline(project.path()).unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("a-ok.txt"), "ok").unwrap();
        std::fs::write(upper.path().join("z-blocked"), "blocked").unwrap();
        let manifest = ArtifactManifest {
            entries: vec![
                ArtifactEntry::Regular(RegularArtifactEntry {
                    path: "a-ok.txt".to_string(),
                    operation: ArtifactOperation::Create,
                    content_hash: hash_content(b"ok"),
                    size_bytes: 2,
                    mode: 0o644,
                    lower_hash: None,
                }),
                ArtifactEntry::Regular(RegularArtifactEntry {
                    path: "z-blocked".to_string(),
                    operation: ArtifactOperation::Update,
                    content_hash: hash_content(b"blocked"),
                    size_bytes: 7,
                    mode: 0o644,
                    lower_hash: None,
                }),
            ],
        };
        let prepared = prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();

        let failure = commit_artifact_manifest(
            project.path(),
            prepared,
            &successful_artifact_cleanup(),
            &successful_verify_cleanup(),
        )
        .unwrap_err();

        assert_eq!(failure.error.code, CepErrorCode::ExecutionFailed);
        assert_eq!(failure.report.commit_outcome, CommitOutcome::Partial);
        assert_eq!(failure.report.applied_entries.len(), 1);
        assert_eq!(failure.report.applied_entries[0].path, "a-ok.txt");
        let failed = failure.report.failed_entry.expect("failed entry");
        assert_eq!(failed.path, "z-blocked");
        assert_eq!(failed.kind, ArtifactFileKind::RegularFile);
        assert!(!failed.error.is_empty());
        assert_eq!(
            std::fs::read_to_string(project.path().join("a-ok.txt")).unwrap(),
            "ok"
        );
    }

    #[test]
    fn artifact_commit_serializes_concurrent_lower_recheck_and_apply_sections() {
        let observer_guard = artifact_commit_lock().lock().unwrap();
        TEST_COMMIT_SECTION_ACTIVE.store(0, Ordering::SeqCst);
        TEST_COMMIT_SECTION_MAX.store(0, Ordering::SeqCst);
        TEST_COMMIT_SECTION_DELAY.store(true, Ordering::SeqCst);
        drop(observer_guard);

        let cleanup = successful_artifact_cleanup();
        let verify = successful_verify_cleanup();
        let handles = (0..2)
            .map(|index| {
                let project = TempDir::new().unwrap();
                let upper = TempDir::new().unwrap();
                let filename = format!("generated-{index}.txt");
                std::fs::write(upper.path().join(&filename), "accepted").unwrap();
                let baseline = capture_lower_baseline(project.path()).unwrap();
                let manifest = ArtifactManifest {
                    entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                        path: filename,
                        operation: ArtifactOperation::Create,
                        content_hash: hash_content(b"accepted"),
                        size_bytes: 8,
                        mode: 0o644,
                        lower_hash: None,
                    })],
                };
                let prepared =
                    prepare_artifact_commit_set(manifest, upper.path(), &baseline).unwrap();
                let cleanup = cleanup.clone();
                let verify = verify.clone();
                std::thread::spawn(move || {
                    let result =
                        commit_artifact_manifest(project.path(), prepared, &cleanup, &verify);
                    assert!(result.is_ok());
                    project
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            let _project = handle.join().unwrap();
        }
        let observer_guard = artifact_commit_lock().lock().unwrap();
        TEST_COMMIT_SECTION_DELAY.store(false, Ordering::SeqCst);

        assert_eq!(TEST_COMMIT_SECTION_MAX.load(Ordering::SeqCst), 1);
        drop(observer_guard);
    }
}
