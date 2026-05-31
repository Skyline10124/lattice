use crate::impact::layer1::Layer1Result;
use cep_protocol::envelope::{ImpactConfidence, ImpactScope, ImpactUnknown};
use std::path::Path;

pub struct Layer2Result {
    pub scope: ImpactScope,
    pub confidence: ImpactConfidence,
    pub unknowns: Vec<ImpactUnknown>,
}

pub fn derive_layer2(
    base: &Layer1Result,
    target: &str,
    _params: Option<&serde_json::Value>,
) -> Layer2Result {
    match &base.base_scope {
        Some(ImpactScope::Host(_) | ImpactScope::SessionState) => Layer2Result {
            scope: base.base_scope.clone().unwrap(),
            confidence: ImpactConfidence::Complete,
            unknowns: vec![],
        },
        _ => resolve_path_scope(target),
    }
}

fn resolve_path_scope(target: &str) -> Layer2Result {
    let has_glob = target.contains('*') || target.contains("**");
    let has_ext = target.contains('.') && !target.ends_with('/') && !target.ends_with('.');
    let has_dir_sep = target.contains('/');

    if has_glob {
        let dir = nearest_directory(target);
        Layer2Result {
            scope: ImpactScope::Directory(dir),
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }
    } else if has_ext {
        Layer2Result {
            scope: ImpactScope::ExactFile(Path::new(target).to_path_buf()),
            confidence: ImpactConfidence::Complete,
            unknowns: vec![],
        }
    } else if has_dir_sep {
        Layer2Result {
            scope: ImpactScope::Directory(Path::new(target).to_path_buf()),
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }
    } else {
        Layer2Result {
            scope: ImpactScope::Directory(std::path::PathBuf::new()),
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![ImpactUnknown {
                kind: "unresolvable_target".to_string(),
                description: format!("Cannot determine scope for target: {target}"),
                location: None,
            }],
        }
    }
}

fn nearest_directory(target: &str) -> std::path::PathBuf {
    let clean = target
        .trim_end_matches('/')
        .trim_end_matches("**")
        .trim_end_matches('*')
        .trim_end_matches('/');
    let parent = Path::new(clean).parent().unwrap_or_else(|| Path::new(""));
    if parent.as_os_str().is_empty() {
        std::path::PathBuf::new()
    } else {
        parent.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cep_protocol::envelope::{EffectScopeKind, ImpactKind};

    #[test]
    fn test_layer2_exact_file() {
        let base = Layer1Result {
            kind: ImpactKind::WorkspaceMutation,
            scope_kind: EffectScopeKind::WorkspaceWrite,
            base_scope: Some(ImpactScope::ExactFile(std::path::PathBuf::from(
                "src/main.rs",
            ))),
        };
        let result = derive_layer2(&base, "src/main.rs", None);
        assert_eq!(result.confidence, ImpactConfidence::Complete);
    }

    #[test]
    fn test_layer2_glob_target_conservative() {
        let base = Layer1Result {
            kind: ImpactKind::WorkspaceObservation,
            scope_kind: EffectScopeKind::PathRead,
            base_scope: Some(ImpactScope::Directory(std::path::PathBuf::from(
                "src/**/*.rs",
            ))),
        };
        let result = derive_layer2(&base, "src/**/*.rs", None);
        assert_eq!(result.confidence, ImpactConfidence::Conservative);
    }

    #[test]
    fn test_layer2_unresolvable_target_has_unknown() {
        let base = Layer1Result {
            kind: ImpactKind::WorkspaceMutation,
            scope_kind: EffectScopeKind::WorkspaceWrite,
            base_scope: None,
        };
        let result = derive_layer2(&base, "unresolvable", None);
        assert_eq!(result.confidence, ImpactConfidence::Conservative);
        assert!(!result.unknowns.is_empty());
        assert_eq!(result.unknowns[0].kind, "unresolvable_target");
    }

    #[test]
    fn test_layer2_host_scope_is_complete() {
        let base = Layer1Result {
            kind: ImpactKind::ExternalObservation,
            scope_kind: EffectScopeKind::ExternalHost,
            base_scope: Some(ImpactScope::Host("example.com".to_string())),
        };
        let result = derive_layer2(&base, "example.com", None);
        assert_eq!(result.confidence, ImpactConfidence::Complete);
    }
}
