//! End-to-end integration tests for the model-centric architecture.
//!
//! Each test exercises a full pipeline scenario:
//!   resolve model → get provider → call with ResolvedModel → process response
//!
//! All tests use pure Rust types — no Python runtime required.
//! MockProvider is used for HTTP mocking (no real API keys or network calls).

#[path = "e2e/model_flow.rs"]
mod model_flow;

#[path = "e2e/fallback.rs"]
mod fallback;

#[path = "e2e/custom_model.rs"]
mod custom_model;

#[path = "e2e/unknown_model.rs"]
mod unknown_model;

#[path = "e2e/interrupt.rs"]
mod interrupt;

#[path = "e2e/concurrent.rs"]
mod concurrent;

#[path = "e2e/credential_resolution_characterization.rs"]
mod credential_resolution_characterization;

#[path = "e2e/error_classification_characterization.rs"]
mod error_classification_characterization;

#[path = "e2e/state_machine_characterization.rs"]
mod state_machine_characterization;
