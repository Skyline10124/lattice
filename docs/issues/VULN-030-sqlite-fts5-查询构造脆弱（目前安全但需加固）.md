# VULN-030 SQLite FTS5 查询构造脆弱（目前安全但需加固）

Severity: LOW
Component: `lattice-harness/src/memory/sqlite.rs`
Affected logic: `sqlite.rs:157`

## Summary

FTS5 查询通过 format!("\"{}\"", query.replace('"', '')) 构造。目前安全因为 FTS5 语法仅在引号外激活，但防御脆弱。如果 FTS5 行为变更或引号处理有误，存在注入风险。所有其他 SQL 查询使用 params![] 参数化。

## Impact

LOW: 当前安全但防御脆弱

## Reproduction

使用含 FTS5 特殊字符的搜索词（如 AND/OR/NEAR），当前被引号包裹而安全

## Root Cause (5-Why)

字符串拼接而非参数化查询。FTS5 不支持参数化，但应使用专用转义库。

## Recommended Fix

使用专用 FTS5 查询构造库或显式转义所有 FTS5 metacharacter。
