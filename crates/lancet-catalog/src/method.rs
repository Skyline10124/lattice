// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP method trait and sealed catalog membership.

use lancet_protocol::{EffectDecl, ImpactKind};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use lancet_protocol::{CapabilityPatterns, CapabilityResolveError, RequiredCapability};

pub(crate) mod sealed {
    pub trait Sealed {}
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
    const BASE_AUTHORITY: BaseAuthorityRequirement = BaseAuthorityRequirement::NONE;
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
    pub base_authority: BaseAuthorityRequirement,
    pub params_schema: fn() -> serde_json::Value,
}

/// Base-envelope authority a method needs when it is exposed for a turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BaseAuthorityRequirement {
    pub session_state: bool,
    pub fetch_hosts: bool,
    pub read_audit: bool,
}

impl BaseAuthorityRequirement {
    pub const NONE: Self = Self::new(false, false, false);
    pub const SESSION_STATE: Self = Self::new(true, false, false);
    pub const FETCH_HOSTS: Self = Self::new(false, true, false);
    pub const READ_AUDIT: Self = Self::new(false, false, true);

    pub const fn new(session_state: bool, fetch_hosts: bool, read_audit: bool) -> Self {
        Self {
            session_state,
            fetch_hosts,
            read_audit,
        }
    }

    pub const fn union(self, other: Self) -> Self {
        Self {
            session_state: self.session_state || other.session_state,
            fetch_hosts: self.fetch_hosts || other.fetch_hosts,
            read_audit: self.read_audit || other.read_audit,
        }
    }
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
            base_authority: M::BASE_AUTHORITY,
            params_schema: params_schema::<M>,
        }
    }
}

fn params_schema<M: CepMethod>() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(M::Params))
        .expect("schemars schema serializes to JSON");
    if let Some(obj) = schema.as_object_mut() {
        let is_composite_object =
            obj.contains_key("oneOf") || obj.contains_key("anyOf") || obj.contains_key("allOf");
        if is_composite_object && !obj.contains_key("type") {
            obj.insert("type".into(), serde_json::Value::String("object".into()));
        }
    }
    schema
}
