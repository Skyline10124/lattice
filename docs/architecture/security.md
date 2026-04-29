# 安全边界

## 凭证

- 所有 API key 通过环境变量注入，无配置文件
- ResolvedModel Debug 脱敏：`api_key: "***"`
- 错误消息不含 API key
- 凭证缓存在内存（Mutex<HashMap>），进程内安全

## HTTPS

- 非 localhost HTTP base_url 在 engine 层被拒绝
- localhost/127.0.0.1/::1 允许 HTTP
- 共享 reqwest::Client 使用系统 TLS

## 工具调用

- 工具结果默认 1MB 上限，可配置
- write_file 路径校验（计划中）
- 17 个内置工具：read_file, grep, write_file, list_directory, run_test, run_clippy, bash, patch, run_command, list_processes, web_search, web_fetch, browser_navigate, browser_screenshot, browser_console, execute_code, agent_call

## Sandbox 安全

- `SandboxConfig` 控制工具执行边界
- 文件系统访问限制（read/write 白名单路径）
- 命令执行白名单
- 网络访问控制（允许/禁止域名）

## 信任边界

| 层 | 持凭证 | 可扩展 |
|----|--------|--------|
| artemis-core | 是 | 不可（通过 trait，非任意代码） |
| artemis-agent | 否 | 不可 |
| artemis-plugin | 否 | 可（Plugin: to_prompt, from_output; Behavior: Strict/Yolo） |
| artemis-harness | 否 | 可（AgentProfile TOML, Pipeline 编排） |
