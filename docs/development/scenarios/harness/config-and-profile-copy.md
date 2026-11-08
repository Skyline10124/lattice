---
status: active
---

# 配置来源与 profile 副本

| 项 | 内容 |
| --- | --- |
| 目标 | 后台从启动时给定的配置目录读取 profile、`providers.toml` 与 `auth.json`；已有会话按 profile 副本执行；各 Turn 的 endpoint、凭据与 TLS 信任互不影响；TLS 信任材料来自后台启动时的 `SSL_CERT_FILE`、`SSL_CERT_DIR` |
| 前提 | 场景 HOME 下两个 agent 的 profile；`providers.toml` 中两个指向不同 fake provider 的 provider，其中一个的证书由不受信任的根签发；key 经 `lattice auth set` 从标准输入录入。场景项目中另放一份 `.env` 与 `providers.toml`，写入其他 endpoint 与 key；前端环境中设置各厂商的 API key 变量 |
| 相关测试 | `direct_background`：`shared_daemon_uses_registered_providers_and_freezes_profiles`、`profile_copy_survives_sources_and_explicit_same_agent_refreshes_policy`、`public_choices_persist_and_model_changes_use_supported_fallbacks`、`backend_tls_roots_are_shared_with_tools_and_fixed_until_restart`、`oversized_startup_roots_reject_https_provider_at_submit_and_serve_http`、`no_usable_startup_tls_root_rejects_https_submissions_before_acceptance`；`direct_crash_barriers`：`accepted_profile_copy_and_choice_reopen_together_before_receipt` |

## 操作

1. 新会话提交后，修改 profile 的模型与策略，以及 `providers.toml` 中该 provider 的 `base_url`；再向同一会话提交，另开新会话提交。
2. 在多步 Turn 进行中修改 `base_url`。
3. 用 `--model`、`--effort` 提交，再不带选择提交；提交新模型不支持的档位；换到另一 provider 的模型。
4. 两个会话并发提交到不同 provider；提交到证书不受信任的 provider。
5. 后台运行时，以不同的 `SSL_CERT_FILE` 启动前端提交；停止后台后由该前端再提交。
6. 另起一个后台，以不能加载的 `SSL_CERT_FILE` 启动后提交；以只含超过 64 KiB 证书的 `SSL_CERT_FILE` 启动后台，分别向 https 与 http provider 提交。

## 断言

- 已有会话的第二次提交仍用副本中的模型与策略，endpoint 已是修改后的值；新会话使用修改后的 profile。
- 进行中的 Turn 在修改之后的请求发往新的 endpoint。
- 选择写回副本，之后不带选择的提交沿用；不支持的档位在接受前被拒绝，副本不变；换 provider 后副本记下新的 provider `id`。
- 并发 Turn 的 endpoint 与凭据互不影响。
- 场景项目中的 `.env`、`providers.toml` 与前端环境变量，既不改变 endpoint，也不提供 key。
- 证书不受信任时握手失败，这一轮按 provider 错误结束。
- 后台运行时前端改用的信任材料不生效；停止后台、由新前端自动启动之后才生效。
- 没有可用根证书的后台照常回答查询：https provider 的提交在接受前按配置错误被拒绝，provider 收不到请求；http provider 的提交照常接受并完成。HTTP 场景代替不了 TLS 验证。
- 记录 fake provider 收到的请求（endpoint、模型、推理参数、认证头是否为录入的 key）、`head` 中的副本摘要、接受结果与握手结果；证据保持脱敏，报告中只出现凭据的名称。
