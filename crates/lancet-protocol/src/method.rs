//! CEP method trait and sealed catalog membership.

use crate::envelope::{Capability, EffectDecl, ImpactKind};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Required capability class for a catalog method.
///
/// Maps to a full [`Capability`] via [`RequiredCapability::resolve`]. Catalog
/// methods declare the capability *kind* as a const; concrete path / argv /
/// host patterns are resolved at envelope scope in later phases. There is **no
/// permissive default** — `resolve` requires explicit patterns for every
/// pattern-bearing capability so that "no ambient authority" (CEP-07) is
/// preserved at the type system level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequiredCapability {
    Inspect,
    ReadPath,
    WritePath,
    RunCommand,
    FetchUrl,
    MutateSessionState,
    ReadAudit,
}

/// Patterns supplied at envelope construction time to materialize a concrete
/// [`Capability`] from a [`RequiredCapability`] kind.
///
/// Only the patterns relevant to the requested kind need to be populated;
/// missing patterns produce [`CapabilityResolveError::MissingPattern`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapabilityPatterns<'a> {
    pub path_pattern: Option<&'a str>,
    pub argv_allowlist: Option<&'a [String]>,
    pub host: Option<&'a str>,
}

/// Reasons [`RequiredCapability::resolve`] cannot materialize a [`Capability`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityResolveError {
    /// The required pattern (path / argv / host) was not provided.
    MissingPattern { kind: &'static str },
}

impl std::fmt::Display for CapabilityResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingPattern { kind } => write!(
                f,
                "missing capability pattern for `{kind}` — refusing to materialize a wide-open default"
            ),
        }
    }
}

impl std::error::Error for CapabilityResolveError {}

impl RequiredCapability {
    /// Materialize the catalog-required capability with **explicit** patterns.
    ///
    /// Refuses to default to wide-open patterns (`**`, `*`, `["*"]`) — that
    /// would silently grant ambient authority. Pattern-bearing capabilities
    /// (`ReadPath`, `WritePath`, `RunCommand`, `FetchUrl`) require their
    /// pattern in [`CapabilityPatterns`]; pattern-less capabilities
    /// (`Inspect`, `MutateSessionState`) ignore the patterns argument.
    pub fn resolve(
        self,
        patterns: &CapabilityPatterns<'_>,
    ) -> Result<Capability, CapabilityResolveError> {
        match self {
            Self::Inspect => Ok(Capability::Inspect),
            Self::MutateSessionState => Ok(Capability::MutateSessionState),
            Self::ReadAudit => Ok(Capability::ReadAudit),
            Self::ReadPath => Ok(Capability::ReadPath {
                path_pattern: patterns
                    .path_pattern
                    .ok_or(CapabilityResolveError::MissingPattern { kind: "ReadPath" })?
                    .to_string(),
            }),
            Self::WritePath => Ok(Capability::WritePath {
                path_pattern: patterns
                    .path_pattern
                    .ok_or(CapabilityResolveError::MissingPattern { kind: "WritePath" })?
                    .to_string(),
            }),
            Self::RunCommand => Ok(Capability::RunCommand {
                argv_allowlist: patterns
                    .argv_allowlist
                    .ok_or(CapabilityResolveError::MissingPattern { kind: "RunCommand" })?
                    .to_vec(),
            }),
            Self::FetchUrl => Ok(Capability::FetchUrl {
                host: patterns
                    .host
                    .ok_or(CapabilityResolveError::MissingPattern { kind: "FetchUrl" })?
                    .to_string(),
            }),
        }
    }
}

/// Typed CEP method contract.
pub trait CepMethod: 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    type Params: Serialize + DeserializeOwned + JsonSchema;
    type Result: Serialize + DeserializeOwned + JsonSchema;
    const IMPACT: ImpactKind;
    const CAPABILITY: RequiredCapability;
    const EFFECT: EffectDecl;
}

/// Sealed catalog membership — only types in `crate::methods` may implement this.
pub trait CatalogMethod: CepMethod + sealed::Sealed {}

/// Static descriptor for catalog projection and registry metadata.
#[derive(Clone, Debug)]
pub struct MethodDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub impact: ImpactKind,
    pub capability: RequiredCapability,
    pub effect: EffectDecl,
    pub params_schema: fn() -> serde_json::Value,
}

impl MethodDescriptor {
    /// Build a descriptor from a catalog method type.
    pub fn for_method<M: CatalogMethod>() -> Self {
        Self {
            name: M::NAME,
            description: M::DESCRIPTION,
            impact: M::IMPACT,
            capability: M::CAPABILITY,
            effect: M::EFFECT,
            params_schema: params_schema::<M>,
        }
    }
}

fn params_schema<M: CepMethod>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(M::Params))
        .expect("schemars schema serializes to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_less_kinds_resolve_without_patterns() {
        let empty = CapabilityPatterns::default();
        assert_eq!(
            RequiredCapability::Inspect.resolve(&empty).unwrap(),
            Capability::Inspect
        );
        assert_eq!(
            RequiredCapability::MutateSessionState
                .resolve(&empty)
                .unwrap(),
            Capability::MutateSessionState
        );
        assert_eq!(
            RequiredCapability::ReadAudit.resolve(&empty).unwrap(),
            Capability::ReadAudit
        );
    }

    #[test]
    fn pattern_bearing_kinds_require_explicit_patterns() {
        let empty = CapabilityPatterns::default();
        for kind in [
            RequiredCapability::ReadPath,
            RequiredCapability::WritePath,
            RequiredCapability::RunCommand,
            RequiredCapability::FetchUrl,
        ] {
            assert!(
                matches!(
                    kind.resolve(&empty),
                    Err(CapabilityResolveError::MissingPattern { .. })
                ),
                "{kind:?} must refuse to default"
            );
        }
    }

    #[test]
    fn read_path_resolves_with_explicit_pattern() {
        let patterns = CapabilityPatterns {
            path_pattern: Some("/workspace/**"),
            ..Default::default()
        };
        assert_eq!(
            RequiredCapability::ReadPath.resolve(&patterns).unwrap(),
            Capability::ReadPath {
                path_pattern: "/workspace/**".into()
            }
        );
    }

    #[test]
    fn run_command_resolves_with_argv_allowlist() {
        let argv = vec!["cargo".to_string(), "test".to_string()];
        let patterns = CapabilityPatterns {
            argv_allowlist: Some(&argv),
            ..Default::default()
        };
        let resolved = RequiredCapability::RunCommand.resolve(&patterns).unwrap();
        assert_eq!(
            resolved,
            Capability::RunCommand {
                argv_allowlist: argv.clone()
            }
        );
    }

    #[test]
    fn fetch_url_resolves_with_host() {
        let patterns = CapabilityPatterns {
            host: Some("api.github.com"),
            ..Default::default()
        };
        assert_eq!(
            RequiredCapability::FetchUrl.resolve(&patterns).unwrap(),
            Capability::FetchUrl {
                host: "api.github.com".into()
            }
        );
    }

    #[test]
    fn missing_pattern_error_names_the_kind() {
        let empty = CapabilityPatterns::default();
        match RequiredCapability::WritePath.resolve(&empty) {
            Err(CapabilityResolveError::MissingPattern { kind }) => assert_eq!(kind, "WritePath"),
            _ => panic!("expected MissingPattern for WritePath"),
        }
    }
}
