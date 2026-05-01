# VULN-031 WebSocket 端点无鉴权/Origin 验证

Severity: MEDIUM
Component: `lattice-harness/src/ws.rs`
Affected logic: `ws.rs:22-59`

## Summary

/ws WebSocket 端点无 Origin header 检查、无认证 token、无 CORS 策略。任何网页可连接 ws://localhost:PORT/ws 接收实时 pipeline 事件（含 agent 名称、模型、输出预览等）。

## Impact

MEDIUM: 跨域信息泄露 — pipeline 结构、模型选择、输出内容被泄露

## Reproduction

恶意网页通过 JavaScript 连接 ws://localhost:PORT/ws 监听事件

## Root Cause (5-Why)

WebSocket 端点未实现任何访问控制。

## Recommended Fix

添加 Origin header 验证、要求 bearer token、添加 CORS middleware。
