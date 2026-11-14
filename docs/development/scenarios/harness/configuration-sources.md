---
status: active
---

# 配置来源与运行时重读

| 项 | 内容 |
| --- | --- |
| 目标 | 后台从启动时给定的配置目录读取 profile、`providers.toml` 与 `auth.json`；profile 快照只在显式指定 `agent` 的 `configure` 时重新加载；`providers.toml` 与凭据按模型尝试读取，TLS 信任材料固定于后台启动时的 `SSL_CERT_FILE`、`SSL_CERT_DIR`；并发 Turn 的 endpoint 与凭据互不影响 |
| 前提 | 场景 `HOME` 下准备两个 `agent` 的配置（含引用外部系统提示文件的用例；多步工具回合开启 `fake-tools` feature，配带 `lattice-server/test-fake-tools` 的 `lattice-daemon` 二进制并在显式启动后台前设置 `LATTICE_TEST_TOOLS`）；按后台环境快照显式启动自有后台；`providers.toml` 配置指向不同假 provider 的条目，其中一个使用不受信任根签发的证书；凭据经 `lattice auth set` 从标准输入录入。场景项目中另放写入其他端点与密钥的 `.env` 和 `providers.toml`；前端环境中单独设置各厂商密钥变量、不同 `HOME` 或根证书变量。会话设置的显式修改、持久化与草稿会话见[会话设置与草稿会话](session-settings-and-drafts.md) |
| 相关测试 | `direct_background`：`shared_daemon_uses_registered_providers_and_freezes_profiles`、`session_settings_survive_sources_and_explicit_same_agent_refreshes_profile`、`accepted_effort_survives_retry_and_tools_while_global_budget_reloads`、`invalidated_provider_configuration_records_last_resolved_origin_without_sending`、`backend_tls_roots_are_fixed_until_restart`、`oversized_startup_roots_reject_https_provider_at_submit_and_serve_http`、`no_usable_startup_tls_root_rejects_https_submissions_before_acceptance`；`fast_env_boundary`：`dotenv_cannot_add_tls_trust_and_startup_environment_requires_restart`、`agents_use_backend_home_and_project_precedence` |

## 操作

1. 新建会话完成首轮提交后，修改配置文件（含 `agent.toml` 与外部系统提示文件）及 `providers.toml` 中该提供方的 `base_url`；向同一会话不带 `--agent` 提交、显式指定同名 `--agent` 提交，删除配置目录后再分别不带 `--agent` 与带 `--agent` 提交，并另开新会话提交。
2. 在带重试与工具调用的多步 `Turn` 进行中修改 `base_url`、改小模型元数据中的输出上限，或使该提供方配置失效（含关闭提供方的 `thinking_budget` 能力）。
3. 两个会话并发提交到不同提供方；提交到证书不受信任的提供方。
4. 后台运行时，修改原证书文件并以不同的 `SSL_CERT_FILE` 或 `HOME` 运行前端提交；随后以新的环境快照显式重启后台再提交。
5. 另起场景，分别以不可加载的 `SSL_CERT_FILE` 和只含超过 `65536` 字节证书的 `SSL_CERT_FILE` 显式启动后台，执行状态查询并分别向 `https` 与 `http` 提供方提交。

## 断言

- 后台按启动时环境快照固定的配置目录读取 `agent` 配置、`providers.toml` 与 `auth.json`；场景项目中的 `.env`、`providers.toml` 与运行期前端环境变量既不改变端点或后台 `HOME`，也不提供凭据。
- 已有会话不带 `--agent` 提交时继续使用已保存会话设置中的系统提示、模型与档位（原配置目录删除后照常完成），请求发往修改后的 `base_url`；显式给出 `--agent` 的 `configure`（含同名 `--agent` 与新会话）重新加载当前配置快照，源文件缺失时拒绝 `configure` 且已有会话设置不变。
- 同一 `Turn` 在重试与工具步骤间保持接受时冻结的模型与档位；模型元数据按请求重读，输出上限改小后，之后的请求按新上限计算 `thinking_budget`（`16384` 变为 `12288`）。进行中修改 `base_url` 后，后续模型尝试发往新端点；提供方配置失效时不发网络请求，按上一次已解析的归属记录不可重试的提供方配置错误；关闭提供方的 `thinking_budget` 能力后，下一次请求不发出，这一轮以 `failed` 结束，会话头与 `UserMessage` 中的档位保持接受时的值。
- 并发 `Turn` 的端点与凭据互不影响；证书不受信任时握手失败，该轮按提供方错误结束。
- 后台运行期间前端环境的 `SSL_CERT_FILE` 改动不改变后台信任根；按新环境快照显式重启后台后，新信任根生效。
- 没有可用启动根证书的后台照常回答查询：`https` 提供方的提交在接受前按配置错误拒绝，提供方收不到请求；`http` 提供方的提交照常接受并完成。明文场景代替不了证书验证。
- 记录 fake provider 收到的请求（endpoint、模型、推理参数、认证头是否为录入的 key）、会话头中的设置摘要、接受结果与握手结果；证据保持脱敏，报告中只出现凭据名称。
