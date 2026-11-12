// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 单次尝试模型步骤驱动器（ADR 0005 第 10 节）。
//!
//! 本 crate 每次调用只驱动一次 provider 尝试，不持有持久化、调用身份分配、工具执行、
//! 重试策略、对账或 Turn 生命周期；多步编排由 `lattice_server::TurnCoordinator` 负责。

pub mod model_step;

pub use model_step::{
    LlmClient, ModelDiagnostic, ModelStepAttempt, ModelStepDriver, ModelStepError, ModelStepEvent,
    ModelStepRequest, ModelStopCause,
};

#[cfg(feature = "test-utils")]
pub mod testing;
