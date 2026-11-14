// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 快速二进制黑盒 CLI 场景测试的共用辅助函数。
//!
//! 从 `LATTICE_BIN` 解析被测二进制，并在场景世界中按规格的单命令超时预算执行调用；
//! 各场景自身的收尾辅助继续负责断言与失败产物落盘。

use crate::driver::{CommandOutcome, LatticeCommand, LatticeDriver};
use crate::scenario::ScenarioSpec;
use crate::world::ScenarioWorld;

/// 从 `LATTICE_BIN` 解析标准驱动；未设置或未指向文件时以统一消息触发 panic。
pub fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN 必须指向已构建的 lattice 二进制")
}

/// 按场景规格的单命令超时预算，在 `world` 中运行 `lattice <args...>`。
pub fn run(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> CommandOutcome {
    driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("驱动 lattice 命令失败")
}
