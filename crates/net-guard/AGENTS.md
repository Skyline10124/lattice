# net-guard 工作指引

## 职责与边界

- 作为领域中立的 Layer-0 叶子，提供 URL 校验、DNS 解析、私有与保留地址拒绝、DNS rebinding 域名检查及中立错误。
- 只依赖 `std` 和 `url`，不引入 Tokio、reqwest、LANCET 或 LLM 类型。网络策略、HTTP 客户端、provider 行为及 Tool 执行留在调用方。
- 调用方必须在建立连接前解析并固定地址，连接只连已校验的地址，不能用配置时校验替代连接边界。解析是阻塞调用，async 调用方放到 blocking 线程执行。
- IPv4/IPv6 special-purpose 分类对齐 IANA 注册表。通用 provider 路径保留已说明的代理 fake-IP 范围，严格 Tool-egress 判定拒绝这些范围；每次更新添加前缀边界测试。

## 上位文档

修改前读取[宿主平台](../../docs/architecture/platform.md)。Tool 网络接线同时读取 [LANCET 网络获取](../../docs/architecture/lancet.md#网络获取)，不将其领域策略套用于所有 provider。

## 验证入口

- 本地：`cargo test -p net-guard`。
- 依赖边界：`cargo tree -p net-guard --depth 1`。
- 跨 crate 影响：按调用路径验证 `lattice-llm`、`lancet-authority` 和 `lancet-runtime`。
