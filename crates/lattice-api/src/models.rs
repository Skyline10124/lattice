// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub use lattice_conversation::{
    ModelId, ModelIdError, ModelRef, ModelRefError, ProviderId, ProviderIdError,
};
pub use lattice_llm::{
    Availability, ConfigurationRequirement, ModelCandidate, ModelCandidateQuery, ModelCandidates,
    ModelPrice, ModelWarning,
};

/// 通过公开 API 初始化日志。
pub fn init_logging(verbose: bool) -> std::io::Result<()> {
    lattice_llm::init_logging(verbose)
}
