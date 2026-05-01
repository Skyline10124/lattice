# VULN-032 system.file symlink 遍历泄露敏感文件到 LLM

Severity: MEDIUM
Component: `lattice-harness/src/profile.rs`
Affected logic: `profile.rs:127-144`

## Summary

system_prompt() 拒绝绝对路径和 .. 但不调用 canonicalize() 解析 symlink。攻击者创建 symlink 指向 /etc/passwd 或 ~/.ssh/id_rsa，引用为 system.file = "harmless-link"。path.exists() 通过，read_to_string 跟随 symlink 读取敏感文件，内容注入 agent 系统提示词并发送到 LLM provider。

## Impact

MEDIUM: 本地文件泄露到 LLM provider（Anthropic/OpenAI/DeepSeek 等）

## Reproduction

ln -s /etc/passwd ~/.lattice/agents/test/harmless-link; agent.toml: system.file = "harmless-link"

## Root Cause (5-Why)

路径检查未解析 symlink。

## Recommended Fix

在读取前调用 path.canonicalize()，验证 canonical 路径在允许的目录前缀内。
