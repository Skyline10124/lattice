# VULN-012 read_allowlist 是 dead code — 任意文件读取

Severity: HIGH
Component: `lattice-agent/src/sandbox.rs`
Affected logic: `sandbox.rs:73-88`

## Summary

SandboxConfig 定义了 read_allowlist: Vec<String> 字段（第 8 行，"Directories where reads are allowed. Empty = anywhere."），但 check_read() 方法（第 73-88 行）从未引用 self.read_allowlist。即使调用方设置了 read_allowlist，限制也完全不生效。

## Impact

HIGH: 攻击者可读取任何不被 sensitive_files 匹配的系统文件（/etc/passwd、/proc/self/environ、应用源码等）

## Reproduction

read_file 工具传入 path: "/etc/passwd"。check_read() 跳过（不含 ..，不在 sensitive_files 中）→ 文件被读取并返回。

## Root Cause (5-Why)

字段实现遗漏。read_allowlist 被定义并文档化但从未集成到 check_read() 的验证逻辑中。

## Recommended Fix

在 check_read() 中添加: if !self.read_allowlist.is_empty() { ... 检查 path 是否以某 allowed 目录开头 ... }
