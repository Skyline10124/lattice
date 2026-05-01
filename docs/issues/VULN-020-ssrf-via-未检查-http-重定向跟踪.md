# VULN-020 SSRF via 未检查 HTTP 重定向跟踪

Severity: HIGH
Component: `lattice-core/src/provider.rs`
Affected logic: `provider.rs:8-14`

## Summary

共享 reqwest Client 默认跟随 HTTP 重定向（最多 10 次）。恶意 API 端点可 302 重定向到内网地址（如 http://169.254.169.254/）。validate_base_url() 仅检查初始 URL，Authorization header 随重定向泄露。

## Impact

HIGH: 完整 SSRF，可窃取云元数据、攻击内网服务，API key 随重定向发送

## Reproduction

配置恶意 model catalog entry 的 base_url 指向可控外部 HTTPS 服务，服务返回 302 → http://169.254.169.254/

## Root Cause (5-Why)

reqwest 默认重定向策略 + URL 验证仅覆盖初始请求。

## Recommended Fix

添加 .redirect(reqwest::redirect::Policy::none()) 或实现自定义 redirect policy 验证每次目标。
