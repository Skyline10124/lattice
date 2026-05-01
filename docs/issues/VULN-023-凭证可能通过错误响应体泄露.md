# VULN-023 凭证可能通过错误响应体泄露

Severity: MEDIUM
Component: `lattice-core/src/errors.rs + lib.rs`
Affected logic: `errors.rs:156-159 / lib.rs:88-94`

## Summary

当 send_streaming_request 收到非 2xx HTTP 响应时，完整响应体文本（最多 8192 bytes）存入 LatticeError::ProviderUnavailable.reason。如果反向代理在错误页面中回显 Authorization header，API key 被捕获在错误结构体中，可能通过日志/序列化泄露。

## Impact

MEDIUM: API key 可能通过错误消息泄露到日志或客户端输出

## Reproduction

触发上游 5xx 错误，代理的错误页面包含请求详情（含 Authorization header）

## Root Cause (5-Why)

错误消息存储原始响应体而非结构化信息。

## Recommended Fix

扫描错误响应体中的 API key 模式并脱敏（sk-、xai-、长 base64 字符串），或仅存储结构化错误码。
