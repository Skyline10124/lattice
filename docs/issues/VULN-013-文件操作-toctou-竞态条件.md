# VULN-013 文件操作 TOCTOU 竞态条件

Severity: HIGH
Component: `lattice-agent/src/tools.rs`
Affected logic: `tools.rs:49-63,90-99`

## Summary

read_file/write_file 在 check_read/check_write 和实际 fs 操作之间无原子性保证。例如: check_read 通过后，攻击者通过并发操作替换 symlink 指向敏感文件，随后 read_to_string 跟随新 symlink 读取敏感内容。

## Impact

HIGH: 在多线程/并发场景下可绕过敏感文件保护

## Reproduction

1) check_read("safe_symlink") 通过（目标为 /tmp/safe）2) 攻击者替换 symlink → 指向 /etc/shadow 3) read_to_string 读取 /etc/shadow

## Root Cause (5-Why)

检查-使用时间差。没有使用 openat/O_NOFOLLOW 风格的原子操作。

## Recommended Fix

先 canonicalize 路径后在 canonical 路径上做二次检查，或使用 O_NOFOLLOW。
