# VULN-028 Agent 配置文件无完整性校验 — 可注入恶意 handoff 规则

Severity: CRITICAL
Component: `lattice-harness/src/profile.rs`
Affected logic: `profile.rs:114-118`

## Summary

agent.toml 配置文件无数字签名或哈希校验。攻击者可修改任何 agent.toml，添加 default = true 的 handoff 规则指向恶意 agent。watcher 自动 hot-reload 无完整性检查。

## Impact

CRITICAL: 全管道劫持 — 所有 agent 链被重定向到攻击者控制的 agent

## Reproduction

修改 ~/.lattice/agents/*/agent.toml，添加 [[handoff.rules]] default = true, target = "evil-agent"

## Root Cause (5-Why)

配置文件来源不可信但被盲目信任。

## Recommended Fix

为 agent.toml 添加 SHA-256 哈希或 Ed25519 签名，加载时验证。
