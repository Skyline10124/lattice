# VULN-011 check_command() 未阻止 > 和 < 重定向操作符

Severity: HIGH
Component: `lattice-agent/src/sandbox.rs`
Affected logic: `sandbox.rs:119`

## Summary

重定向操作符 > 和 < 不在 metacharacter 黑名单中。攻击者可读取任意系统文件（grep . < /etc/shadow）或覆盖关键文件（cargo test > /etc/crontab），完全绕过文件沙箱。bash 工具仅调用 check_command()，不调用 check_read/check_write。

## Impact

HIGH: 绕过所有文件级沙箱保护，可读取 /etc/shadow、覆盖 /etc/crontab 等

## Reproduction

bash 工具传入 command: "grep . < /etc/shadow" 或 "cargo test > /etc/crontab"

## Root Cause (5-Why)

沙箱在 shell 层和文件层之间有空隙：check_command 检查命令字符串，check_read/check_write 检查文件路径，但 bash 工具不交叉检查两者。shell 重定向直接操作文件而不经过路径沙箱。

## Recommended Fix

将 > 和 < 添加到 metacharacter 黑名单。根本方案: 放弃 sh -c。
