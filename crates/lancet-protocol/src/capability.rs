// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP capability class data shared by envelopes, policy, and catalog descriptors.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Required capability class for a catalog method.
///
/// Catalog methods declare the capability *kind* as a const. In a Direct-only
/// world, `RequiredCapability` is the catalog-level kind — no pattern resolution
/// or concrete `Capability` construction is needed (D-04).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequiredCapability {
    Inspect,
    ReadPath,
    WritePath,
    RunCommand,
    FetchUrl,
    MutateSessionState,
    ReadAudit,
}
