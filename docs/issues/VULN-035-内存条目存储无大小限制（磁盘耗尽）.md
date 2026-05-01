# VULN-035 内存条目存储无大小限制（磁盘耗尽）

Severity: LOW
Component: `lattice-harness/src/pipeline.rs`
Affected logic: `pipeline.rs:760-779`

## Summary

Pipeline::save_memory_entry() 存储完整 agent 输出无大小上限。恶意 agent 可生成大量 JSON 输出存入 SQLite，反复操作可快速耗尽磁盘。

## Impact

LOW: 磁盘耗尽导致系统不稳定

## Reproduction

重复运行产生超大 JSON 输出的 agent，输出被无限制写入 SQLite

## Root Cause (5-Why)

输出存储缺少大小限制。

## Recommended Fix

添加最大输出大小上限（如 1 MB），超限则截断或拒绝。
