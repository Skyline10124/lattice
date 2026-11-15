// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE 本地后台内置 IM 网关（当前支持 Telegram 私聊纯文本接入）。
//!
//! 根模块仅公开宿主组装所需的只读构造入口 [`prepare_im_gateway`]、构造判定 [`ImBuildOutcome`]、
//! 待运行网关实例 [`PreparedImGateway`] 与台账启动错误 [`ImGatewayError`]。

mod command;
mod config;
mod gateway;
mod identity;
mod ledger;
mod redact;
mod reply;
mod telegram;
mod telegram_http;
mod usage;

#[cfg(test)]
mod gateway_tests;

pub use config::{ImBuildOutcome, prepare_im_gateway};
pub use gateway::{ImGatewayError, PreparedImGateway};
