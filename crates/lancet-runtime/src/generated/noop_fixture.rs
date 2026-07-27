// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fixed no-op fixture for integration tests (`test-utils` feature).
//!
//! Constructs an `EmbeddedTool` registration and `ExpectedToolIdentity`
//! from the committed `tools/noop/generated/` artifacts. The fixture is
//! fixed: no caller-controlled registrations, paths, bytes, handlers, or
//! approval set. This is the only non-production composition entry point.

use crate::composition::{
    _parse_hex_digest as parse_hex_digest, EmbeddedTool, ExpectedToolIdentity,
};

/// Fixed registration: the committed `noop.echo` descriptor + component bytes.
static NOOP_FIXTURE: EmbeddedTool = EmbeddedTool {
    name: "noop.echo",
    descriptor_json: include_bytes!("../../../../tools/noop/generated/descriptor.json"),
    component_bytes: include_bytes!("../../../../tools/noop/generated/noop_echo.component.wasm"),
};

pub fn registration() -> &'static EmbeddedTool {
    &NOOP_FIXTURE
}

pub fn expected_identity() -> ExpectedToolIdentity {
    ExpectedToolIdentity {
        name: "noop.echo",
        // These must equal `descriptor-sha256` / `artifact-sha256` in
        // `tools/noop/generated/expected-identity.json`. Despite living under
        // `generated/`, this file is hand-maintained and nothing regenerates or
        // cross-checks it, so re-pinning a Component silently breaks the
        // composition gate here until both digests are copied across by hand
        // (see #103).
        descriptor_sha256: parse_hex_digest(
            "d80977cbf0b61c937d93692f2a5b07b5d3a408230498f16bea83a833dbe504ad",
        ),
        artifact_sha256: parse_hex_digest(
            "b677a9b339c4703d8a02af54def031509ff17d73c66a83fc59176a528d646f08",
        ),
        abi_version: 1,
        // The component imports the `host-control` interface from
        // `lattice:tool-host@0.1.0`. The component binary records this as
        // an instance import named `lattice:tool-host/host-control@0.1.0`.
        imports: &["lattice:tool-host/host-control@0.1.0"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digests above are hand-copied from
    /// `tools/noop/generated/expected-identity.json`, which `cargo xtask
    /// tool-build` rewrites whenever a Component is re-pinned. Nothing
    /// regenerates this file, so without this test a re-pin leaves the
    /// composition gate rejecting the fixture with a digest mismatch and the
    /// only clue is a failing integration test elsewhere (#103).
    ///
    /// Parsed from the same bytes the fixture embeds, so this cannot pass by
    /// reading a stale copy.
    #[test]
    fn fixture_digests_match_the_generated_expected_identity() {
        const IDENTITY_JSON: &str =
            include_str!("../../../../tools/noop/generated/expected-identity.json");

        let identity: serde_json::Value =
            serde_json::from_str(IDENTITY_JSON).expect("expected-identity.json is valid JSON");
        let expected = expected_identity();

        let artifact = identity["artifact-sha256"]
            .as_str()
            .expect("artifact-sha256 is a string");
        let descriptor = identity["descriptor-sha256"]
            .as_str()
            .expect("descriptor-sha256 is a string");

        assert_eq!(
            expected.artifact_sha256,
            parse_hex_digest(artifact),
            "artifact digest drifted from expected-identity.json ({artifact}); \
             copy it into noop_fixture.rs after `cargo xtask tool-build`"
        );
        assert_eq!(
            expected.descriptor_sha256,
            parse_hex_digest(descriptor),
            "descriptor digest drifted from expected-identity.json ({descriptor}); \
             copy it into noop_fixture.rs after `cargo xtask tool-build`"
        );
        assert_eq!(
            expected.abi_version,
            identity["abi-version"]
                .as_u64()
                .expect("abi-version is a number") as u32,
        );
    }
}
