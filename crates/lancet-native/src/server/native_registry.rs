// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Transitional native registry.
//!
//! All production methods are embedded Components. The empty registry remains
//! until ADR 0004 removes the native/Component dispatch split.

use crate::server::registry::MethodRegistry;
use lancet_protocol::CepError;

pub fn register_production_method_registry(_registry: &mut MethodRegistry) -> Result<(), CepError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_registry_is_empty_after_fetch_web_migration() {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry)
            .expect("empty production registry registers");
        assert!(registry.registered_methods().is_empty());
    }
}
