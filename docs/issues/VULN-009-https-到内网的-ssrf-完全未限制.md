# VULN-009 HTTPS 到内网的 SSRF 完全未限制

Severity: CRITICAL
Component: `lattice-agent/src/sandbox.rs`
Affected logic: `sandbox.rs:162`

## Summary

check_url() 仅对 http:// 检查是否是 localhost，对 https:// 不做任何 host 检查。攻击者可访问 https://127.0.0.1:6379/（本地 Redis）、https://169.254.169.254/（AWS 元数据）等内网服务。

## Impact

CRITICAL: 可访问云元数据 API（AWS/GCP/Azure）、本地管理面板、数据库等

## Reproduction

web_search 工具传入 url: "https://169.254.169.254/latest/meta-data/"

## Root Cause (5-Why)

https:// 被认为安全，但对目的 host 无任何内网/保留地址检查。

## Recommended Fix

对 https:// URL 同样检查 host，拒绝 127.0.0.1/::1/10.0.0.0/8/172.16.0.0/12/192.168.0.0/16/169.254.0.0/16 等。
