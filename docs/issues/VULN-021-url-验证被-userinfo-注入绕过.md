# VULN-021 URL 验证被 userinfo 注入绕过

Severity: MEDIUM
Component: `lattice-core/src/router.rs`
Affected logic: `router.rs:592-624`

## Summary

validate_base_url 通过手动字符串分割提取 host，不解析 RFC 3986 userinfo。恶意 base_url 如 https://anything@169.254.169.254/ 中 'anything@169.254.169.254' 不匹配 localhost 检查，但 reqwest 的 URL 解析器正确将 anything 视为用户名、169.254.169.254 为实际 host。

## Impact

MEDIUM: SSRF 到任意内网 IP，绕过 host 验证

## Reproduction

配置 catalog entry 的 base_url: "https://anything@169.254.169.254/v1"

## Root Cause (5-Why)

手动字符串解析代替 URL 解析库。

## Recommended Fix

使用 url::Url::parse() 正确提取 host 组件。
