# VULN-034 diagnostics() 方法泄露 API key 存在性

Severity: LOW
Component: `lattice-cli/src/credentials.rs`
Affected logic: `credentials.rs:58-64,75-80`

## Summary

CredentialsStore::diagnostics() 返回哪些 API key 环境变量已设置。攻击者可枚举已配置的 LLM provider（如 ANTHROPIC_API_KEY: true），聚焦攻击目标。

## Impact

LOW: 凭证侦察 — 攻击者知道哪些 LLM provider 有可用凭证

## Reproduction

调用 CredentialsStore::diagnostics() 获取已配置 provider 列表

## Root Cause (5-Why)

诊断方法在返回的公开信息中包含凭证存在性。

## Recommended Fix

移除 diagnostics() 或仅返回非凭证配置项的 presence/absence。
