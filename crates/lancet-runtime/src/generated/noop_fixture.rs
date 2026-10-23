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

/// Fixed registration: the committed `noop__echo` descriptor + component bytes.
static NOOP_FIXTURE: EmbeddedTool = EmbeddedTool {
    name: "noop__echo",
    descriptor_json: include_bytes!("../../../../tools/noop/generated/descriptor.json"),
    component_bytes: include_bytes!("../../../../tools/noop/generated/noop_echo.component.wasm"),
};

pub fn registration() -> &'static EmbeddedTool {
    &NOOP_FIXTURE
}

pub fn expected_identity() -> ExpectedToolIdentity {
    ExpectedToolIdentity {
        name: "noop__echo",
        // These must equal `descriptor-sha256` / `artifact-sha256` in
        // `tools/noop/generated/expected-identity.json`. Despite living under
        // `generated/`, this file is hand-maintained and nothing regenerates or
        // cross-checks it, so re-pinning a Component silently breaks the
        // composition gate here until both digests are copied across by hand
        // (see #103).
        descriptor_sha256: parse_hex_digest(
            "709a4e536f842f80bf493f03ec636fe94ab7798271370d86542a66169f8f8843",
        ),
        artifact_sha256: parse_hex_digest(
            "3c7085377ed38b68fefd38e0915377a54259eb07a68377e7fe5bcc819e37236e",
        ),
        abi_version: 1,
        // The component imports the `host-control` interface from
        // `lattice:tool-host@0.1.0`. The component binary records this as
        // an instance import named `lattice:tool-host/host-control@0.1.0`.
        imports: &["lattice:tool-host/host-control@0.1.0"],
    }
}
