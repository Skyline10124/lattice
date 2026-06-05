// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Centralized provider endpoint constants.
//! These are private internal constants - not exposed in public API.

/// OpenAI API base URL
pub(crate) const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Anthropic API base URL
pub(crate) const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// Google Gemini API base URL
pub(crate) const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// OpenAI-compatible default base URL (Ollama local)
pub(crate) const OPENAI_COMPATIBLE_DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";
