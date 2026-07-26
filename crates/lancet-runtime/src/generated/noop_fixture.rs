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
        descriptor_sha256: parse_hex_digest(
            "17de570c1304cce5696c53b86bcb0f79e604c032ac5e37433995152a0b8870c3",
        ),
        artifact_sha256: parse_hex_digest(
            "6fcc7fc8570be61f887278661f4a5a6353c3ca2c46e249dcc8de843500f73df4",
        ),
        abi_version: 1,
        // The component imports the `host-control` interface from
        // `lattice:tool-host@0.1.0`. The component binary records this as
        // an instance import named `lattice:tool-host/host-control@0.1.0`.
        imports: &["lattice:tool-host/host-control@0.1.0"],
    }
}
