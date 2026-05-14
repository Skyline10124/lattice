#![allow(dead_code, unused_imports, private_interfaces)]

mod coding_agent;
mod commands;
mod config;
mod credentials;
mod display;
mod frontend;
mod plugins;
mod runtime;
mod security;
mod session;

#[cfg(debug_assertions)]
pub mod test_support;
