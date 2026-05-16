//! Concrete provider modules for v1.1 LLM execution.
//!
//! Each provider module owns request JSON construction, SSE parsing,
//! and auth header logic for a specific provider family.
//! No provider registry, plugin trait, or `lattice-core` dependency exists here.

pub mod anthropic;
pub mod gemini;
pub mod openai;
