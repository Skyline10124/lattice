# VULN-018 Bash ANSI-C 引用绕过 metacharacter 检查

Severity: MEDIUM
Component: `lattice-agent/src/sandbox.rs`
Affected logic: `sandbox.rs:119`

## Summary

当 /bin/sh 实际为 bash 时，$'...' ANSI-C 引用可编码任意字符。例如 cargo test$'\\n'rm -rf / 中 $'\\n' 展开为字面换行符，$'\\x3b' → ;。黑名单检查的是原始字符串而非展开结果。

## Impact

MEDIUM: 在 /bin/sh → bash 的系统上，metacharacter 黑名单可被绕过

## Reproduction

bash 工具传入 command: "cargo test$'\\n'rm -rf /"

## Root Cause (5-Why)

黑名单 + Shell 展开的组合脆弱性。黑名单检查输入，shell 展开输入后执行结果。

## Recommended Fix

将 $' 添加到 metacharacter 黑名单。根本方案: 放弃 sh -c。
