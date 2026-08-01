// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::fixtures::RuntimeEffectScenario;

mod external;

pub(super) type RuntimeEffectFixture = fn() -> Vec<RuntimeEffectScenario>;

pub(super) use external::fetch_runtime_effects;
