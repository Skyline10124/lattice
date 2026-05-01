# VULN-029 无界限 fork 目标导致线程轰炸

Severity: HIGH
Component: `lattice-harness/src/handoff_rule.rs + pipeline.rs`
Affected logic: `handoff_rule.rs:31-38 / pipeline.rs:535-594`

## Summary

HandoffTarget::parse() 接受任意数量的逗号分隔 fork 目标，无上限。恶意 agent.toml 可定义 fork:agent1,...,agent500 生成 500 个线程，每个执行 LLM API 调用。

## Impact

HIGH: 资源耗尽（CPU/内存/线程）、API rate limit 触发

## Reproduction

agent.toml 中 handoff target = "fork:agent1,agent2,...,agent500"

## Root Cause (5-Why)

fork 分支数无上限。

## Recommended Fix

在 TOML 反序列化时验证分叉数上限（如 10 个）。
