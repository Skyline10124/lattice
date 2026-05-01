# VULN-024 URL 验证接受非 HTTP 协议

Severity: LOW
Component: `lattice-core/src/router.rs`
Affected logic: `router.rs:592-624`

## Summary

validate_base_url 仅对 http:// 检查 localhost，对其他协议（ftp://, file://, gopher:// 等）不作拒绝。目前 reqwest 拒绝不支持的协议，但防御不完整。

## Impact

LOW: 纵深防御缺陷，未来协议支持变更可能导致问题

## Reproduction

base_url: "file:///etc/passwd" 通过验证（不含 http://，也不匹配 localhost 检查路径）

## Root Cause (5-Why)

缺少显式协议白名单。

## Recommended Fix

仅允许 https:// 和 http://（with localhost restriction），显式拒绝所有其他协议。
