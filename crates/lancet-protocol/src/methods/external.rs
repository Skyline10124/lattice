//! External observation methods.

use crate::envelope::{EffectDecl, EffectKindSet, EffectScopeKind, ImpactKind};
use crate::method::{CatalogMethod, CepMethod, RequiredCapability};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `web.fetch`
pub struct Fetch;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FetchParams {
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FetchResult {
    pub status: u16,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

impl CepMethod for Fetch {
    const NAME: &'static str = "web.fetch";
    const DESCRIPTION: &'static str = "Fetch a URL and return its text content.";
    type Params = FetchParams;
    type Result = FetchResult;
    const IMPACT: ImpactKind = ImpactKind::ExternalObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::FetchUrl;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::ExternalObservation]),
        scope: EffectScopeKind::ExternalHost,
        capabilities: &[RequiredCapability::FetchUrl],
    };
}

impl CatalogMethod for Fetch {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_fetch_name_is_dotted() {
        assert_eq!(Fetch::NAME, "web.fetch");
        assert!(Fetch::NAME.contains('.'));
        assert!(!Fetch::NAME.contains('/'));
    }

    #[test]
    fn fetch_result_has_additive_truncated_field() {
        let value = serde_json::json!({
            "status": 404,
            "body": "not found",
            "content_type": "text/plain",
            "truncated": true
        });
        let parsed: FetchResult = serde_json::from_value(value).expect("deserialize FetchResult");
        assert_eq!(parsed.status, 404);
        assert_eq!(parsed.truncated, Some(true));
        let legacy = serde_json::json!({ "status": 200, "body": "ok" });
        let without: FetchResult = serde_json::from_value(legacy).expect("legacy shape");
        assert_eq!(without.truncated, None);
    }
}
