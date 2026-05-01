# VULN-027 Gemini URL 中 model ID 长度无上限

Severity: LOW
Component: `lattice-core/src/transport/gemini.rs`
Affected logic: `gemini.rs:436-440,497-502`

## Summary

Gemini URL 构造对 api_model_id 无长度限制。超长 ID 可导致服务端拒绝或中间代理截断 URL。

## Impact

LOW: 请求可能被拒绝或 URL 被截断

## Reproduction

使用超长 api_model_id

## Root Cause (5-Why)

输入长度未验证。

## Recommended Fix

在 URL 构造前验证 api_model_id 长度。
