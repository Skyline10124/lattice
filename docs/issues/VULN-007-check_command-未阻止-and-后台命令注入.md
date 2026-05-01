# VULN-007 check_command() 未阻止 & 后台命令注入

Severity: CRITICAL
Component: `lattice-agent/src/sandbox.rs`
Affected logic: `sandbox.rs:119`

## Summary

check_command() 的 metacharacter 黑名单包含 '&&' 但不包含独立的 '&'。攻击路径: {"command": "cargo test & rm -rf /"} —— '&' 不在黑名单中，cargo 通过 allowlist，sh -c 将 '&' 解析为后台操作符导致任意命令执行。

## Impact

CRITICAL: 完全命令注入，可执行任意系统命令

## Reproduction

在默认 sandbox 下，bash 工具传入 command: "cargo test & rm -rf /"

## Root Cause (5-Why)

黑名单模型不完整。check_command() 枚举了常见 shell 操作符但遗漏了 &（后台）、\n（换行=命令分隔）、\r（回车）、>、<（重定向）等。正确方案是将 shell 执行改为直接 exec。

## Recommended Fix

将 & 添加到 blocked metacharacter 列表（& 在 && 之前匹配）。根本方案: 放弃 sh -c，使用 std::process::Command 直接执行二进制。
