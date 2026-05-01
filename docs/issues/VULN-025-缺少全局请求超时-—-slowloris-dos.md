# VULN-025 缺少全局请求超时 — Slowloris DoS

Severity: LOW
Component: `lattice-core/src/provider.rs`
Affected logic: `provider.rs:8-14`

## Summary

共享 HTTP Client 设置 connect_timeout(10s) 和 read_timeout(120s) 但无总 timeout()。恶意服务端可每 60 秒发送 1 字节，持续占用连接。

## Impact

LOW: 连接耗尽导致 DoS，发至 120 秒的重复读取无总时长限制

## Reproduction

恶意 API endpoint 以极慢速率（如 1 byte/60s）发送响应

## Root Cause (5-Why)

缺少总超时配置。read_timeout 限制单次读取而非总请求时长。

## Recommended Fix

添加 .timeout(Duration::from_secs(300))。
