// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#[path = "../build_info.rs"]
mod build_info;

fn main() {
    build_info::emit();
}
