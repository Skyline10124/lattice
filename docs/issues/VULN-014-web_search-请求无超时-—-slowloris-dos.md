# VULN-014 web_search 请求无超时 — Slowloris DoS

Severity: HIGH
Component: `lattice-agent/src/tools.rs`
Affected logic: `tools.rs:195`

## Summary

web_search 使用 reqwest::blocking::get(url) 无任何超时配置（连接超时、读取超时、总超时均缺失）。攻击者控制的外部端点可通过 Slowloris 技术（每 10 分钟发 1 字节）永久阻塞 agent 线程。

## Impact

HIGH: Agent 线程被无限期阻塞，无法处理后续对话轮次

## Reproduction

web_search 工具传入 url: "https://evil.com/slow-endpoint"（evil.com 的 /slow-endpoint 每 10 分钟发送 1 字节）

## Root Cause (5-Why)

使用便捷函数 reqwest::blocking::get() 而非构建带超时配置的 Client。

## Recommended Fix

使用 reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?。
