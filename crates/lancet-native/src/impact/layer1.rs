use lancet_catalog::catalog;
use lancet_protocol::envelope::{EffectScopeKind, ImpactKind, ImpactScope};

pub struct Layer1Result {
    pub kind: ImpactKind,
    pub scope_kind: EffectScopeKind,
    pub base_scope: Option<ImpactScope>,
}

pub fn derive_layer1(method_name: &str, target: &str) -> Layer1Result {
    let descriptor = catalog()
        .iter()
        .find(|descriptor| descriptor.name == method_name);

    let (kind, scope_kind) = match descriptor {
        Some(d) => (d.impact.clone(), d.effect.scope),
        None => {
            return Layer1Result {
                kind: ImpactKind::WorkspaceMutation,
                scope_kind: EffectScopeKind::WorkspaceWrite,
                base_scope: None,
            };
        }
    };

    let base_scope = scope_kind_to_scope(scope_kind, target);

    Layer1Result {
        kind,
        scope_kind,
        base_scope,
    }
}

fn scope_kind_to_scope(kind: EffectScopeKind, target: &str) -> Option<ImpactScope> {
    match kind {
        EffectScopeKind::PathRead | EffectScopeKind::WorkspaceWrite => {
            if is_single_file(target) {
                Some(ImpactScope::ExactFile(std::path::PathBuf::from(target)))
            } else {
                Some(ImpactScope::Directory(std::path::PathBuf::from(target)))
            }
        }
        EffectScopeKind::ObservationRoot | EffectScopeKind::CommandExecution => {
            Some(ImpactScope::Directory(std::path::PathBuf::from(target)))
        }
        EffectScopeKind::ExternalHost => Some(ImpactScope::Host(target.to_string())),
        EffectScopeKind::SessionState => Some(ImpactScope::SessionState),
    }
}

fn is_single_file(target: &str) -> bool {
    !target.ends_with('/') && !target.contains('*') && target.contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layer1_create_single_path() {
        let result = derive_layer1("mutate.create", "src/main.rs");
        assert_eq!(result.kind, ImpactKind::WorkspaceMutation);
        assert!(matches!(result.base_scope, Some(ImpactScope::ExactFile(_))));
    }

    #[test]
    fn test_layer1_grep_directory() {
        let result = derive_layer1("observe.workspace", "src/");
        assert_eq!(result.kind, ImpactKind::WorkspaceObservation);
        assert!(matches!(result.base_scope, Some(ImpactScope::Directory(_))));
    }

    #[test]
    fn test_layer1_directory_with_dot_suffix_stays_directory() {
        let result = derive_layer1("mutate.create", "config.d/");
        assert!(matches!(result.base_scope, Some(ImpactScope::Directory(_))));
    }

    #[test]
    fn test_layer1_unknown_method_is_mutation() {
        let result = derive_layer1("unknown.method", "foo");
        assert_eq!(result.kind, ImpactKind::WorkspaceMutation);
    }

    #[test]
    fn test_layer1_read_file_is_observation() {
        let result = derive_layer1("observe.workspace", "src/lib.rs");
        assert_eq!(result.kind, ImpactKind::WorkspaceObservation);
        assert!(matches!(result.base_scope, Some(ImpactScope::Directory(_))));
    }

    #[test]
    fn test_layer1_fetch_is_host_scope() {
        let result = derive_layer1("fetch.web", "example.com");
        assert_eq!(result.kind, ImpactKind::ExternalObservation);
        assert!(matches!(result.base_scope, Some(ImpactScope::Host(_))));
    }
}
