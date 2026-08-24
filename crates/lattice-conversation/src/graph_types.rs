// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed durable branch transcript.

use serde::{Deserialize, Serialize};

use crate::{BoundToolCall, NativeMessage};

/// Complete ordered typed transcript for the current branch HEAD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadTranscript {
    pub session_id: String,
    pub parent_commit_id: Option<String>,
    pub messages: Vec<NativeMessage<BoundToolCall>>,
}
