# VULN-010 bash 命令执行无超时控制导致 DoS

Severity: CRITICAL
Component: `lattice-agent/src/sandbox.rs + tools.rs`
Affected logic: `sandbox.rs:20 / tools.rs:135`

## Summary

SandboxConfig 定义了 max_command_timeout: u32 = 30，但 tools.rs 中的 bash 执行（Command::new("sh").args(["-c", cmd])）完全未使用此字段。无 .timeout() 或 wait_timeout() 调用。

## Impact

CRITICAL: 单次工具调用可永久阻塞 agent 线程。permissive 模式下 sleep 99999 即触发。

## Reproduction

bash 工具传入 command: "sleep 99999"（permissive 模式）或 "cargo test -- endless-loop-test"

## Root Cause (5-Why)

超时字段定义但执行路径未消费。设计与实现脱节。

## Recommended Fix

在 bash 执行中添加 .spawn() + .wait_timeout() 或使用 tokio::time::timeout 包装。
