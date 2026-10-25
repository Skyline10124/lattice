# lancet-authority 工作指引

## 职责

维护确定性、无 Tool I/O 的宿主策略：网络与工作区策略、观察预算、Guard L1、读取可见性和 System Path Exclusion。Cell 准入执行、Component 生命周期及 Host I/O 归 `lancet-runtime`，持久证据归 `lattice-session` 的调用槽位。

- `policy/network.rs`：I/O 前 URL 策略与唯一 `allowed_hosts` matcher；runtime 独立核验解析地址与连接。
- `policy/observation.rs`：观察界限及共享 per-Turn 预算。
- `policy/default.rs`：profile/外部策略数据及读写策略。
- `policy/visibility.rs`：read/list/grep/find 共用的可见性与系统排除判定。
- `guard.rs`：Guard L1 路径和 scope 裁决。

## 上位文档

修改前读取[工作区路径策略](../../docs/architecture/workspace.md)、[Cell 准入与授权](../../docs/architecture/cell.md)、[LANCET 网络获取](../../docs/architecture/lancet.md#网络获取)。

## 局部约束

- 工具暴露不授予权限；Component 只声明请求的 Cell，Host 准入及每次 import 的操作授权共同把关。
- `fetch__web` 需要启用且以 `Trust::Ex` 准入、`allow_public_https = true`、主机匹配非空 `allowed_hosts`。localhost 另需 `allow_localhost = true` 及精确 allowlist 匹配；URL、DNS、重定向和连接端点仍须通过地址策略。
- `host_allowed_by_list` 是唯一主机模式匹配器：trim、ASCII 小写化，前导点匹配 apex 与子域，空或全点模式不匹配。URL 策略与 runtime host-target scope 共用此函数。
- DNS、socket、TLS、重定向和响应整形不进入本 crate。策略 reader 与配置键一同维护，不接受执行端会忽略的键。

## 验证入口

- 运行 `cargo test -p lancet-authority`。
- 改了网络策略时，再运行 `cargo test -p lancet-runtime --lib host_network::`。
- 改了 Guard 时，runtime 场景要同时覆盖允许与拒绝两种结果；测试资源的选择见[测试工程](../../docs/development/testing.md)。
