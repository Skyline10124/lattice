# VULN-019 多向量组合绕过 (PATH 注入 + 子进程继承)

Severity: MEDIUM
Component: `lattice-agent/src/sandbox.rs + tools.rs`
Affected logic: `sandbox.rs:132-133`

## Summary

check_command 仅验证程序名是否在 allowlist，参数完全不检查。结合沙箱的其他漏洞，可进行: 1) PATH=... 环境变量注入加载恶意二进制 2) 允许 cargo 执行用户 build.rs 脚本的链式攻击。

## Impact

MEDIUM: 多种组合绕过路径使黑名单模型极为脆弱

## Reproduction

bash 工具传入 command: "PATH=/tmp/malicious:$PATH cargo test"

## Root Cause (5-Why)

allowlist+黑名单组合模型存在系统性问题，无法覆盖所有注入向量。

## Recommended Fix

放弃 sh -c，使用结构化命令执行；执行前清理环境变量；限制 cargo 子命令为白名单。
