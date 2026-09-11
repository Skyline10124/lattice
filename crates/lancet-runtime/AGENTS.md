# lancet-runtime 工作指引

## 职责与模块

维护唯一 Component 执行路径、封闭目录、Cell 准入、call-scoped 控制与事件、Tool Host、具体 LocalWorkspace、WorkspaceHistory、证据和结果校验。

- `composition.rs`/`catalog.rs`：组成校验与不可变目录、provider 投影；缺失/重复/身份/ABI/import/通道或 capability→Cell 冲突拒绝。
- `admission.rs`：enabled→deny→boundary 准入、Host scope 解析与逐操作授权；拒绝携带 AdmissionEvidence，未知 deny/enabled 名称在组成时拒绝。
- `execution.rs`：按 Turn 组合 runtime、上下文、Host、预算、工作区及历史，核对 request/binding 一致性。
- `runtime.rs`：Component 实例、engine/linker/ticker、参数校验、取消、结果 CellRef 与 schema 校验及 Host 计量；`limits.rs`/`error_map.rs` 维护资源限制与类型化错误。
- `control.rs`/`host.rs`：当前调用取消、顺序事件和 receipt；`evidence.rs` 的共享记录器提供快照。
- `workspace/`、`host_workspace.rs`、`host_mutate.rs`：具体本地路径解析、观察、编辑准备与安装；`history.rs` 记录和规划私有历史。
- `host_network.rs`：受控 GET、逐跳地址固定、TLS、响应限制与证据；`bindings.rs` 生成版本化 Tool Host WIT 绑定。

## 上位文档

修改前按对象读取 [LANCET](../../docs/architecture/lancet.md)、[Cell](../../docs/architecture/cell.md)、[工作区](../../docs/architecture/workspace.md)、[存储](../../docs/architecture/storage.md)合同。工具构建见[开发指南](../../docs/development/tool-development.md)，验证范围见[测试工程](../../docs/development/testing.md)。

> 本入口的冻结 TLS 根修订待人类批准；生效前原有效版本继续适用，见[修订状态](../../docs/README.md#工作分支修订状态)。

## 局部约束

- 不依赖 lancet-api 或 LATTICE session/agent/profile；authority 是合法下层。无 `wasmtime-wasi`/`wasi-common`，只链接版本化 Host imports，无 ambient WASI。
- `from_embedded` 只消费 inventory 与生成的 EXPECTED_COMPONENTS，无运行时注册 API。`generated/host_composition.rs` 由 manifest 经 tool-codegen 生成，是生产嵌入的唯一来源；Tool 自己的 registration.rs 不单独编译接入。
- `generated/noop_fixture.rs` 是手工维护的固定 test-utils fixture，test_helpers 只为负向测试合成身份/注册。生产不启用 test-utils，不使用固定 fixture 或调用者自造目录。
- `call(&self)` 在阻塞 worker 运行 Wasmtime；async imports 使用有界进程级 Tokio runtime：2 个 async workers，最多 8 个阻塞 DNS task。epoch ticker 首次调用经 OnceLock 启动，需要 Tokio 上下文，组成本身不需要；共享 Arc 跨投影/执行复用，`set_limits(&mut self)` 仅测试。
- call-scoped 状态是唯一取消权威，pre-call、guest logical error、epoch abort 汇入同一 `CallCancelled`；API 复用 `cancelled_error`，不复制代码/类别。错误脱敏，不暴露 backtrace、宿主路径或秘密。
- 每个 effectful import 绑定 Host 确认的 Cell 与具体目标，拒绝只拒绝该操作，设施不执行，记录 DeniedOperation 而非 effect。path/host 目标分别要求 workspace/network scope；emit-event 绑定当前 Turn 的 session Cell，cancelled 是不经授权门的纯控制探测。
- 观察四操作使用相同可见性与授权入口，Host 渲染单个可见窗口，使预算、truncation 和无内容 visibility 证据对齐；guest 只包装外层结果。
- `read-for-edit`、`create-if-absent`、`replace-with-expected`、不安装的 `refuse-edit` 每次重解析路径并授权 `mutate:write:workspace`，执行单一写入上限、系统排除、write-deny 和前提。类型化 Host 拒绝必须优先于 guest 的字符串转述。
- 文件安装、辅助资源清理、安装后目录同步失败的效果报告按工作区合同。不能把目标未安装扩大为工作区无变化；新设计中未完成的实现和验证不因本入口整理而消失。
- WorkspaceHistory 只记录已安装事实并规划恢复，不写本地文件、用户 Git 或补偿 commit，不提供 Agent 历史命令。前态以安装者观察的内容为准；ref/锁/fsync 委托内核，不手写、不用 git subprocess/git2/平行 SQLite 或 receipt store。
- 操作锁与仓库锁期限遵循合同，新 mutation 入口先取得 exclusive 操作锁再读前提，不共享升级。历史严格打开，创建限指定组成入口；Rewind 保留被放弃 tip，不能用时间过期破坏历史 OID 可解析性。
- 网络 TLS 输入与逐跳行为遵循 [LANCET 网络获取](../../docs/architecture/lancet.md#网络获取)，Turn 组合接收显式冻结根，不读后台环境替换信任配置。保留跨 origin 剥离 caller headers、禁压缩、共享预算与取消；区分 received/delivered bytes，I/O 后失败记录 effect，DeniedOperation 仅用于 I/O 前拒绝。
- reserved Descriptor 到实际 compiled Component 的精确身份及启用集校验风险见 LANCET 合同待核验项；不得以文档迁移宣称已解决。

## 验证入口

- `cargo test -p lancet-runtime`、`cargo clippy -p lancet-runtime --all-targets -- -D warnings`。
- 观察：`--lib host_workspace::` 与 `direct_observation`；网络：`--lib host_network::` 与 Tool round-trip。
- 变更：`--lib host_mutate::` 与 direct_mutation/failures/rewind/workspace_history；历史：`--lib history::` 与 API workspace_history_acceptance；Rewind 关联 rewind_acceptance。
- 协议错误码关联 lancet-protocol。依赖树应无 ambient WASI；harness 使用当前构建产物并按影响选择，不默认启动全仓验证。
