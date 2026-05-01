# VULN-033 LATTICE_AGENTS_DIR 环境变量可毒化 agent 注册表

Severity: MEDIUM
Component: `lattice-harness + lattice-cli`
Affected logic: `watcher.rs:74-79 + run/new_agent/validate 等多处`

## Summary

多个函数通过 LATTICE_AGENTS_DIR 环境变量解析 agent 目录。攻击者控制执行环境后可设 LATTICE_AGENTS_DIR=/tmp/malicious，加载完全由攻击者控制的 agent.toml。覆盖所有 agent 配置。

## Impact

MEDIUM: 完整 agent 注册表接管，所有 pipeline 路由被重定向

## Reproduction

export LATTICE_AGENTS_DIR=/tmp/malicious-agents; cargo run -- run pipeline

## Root Cause (5-Why)

环境变量覆盖无路径范围验证。

## Recommended Fix

移除环境变量覆盖或验证路径必须在 ~/.lattice/ 或项目根下。
