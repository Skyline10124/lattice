# VULN-036 深度嵌套 JSON 比较可能导致栈溢出

Severity: LOW
Component: `lattice-harness/src/handoff_rule.rs`
Affected logic: `handoff_rule.rs:258-281`

## Summary

HandoffCondition.value 是 serde_json::Value（允许任意嵌套深度）。values_equal() 对复合类型使用 PartialEq 递归比较。深度嵌套的 TOML 配置在手递规则评估时可导致栈溢出。

## Impact

LOW: DoS via 精心构造的 agent profile

## Reproduction

构造深度嵌套数千层的 TOML 配置 value 字段

## Root Cause (5-Why)

递归比较无深度限制。

## Recommended Fix

在手递规则反序列化时添加最大嵌套深度检查。
