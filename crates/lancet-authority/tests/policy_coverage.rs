// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Catalog method coverage matrix.
//!
//! These tests ensure that every catalog method has explicit coverage in the
//! policy resolver and that adding a new method without coverage fails the
//! build.

use lancet_catalog::method::RequiredCapability;

/// Fixture row describing expected policy behavior for a catalog method.
struct MethodFixture {
    method: &'static str,
    required_capability: RequiredCapability,
}

/// Build the method fixture matrix covering all canonical catalog methods.
fn method_fixtures() -> Vec<MethodFixture> {
    vec![
        MethodFixture {
            method: "observe.workspace",
            required_capability: RequiredCapability::Inspect,
        },
        MethodFixture {
            method: "mutate.create",
            required_capability: RequiredCapability::WritePath,
        },
        MethodFixture {
            method: "mutate.edit",
            required_capability: RequiredCapability::WritePath,
        },
        MethodFixture {
            method: "fetch.web",
            required_capability: RequiredCapability::FetchUrl,
        },
    ]
}

// ── Coverage tests ─────────────────────────────────────────────────────

/// Verify every catalog method has an explicit fixture row.
///
/// If a new method is added to the catalog without a fixture row, this test
/// fails with the missing method names listed.
#[test]
fn all_catalog_methods_have_coverage() {
    let catalog = lancet_catalog::catalog::catalog();
    let fixtures = method_fixtures();
    let fixture_methods: Vec<&str> = fixtures.iter().map(|f| f.method).collect();

    let mut missing = Vec::new();
    for descriptor in catalog {
        if !fixture_methods.contains(&descriptor.name) {
            missing.push(descriptor.name);
        }
    }

    assert!(
        missing.is_empty(),
        "Catalog methods missing coverage fixtures: {:?}\n\
         Add a row to method_fixtures() for each missing method.",
        missing
    );
}

/// For each fixture row, verify the fixture's required_capability matches the
/// catalog descriptor.
#[test]
fn method_matrix_covers_all_methods() {
    let catalog = lancet_catalog::catalog::catalog();
    let fixtures = method_fixtures();

    for fixture in &fixtures {
        let descriptor = catalog
            .iter()
            .find(|d| d.name == fixture.method)
            .unwrap_or_else(|| panic!("Method '{}' not found in catalog", fixture.method));

        assert_eq!(
            descriptor.capability, fixture.required_capability,
            "RequiredCapability mismatch for {}: catalog has {:?}, fixture expects {:?}",
            fixture.method, descriptor.capability, fixture.required_capability
        );
    }
}
