// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Transitional native CEP catalog.

use crate::method::MethodDescriptor;

/// Every built-in method is now an embedded Component Tool.
pub static CATALOG: [MethodDescriptor; 0] = [];

/// Compile-time-known native catalog length.
pub const CATALOG_LEN: usize = 0;

/// Return the production native catalog, empty after #32.
pub fn catalog() -> &'static [MethodDescriptor] {
    &CATALOG
}

#[cfg(feature = "inspect-scaffold")]
use crate::methods::scaffold::{Inspect, Navigate};
#[cfg(feature = "inspect-scaffold")]
use std::sync::LazyLock;

/// Feature-gated scaffold descriptors; never part of [`catalog`].
#[cfg(feature = "inspect-scaffold")]
pub static SCAFFOLD_CATALOG: LazyLock<[MethodDescriptor; 2]> = LazyLock::new(|| {
    [
        MethodDescriptor::for_method::<Inspect>(),
        MethodDescriptor::for_method::<Navigate>(),
    ]
});

#[cfg(feature = "inspect-scaffold")]
pub fn scaffold_catalog() -> &'static [MethodDescriptor] {
    &*SCAFFOLD_CATALOG
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_catalog_is_empty_after_fetch_web_migration() {
        assert!(super::catalog().is_empty());
        assert_eq!(super::CATALOG_LEN, 0);
    }
}
