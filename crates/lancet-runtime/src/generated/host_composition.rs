// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Production host composition: the host-pinned `EXPECTED_COMPONENTS` list.
//!
//! Empty until a production Tool migrates to a Component (#29–#32, Codeberg
//! #7–#10). The `from_embedded()` constructor reads
//! `inventory::iter::<&EmbeddedTool>` + this list; when both are empty, the
//! runtime composes an empty catalog (no Component Tools are available yet,
//! but native CEP methods still work through `lancet-dispatch`).

use crate::composition::ExpectedToolIdentity;

/// The production expected-identity set. Pinned by the trusted host
/// composition step; a Tool crate cannot self-approve.
pub(crate) static EXPECTED_COMPONENTS: &[ExpectedToolIdentity] = &[];
