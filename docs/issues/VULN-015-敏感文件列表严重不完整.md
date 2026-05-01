# VULN-015 敏感文件列表严重不完整

Severity: MEDIUM
Component: `lattice-agent/src/sandbox.rs`
Affected logic: `sandbox.rs:36-43`

## Summary

sensitive_files 仅包含 6 项: .env, .env.local, .env.production, credentials.json, secrets, .git/credentials。缺少 .git/config（含 remote URL/token）、~/.ssh/id_*（SSH 私钥）、~/.aws/credentials、~/.config/gcloud/、.npmrc、.pypirc、/proc/self/environ 等。

## Impact

MEDIUM: 大量敏感文件和凭证可通过 read_file 或 bash 工具读取

## Reproduction

read_file 工具传入 path: "/home/user/.ssh/id_rsa" 或 "/proc/self/environ"

## Root Cause (5-Why)

sensitive_files 是手动枚举而非基于文件类型/位置的系统性匹配。

## Recommended Fix

大幅扩充列表并改用 glob 或正则匹配，覆盖 SSH 密钥、云凭证、包管理器 token、进程敏感信息。
