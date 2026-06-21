// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Explicit DTO bridge between native [`ExecutionBackend`] and [`Workspace`].

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lancet_native::{
    DirectoryEntry as NativeDirectoryEntry, ExecOutput as NativeExecOutput,
    ExecRequest as NativeExecRequest, ExecutionBackend, ExecutionBackendError,
    FindRequest as NativeFindRequest, GrepMatch as NativeGrepMatch,
    GrepRequest as NativeGrepRequest, PathFacts as NativePathFacts, PathIntent as NativePathIntent,
    PathKind as NativePathKind, UpperContext, WorkspacePath as NativeWorkspacePath,
};
use lattice_workspace::{
    DirectoryEntry, ExecOutput, ExecRequest, FindRequest, GrepMatch, GrepRequest, PathFacts,
    PathIntent, PathKind, UpperHandle, Workspace, WorkspaceError, WorkspacePath,
    WorkspacePathError as WsPathError,
};

/// Bridges one immutable [`UpperHandle`] through a [`Workspace`] adapter.
pub struct WorkspaceExecutionBackend {
    workspace: Arc<dyn Workspace>,
    handle: UpperHandle,
    upper_context: UpperContext,
}

impl WorkspaceExecutionBackend {
    pub fn new(
        workspace: Arc<dyn Workspace>,
        handle: UpperHandle,
        upper_context: UpperContext,
    ) -> Self {
        Self {
            workspace,
            handle,
            upper_context,
        }
    }

    pub fn upper_context(&self) -> &UpperContext {
        &self.upper_context
    }

    fn convert_path_error(err: WsPathError) -> lancet_native::WorkspacePathError {
        use lancet_native::WorkspacePathError as NativePathError;
        match err {
            WsPathError::Empty => NativePathError::Empty,
            WsPathError::NulByte => NativePathError::NulByte,
            WsPathError::ParentTraversal => NativePathError::ParentTraversal,
            WsPathError::Absolute => NativePathError::Absolute,
            WsPathError::UpperRootAlias => NativePathError::UpperRootAlias,
            WsPathError::InvalidUtf8 => NativePathError::InvalidUtf8,
            WsPathError::ProjectRootNotAllowedForFileIntent => {
                NativePathError::ProjectRootNotAllowedForFileIntent
            }
        }
    }

    fn convert_path(path: &NativeWorkspacePath) -> Result<WorkspacePath, ExecutionBackendError> {
        WorkspacePath::parse(path.as_str())
            .map_err(|e| ExecutionBackendError::Path(Self::convert_path_error(e)))
    }

    fn convert_path_out(
        path: &WorkspacePath,
    ) -> Result<NativeWorkspacePath, ExecutionBackendError> {
        NativeWorkspacePath::parse(path.as_str()).map_err(|e| ExecutionBackendError::Path(e))
    }

    fn convert_intent(intent: NativePathIntent) -> PathIntent {
        match intent {
            NativePathIntent::Read => PathIntent::Read,
            NativePathIntent::Create => PathIntent::Create,
            NativePathIntent::Overwrite => PathIntent::Overwrite,
            NativePathIntent::Delete => PathIntent::Delete,
            NativePathIntent::ListDirectory => PathIntent::ListDirectory,
            NativePathIntent::ExecCwd => PathIntent::ExecCwd,
        }
    }

    fn convert_kind(kind: PathKind) -> NativePathKind {
        match kind {
            PathKind::File => NativePathKind::File,
            PathKind::Directory => NativePathKind::Directory,
            PathKind::Symlink => NativePathKind::Symlink,
        }
    }

    fn convert_kind_opt(kind: Option<PathKind>) -> Option<NativePathKind> {
        kind.map(Self::convert_kind)
    }

    fn convert_facts(facts: PathFacts) -> NativePathFacts {
        NativePathFacts {
            normalized_path: facts.normalized_path,
            exists: facts.exists,
            kind: Self::convert_kind_opt(facts.kind),
            parent_exists: facts.parent_exists,
            parent_kind: Self::convert_kind_opt(facts.parent_kind),
            byte_size: facts.byte_size,
            canonical_under_workspace: facts.canonical_under_workspace,
        }
    }

    fn convert_entry(entry: DirectoryEntry) -> NativeDirectoryEntry {
        NativeDirectoryEntry {
            name: entry.name,
            kind: Self::convert_kind(entry.kind),
        }
    }

    fn convert_grep_request(
        request: &NativeGrepRequest,
    ) -> Result<GrepRequest, ExecutionBackendError> {
        Ok(GrepRequest {
            pattern: request.pattern.clone(),
            path: Self::convert_path(&request.path)?,
            glob: request.glob.clone(),
            max_results: lattice_workspace::DEFAULT_GREP_MAX_RESULTS,
        })
    }

    fn convert_find_request(
        request: &NativeFindRequest,
    ) -> Result<FindRequest, ExecutionBackendError> {
        Ok(FindRequest {
            pattern: request.pattern.clone(),
            path: Self::convert_path(&request.path)?,
            max_results: lattice_workspace::DEFAULT_FIND_MAX_RESULTS,
        })
    }

    fn convert_exec_request(
        request: &NativeExecRequest,
    ) -> Result<ExecRequest, ExecutionBackendError> {
        ExecRequest::new(
            request.argv.clone(),
            request.env.clone(),
            request.timeout_ms,
            Self::convert_path(&request.cwd)?,
        )
        .map_err(|_| ExecutionBackendError::ExecutionFailed {
            message: "empty exec argv".into(),
        })
    }

    fn convert_exec_output(output: ExecOutput) -> NativeExecOutput {
        NativeExecOutput {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }

    fn map_error(err: WorkspaceError) -> ExecutionBackendError {
        match err {
            WorkspaceError::InvalidPath(_) => ExecutionBackendError::ExecutionFailed {
                message: "invalid workspace path".into(),
            },
            WorkspaceError::ContentTooLarge { .. } => ExecutionBackendError::ContentTooLarge,
            WorkspaceError::NotFound { .. } => ExecutionBackendError::ExecutionFailed {
                message: "path not found".into(),
            },
            WorkspaceError::WrongKind { .. } => ExecutionBackendError::ExecutionFailed {
                message: "wrong path kind".into(),
            },
            WorkspaceError::PathEscape { .. } => ExecutionBackendError::ExecutionFailed {
                message: "path escape".into(),
            },
            WorkspaceError::Unavailable(msg) => ExecutionBackendError::Unavailable { message: msg },
            other => ExecutionBackendError::ExecutionFailed {
                message: other.to_string(),
            },
        }
    }
}

impl ExecutionBackend for WorkspaceExecutionBackend {
    fn backend_name(&self) -> &str {
        self.handle.backend.as_str()
    }

    fn resolve_path(
        &self,
        path: &NativeWorkspacePath,
        intent: NativePathIntent,
    ) -> Pin<Box<dyn Future<Output = Result<NativePathFacts, ExecutionBackendError>> + Send + '_>>
    {
        let path = match Self::convert_path(path) {
            Ok(p) => p,
            Err(err) => return Box::pin(async move { Err(err) }),
        };
        let workspace = Arc::clone(&self.workspace);
        let handle = self.handle.clone();
        Box::pin(async move {
            let facts = workspace
                .resolve_path(&handle, &path, Self::convert_intent(intent))
                .await
                .map_err(Self::map_error)?;
            Ok(Self::convert_facts(facts))
        })
    }

    fn read_file(
        &self,
        path: &NativeWorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>> {
        let path = match Self::convert_path(path) {
            Ok(p) => p,
            Err(err) => return Box::pin(async move { Err(err) }),
        };
        let workspace = Arc::clone(&self.workspace);
        let handle = self.handle.clone();
        Box::pin(async move {
            workspace
                .read_file(&handle, &path)
                .await
                .map_err(Self::map_error)
        })
    }

    fn write_file(
        &self,
        path: &NativeWorkspacePath,
        content: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
        let path = match Self::convert_path(path) {
            Ok(p) => p,
            Err(err) => return Box::pin(async move { Err(err) }),
        };
        let content = content.to_vec();
        let workspace = Arc::clone(&self.workspace);
        let handle = self.handle.clone();
        Box::pin(async move {
            workspace
                .write_file(&handle, &path, &content)
                .await
                .map_err(Self::map_error)
        })
    }

    fn remove_file(
        &self,
        path: &NativeWorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
        let path = match Self::convert_path(path) {
            Ok(p) => p,
            Err(err) => return Box::pin(async move { Err(err) }),
        };
        let workspace = Arc::clone(&self.workspace);
        let handle = self.handle.clone();
        Box::pin(async move {
            workspace
                .remove_file(&handle, &path)
                .await
                .map_err(Self::map_error)
        })
    }

    fn list_dir(
        &self,
        path: &NativeWorkspacePath,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<NativeDirectoryEntry>, ExecutionBackendError>>
                + Send
                + '_,
        >,
    > {
        let path = match Self::convert_path(path) {
            Ok(p) => p,
            Err(err) => return Box::pin(async move { Err(err) }),
        };
        let workspace = Arc::clone(&self.workspace);
        let handle = self.handle.clone();
        Box::pin(async move {
            let entries = workspace
                .list_dir(&handle, &path)
                .await
                .map_err(Self::map_error)?;
            Ok(entries.into_iter().map(Self::convert_entry).collect())
        })
    }

    fn grep(
        &self,
        request: &NativeGrepRequest,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<NativeGrepMatch>, ExecutionBackendError>> + Send + '_>,
    > {
        let request = match Self::convert_grep_request(request) {
            Ok(r) => r,
            Err(err) => return Box::pin(async move { Err(err) }),
        };
        let workspace = Arc::clone(&self.workspace);
        let handle = self.handle.clone();
        Box::pin(async move {
            let matches = workspace
                .grep(&handle, &request)
                .await
                .map_err(Self::map_error)?;
            let mut out = Vec::with_capacity(matches.len());
            for m in matches {
                out.push(NativeGrepMatch {
                    path: Self::convert_path_out(&m.path)?,
                    line_number: m.line_number,
                    line: m.line,
                });
            }
            Ok(out)
        })
    }

    fn find_files(
        &self,
        request: &NativeFindRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<NativeWorkspacePath>, ExecutionBackendError>>
                + Send
                + '_,
        >,
    > {
        let request = match Self::convert_find_request(request) {
            Ok(r) => r,
            Err(err) => return Box::pin(async move { Err(err) }),
        };
        let workspace = Arc::clone(&self.workspace);
        let handle = self.handle.clone();
        Box::pin(async move {
            let paths = workspace
                .find_files(&handle, &request)
                .await
                .map_err(Self::map_error)?;
            let mut out = Vec::with_capacity(paths.len());
            for p in paths {
                out.push(Self::convert_path_out(&p)?);
            }
            Ok(out)
        })
    }

    fn exec(
        &self,
        request: &NativeExecRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NativeExecOutput, ExecutionBackendError>> + Send + '_>>
    {
        let request = match Self::convert_exec_request(request) {
            Ok(r) => r,
            Err(err) => return Box::pin(async move { Err(err) }),
        };
        let workspace = Arc::clone(&self.workspace);
        let handle = self.handle.clone();
        Box::pin(async move {
            let output = workspace
                .exec(&handle, &request)
                .await
                .map_err(Self::map_error)?;
            Ok(Self::convert_exec_output(output))
        })
    }

    fn changed_paths(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<NativeWorkspacePath>, ExecutionBackendError>>
                + Send
                + '_,
        >,
    > {
        let workspace = Arc::clone(&self.workspace);
        let handle = self.handle.clone();
        Box::pin(async move {
            let status = workspace
                .git_status(&handle)
                .await
                .map_err(Self::map_error)?;
            let mut out = Vec::new();
            for path_str in parse_porcelain_changed_paths(&status.porcelain) {
                let path = NativeWorkspacePath::parse(&path_str).map_err(|err| {
                    ExecutionBackendError::ExecutionFailed {
                        message: format!(
                            "changed_paths: unparseable porcelain path {:?}: {}",
                            path_str, err
                        ),
                    }
                })?;
                out.push(path);
            }
            Ok(out)
        })
    }
}

fn parse_porcelain_changed_paths(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            if line.len() < 3 {
                return None;
            }
            let rest = line[2..].trim_start();
            if rest.is_empty() {
                return None;
            }
            let path = if let Some((_, new_path)) = rest.split_once(" -> ") {
                new_path.trim()
            } else {
                rest
            };
            Some(path.to_string())
        })
        .collect()
}

#[cfg(test)]
mod workspace_execution_backend_tests {
    use super::*;
    use std::collections::BTreeMap;

    use lattice_workspace::{Backend, UpperSpec};

    #[derive(Default)]
    struct SpyWorkspace {
        records: Mutex<Vec<String>>,
        git_status_porcelain: Mutex<String>,
    }

    #[async_trait]
    impl Workspace for SpyWorkspace {
        async fn create_upper(&self, _spec: &UpperSpec) -> lattice_workspace::Result<UpperHandle> {
            Err(WorkspaceError::Unavailable("spy".into()))
        }

        async fn exec_upper(
            &self,
            _handle: &UpperHandle,
            _prog: &str,
            _args: &[&str],
        ) -> lattice_workspace::Result<ExecOutput> {
            Err(WorkspaceError::Unavailable("spy".into()))
        }

        async fn git_status(
            &self,
            _handle: &UpperHandle,
        ) -> lattice_workspace::Result<lattice_workspace::GitStatus> {
            Ok(lattice_workspace::GitStatus {
                porcelain: self.git_status_porcelain.lock().unwrap().clone(),
                is_clean: false,
            })
        }

        async fn template_create(
            &self,
            _source: &UpperHandle,
            _name: &str,
        ) -> lattice_workspace::Result<String> {
            Err(WorkspaceError::Unavailable("spy".into()))
        }

        async fn restore_upper(
            &self,
            _template_name: &str,
            _new_name: &str,
            _spec: &UpperSpec,
        ) -> lattice_workspace::Result<UpperHandle> {
            Err(WorkspaceError::Unavailable("spy".into()))
        }

        async fn destroy_upper(&self, _handle: &UpperHandle) -> lattice_workspace::Result<()> {
            Ok(())
        }

        async fn exec(
            &self,
            handle: &UpperHandle,
            request: &ExecRequest,
        ) -> lattice_workspace::Result<ExecOutput> {
            self.records.lock().unwrap().push(format!(
                "exec:{}:{:?}:{:?}:{}",
                handle.id,
                request.argv,
                request.env,
                request.cwd.as_str()
            ));
            Ok(ExecOutput {
                exit_code: 0,
                stdout: "ok".into(),
                stderr: String::new(),
            })
        }

        async fn export_candidate(
            &self,
            _handle: &UpperHandle,
            _baseline_oid: &str,
            _max_artifact_bytes: u64,
        ) -> lattice_workspace::Result<lattice_workspace::CandidateExport> {
            Err(WorkspaceError::Unavailable("spy".into()))
        }
    }

    #[test]
    fn parse_porcelain_changed_paths_handles_renames() {
        let porcelain = " M src/lib.rs\n?? tests/new.rs\nR  old.rs -> new.rs\n";
        let paths = super::parse_porcelain_changed_paths(porcelain);
        assert_eq!(paths, vec!["src/lib.rs", "tests/new.rs", "new.rs"]);
    }

    #[tokio::test]
    async fn workspace_execution_backend_changed_paths_from_git_status() {
        let spy = Arc::new(SpyWorkspace::default());
        *spy.git_status_porcelain.lock().unwrap() =
            " M src/lib.rs\n?? tests/new.rs\n".to_string();
        let handle = UpperHandle {
            id: "upper-test".into(),
            backend: Backend::Deterministic,
        };
        let backend = WorkspaceExecutionBackend::new(
            spy.clone(),
            handle,
            UpperContext {
                execution_backend: "deterministic".into(),
                upper_workspace_id: "upper-test".into(),
            },
        );
        let paths = backend.changed_paths().await.unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].as_str(), "src/lib.rs");
        assert_eq!(paths[1].as_str(), "tests/new.rs");
    }

    #[tokio::test]
    async fn workspace_execution_backend_preserves_exec_request() {
        let spy = Arc::new(SpyWorkspace::default());
        let handle = UpperHandle {
            id: "upper-test".into(),
            backend: Backend::Deterministic,
        };
        let backend = WorkspaceExecutionBackend::new(
            spy.clone(),
            handle,
            UpperContext {
                execution_backend: "deterministic".into(),
                upper_workspace_id: "upper-test".into(),
            },
        );
        let cwd = NativeWorkspacePath::parse("src").unwrap();
        let mut env = BTreeMap::new();
        env.insert("MODE".into(), "test".into());
        let request =
            NativeExecRequest::new(vec!["echo".into(), "hi".into()], env, 1_000, cwd).unwrap();
        let out = backend.exec(&request).await.unwrap();
        assert_eq!(out.stdout, "ok");
        let record = spy.records.lock().unwrap().pop().unwrap();
        assert!(record.contains("echo"));
        assert!(record.contains("hi"));
        assert!(record.contains("MODE"));
        assert!(record.contains("src"));
        assert!(!record.contains("/workspace"));
        assert!(!record.contains("/home"));
    }

    #[tokio::test]
    async fn workspace_execution_backend_maps_content_too_large() {
        let err = WorkspaceExecutionBackend::map_error(WorkspaceError::ContentTooLarge {
            actual: 9,
            max: 1,
        });
        assert!(matches!(err, ExecutionBackendError::ContentTooLarge));
        let debug = format!("{err:?}");
        assert!(!debug.contains("/workspace"));
    }
}
