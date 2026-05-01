# VULN-022 任意 HTTP Header 注入 via provider_specific header: 机制

Severity: MEDIUM
Component: `lattice-core/src/lib.rs + transport/gemini.rs`
Affected logic: `lib.rs:76-80 / gemini.rs:448-452,510-514`

## Summary

provider_specific HashMap 支持 header: 前缀注入任意 HTTP 头。无白名单，可注入 Host、X-Forwarded-For 等。Gemini transport 实现了两次（streaming + non-streaming）。

## Impact

MEDIUM: 恶意 catalog 数据可注入 HTTP 头，改变请求路由或绕过安全控制

## Reproduction

catalog entry 的 provider_specific: {"header:Host": "evil.com", "header:X-Forwarded-For": "10.0.0.1"}

## Root Cause (5-Why)

header 注入机制无 allowlist 验证。

## Recommended Fix

维护允许的 header 名白名单（仅 x-* headers），拒绝安全敏感 header（Host, Content-Length, Transfer-Encoding, Authorization）。
