// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 工具调用参数对象的规范摘要。

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::canonical_json;

const ARGUMENTS_DOMAIN: &[u8] = b"lattice-tool-arguments-digest-v1\0";

/// SHA-256 over the fixed domain separator, explicit byte length, and canonical
/// JSON bytes of a Tool arguments object.
pub fn arguments_digest(arguments: &Value) -> [u8; 32] {
    let bytes = canonical_json(arguments);
    let mut hasher = Sha256::new();
    hasher.update(ARGUMENTS_DOMAIN);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes.as_bytes());
    hasher.finalize().into()
}
