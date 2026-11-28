// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

// 推导测试会启动大量 git 子进程，fork 到 exec 之间会暂时持有继承的 socket 描述符。
// 若与依赖对端关闭 socket 的 lattice 单测同进程并行，关闭效果会被延后。
// 因此将这些推导测试放在独立的测试进程中。
#[path = "../../build_info.rs"]
mod build_info;
