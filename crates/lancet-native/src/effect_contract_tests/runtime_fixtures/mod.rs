// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::fixtures::RuntimeEffectScenario;

mod audit;
mod command;
mod external;
mod session;
mod workspace;

pub(super) type RuntimeEffectFixture = fn() -> Vec<RuntimeEffectScenario>;

pub(super) use audit::{audit_observe_runtime_effects, audit_replay_runtime_effects};
pub(super) use command::command_run_runtime_effects;
pub(super) use external::fetch_runtime_effects;
pub(super) use session::{envelope_runtime_effects, todo_write_runtime_effects};
pub(super) use workspace::{
    create_runtime_effects, observe_workspace_runtime_effects, patch_runtime_effects,
};
