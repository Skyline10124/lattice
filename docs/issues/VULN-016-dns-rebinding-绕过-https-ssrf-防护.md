# VULN-016 DNS rebinding 绕过 HTTPS SSRF 防护

Severity: MEDIUM
Component: `lattice-agent/src/sandbox.rs`
Affected logic: `sandbox.rs:162`

## Summary

即使修复了直接 IP/localhost 检查，DNS rebinding 服务（如 nip.io, xip.io, localtest.me）可解析域名到内网 IP。例如 https://1.0.0.127.nip.io/ → 127.0.0.1。

## Impact

MEDIUM: 可绕过 IP 黑名单，攻击本地服务

## Reproduction

web_search 工具传入 url: "https://1.0.0.127.nip.io:6379/"

## Root Cause (5-Why)

URL 检查在 DNS 解析之前执行。应做二次检查。

## Recommended Fix

DNS 解析后对实际 IP 做二次检查，拒绝内网 IP 段。
