# net-guard 工作指引

提供领域中立的 URL 校验、DNS 解析、私有与保留地址分类及 DNS rebinding 域名检查，被 `lattice-llm` 调用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | `validate_base_url_static` / `validate_base_url`、`resolve_public_socket_addrs`、`is_forbidden_tool_target_ip`、`host_is_dns_rebinding_domain` 与 `NetGuardError` |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [宿主平台](../../docs/architecture/platform.md)、[工程规范](../../docs/development/engineering.md) |
| provider URL 与连接地址校验 | [provider 配置](../../docs/architecture/llm/providers.md#endpoint-的来源)、[会话存储与并发](../../docs/architecture/session/concurrency.md#输入与传输限制) |

## 约束

- `Cargo.toml` 仅依赖标准库与 `url`，输出同步纯函数与领域中立的 `NetGuardError`，保持 Layer-0 叶子独立于异步运行时和上层协议。
- 调用方在建立连接前调用 `resolve_public_socket_addrs` 解析并固定已校验的公网地址，把 `validate_base_url_static` 与 `validate_base_url` 作为配置加载期的静态或尽力检查，以在连接边界阻断 DNS rebinding；连接固定要求见 [endpoint 的来源](../../docs/architecture/llm/providers.md#endpoint-的来源)。
- `resolve_public_socket_addrs` 与 `validate_base_url` 执行阻塞式系统名称解析，异步调用方将其放入 blocking 线程运行，避免阻塞异步执行器。
- IPv4/IPv6 special-purpose 分类对齐 IANA 注册表：通用 provider 路径保留基准测试网段（如 `198.18.0.0/15`）以兼容本地代理 fake-IP，`is_forbidden_tool_target_ip` 则对该范围一并拒绝，使 provider 接入与 Tool 出站各自维持对应的隔离级别。
- 本 crate 之外：provider 连接池复用、重定向与传输策略由 `lattice-llm` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p net-guard` |
| 修改 `Cargo.toml` 依赖 | `cargo tree -p net-guard --depth 1`（核对仅含 `url`） |
| 修改 IP 分类或域名后缀规则 | 在 `cargo test -p net-guard` 中覆盖网段起止与相邻前缀边界用例 |
| 修改公开签名或判定行为 | `cargo test -p lattice-llm` |
