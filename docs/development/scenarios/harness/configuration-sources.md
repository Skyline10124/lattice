---
status: active
---

# 配置来源与运行时重读

| 项 | 内容 |
| --- | --- |
| 目标 | 后台从启动时给定的配置目录读取 profile、`providers.toml` 与 `auth.json`；profile 快照只在显式指定 `agent` 的 `configure` 时重新加载；`providers.toml` 与凭据按模型尝试读取，TLS 信任材料固定于后台启动时的 `SSL_CERT_FILE`、`SSL_CERT_DIR`；并发 Turn 的 endpoint 与凭据互不影响 |
| 前提 | 场景 HOME 下准备两个 agent 的 profile（含引用外部系统提示文件的用例；多步工具回合开启 `fake-tools` feature，配 `test-fake-tools` 二进制与 `LATTICE_TEST_TOOLS`）；`providers.toml` 配置指向不同 fake provider 的条目，其中一个使用不受信任根签发的证书；凭据经 `lattice auth set` 从标准输入录入。场景项目中另放写入其他 endpoint 与 key 的 `.env` 和 `providers.toml`；前端环境中设置各厂商 API key 变量。会话设置的显式修改、持久化与草稿会话见[会话设置与草稿会话](session-settings-and-drafts.md) |
| 相关测试 | `direct_background`：`shared_daemon_uses_registered_providers_and_freezes_profiles`、`session_settings_survive_sources_and_explicit_same_agent_refreshes_profile`、`accepted_effort_survives_retry_and_tools_while_global_budget_reloads`、`invalidated_provider_configuration_records_last_resolved_origin_without_sending`、`backend_tls_roots_are_fixed_until_restart`、`oversized_startup_roots_reject_https_provider_at_submit_and_serve_http`、`no_usable_startup_tls_root_rejects_https_submissions_before_acceptance` |

## 操作

1. 新建会话完成首轮提交后，修改 profile 源文件（含 `agent.toml` 与外部系统提示文件）及 `providers.toml` 中该 provider 的 `base_url`；向同一会话不带 `--agent` 提交、显式指定同名 `--agent` 提交，删除 profile 目录后再分别不带 `--agent` 与带 `--agent` 提交，并另开新会话提交。
2. 在带重试与工具调用的多步 Turn 进行中修改 `base_url` 或使该 provider 配置失效；在两轮之间修改 `[llm]` 预算并显式指定 `--agent` 提交。
3. 两个会话并发提交到不同 provider；提交到证书不受信任的 provider。
4. 后台运行时，以不同的 `SSL_CERT_FILE` 启动前端提交；执行 `daemon stop` 后由该前端拉起新后台再提交。
5. 另起后台，分别以不可加载的 `SSL_CERT_FILE` 和只含超过 64 KiB 证书的 `SSL_CERT_FILE` 启动，执行状态查询并分别向 https 与 http provider 提交。

## 断言

- 后台按启动时固定的配置目录读取 profile、`providers.toml` 与 `auth.json`；场景项目中的 `.env`、`providers.toml` 与前端环境变量既不改变 endpoint，也不提供凭据。
- 已有会话不带 `--agent` 提交时继续使用已保存会话设置中的系统提示、模型与档位（原 profile 目录删除后照常完成），请求发往修改后的 `base_url`；显式给出 `--agent` 的 `configure`（含同名 `--agent` 与新会话）重新加载当前 profile 快照与 `[llm]` 预算，源文件缺失时拒绝 `configure` 且已有会话设置不变。
- 同一 Turn 在重试与工具步骤间保持接受时冻结的模型与档位；进行中修改 `base_url` 后，后续模型尝试发往新 endpoint；provider 配置失效时不发网络请求，按上一次已解析的归属记录不可重试的 provider 配置错误。
- 并发 Turn 的 endpoint 与凭据互不影响；证书不受信任时握手失败，该轮按 provider 错误结束。
- 后台运行期间前端传入的 `SSL_CERT_FILE` 不改变后台信任根；停止后台并由新前端启动后，新信任根生效。
- 没有可用启动根证书的后台照常回答查询：https provider 的提交在接受前按配置错误拒绝，provider 收不到请求；http provider 的提交照常接受并完成。HTTP 场景代替不了 TLS 验证。
- 记录 fake provider 收到的请求（endpoint、模型、推理参数、认证头是否为录入的 key）、会话头中的设置摘要、接受结果与握手结果；证据保持脱敏，报告中只出现凭据名称。
