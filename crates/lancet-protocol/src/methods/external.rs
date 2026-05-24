//! External observation methods.

use crate::envelope::ImpactKind;
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
}

impl CepMethod for Fetch {
    const NAME: &'static str = "web.fetch";
    const DESCRIPTION: &'static str = "Fetch a URL and return its text content.";
    type Params = FetchParams;
    type Result = FetchResult;
    const IMPACT: ImpactKind = ImpactKind::ExternalObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::FetchUrl;
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
}
