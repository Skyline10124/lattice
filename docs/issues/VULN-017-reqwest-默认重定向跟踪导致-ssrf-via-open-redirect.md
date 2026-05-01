# VULN-017 reqwest 默认重定向跟踪导致 SSRF via Open Redirect

Severity: MEDIUM
Component: `lattice-agent/src/tools.rs`
Affected logic: `tools.rs:195`

## Summary

reqwest::blocking::get() 默认跟随 HTTP 重定向（最多 10 次）。攻击者设置外部 HTTPS 端点返回 302 → http://169.254.169.254/...。初始 URL 通过 check_url（https://），重定向后访问 AWS 元数据。

## Impact

MEDIUM: 通过重定向链访问内网 HTTP 服务（包括云元数据 API）

## Reproduction

web_search 工具传入 url: "https://evil.com/redirect?to=http://169.254.169.254/latest/meta-data/"

## Root Cause (5-Why)

URL 验证仅对初始 URL 执行，重定向后的目标 URL 未重新验证。

## Recommended Fix

禁用自动重定向: .redirect(reqwest::redirect::Policy::none())。
