// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::fixtures::RuntimeEffectScenario;

mod external;
mod workspace;

pub(super) type RuntimeEffectFixture = fn() -> Vec<RuntimeEffectScenario>;

pub(super) use external::fetch_runtime_effects;
pub(super) use workspace::{
    create_runtime_effects, edit_runtime_effects, observe_workspace_runtime_effects,
};
