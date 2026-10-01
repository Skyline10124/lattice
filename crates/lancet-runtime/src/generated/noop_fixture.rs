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
            "5d9a09676e41362dfbc493f341b1bcc422488b621a570080cc2defc6f12d4e2d",
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
