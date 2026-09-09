# lancet-protocol 工作指引

## 职责

维护纯共享 wire/data 契约：JSON-RPC 信封和 CEP Tool frame、类型化持久身份与绑定、Cell/能力、错误和版本化证据。无 I/O、async runtime、执行引擎或策略副作用。

`jsonrpc.rs`/`frame.rs` 维护信封及别名；`cell.rs`/`capability.rs` 维护 Cell、trust-free CellRef 和能力解析；`evidence.rs` 维护三类版本化证据；`canonical.rs` 维护确定性 JSON；`binding.rs`、`identity.rs`、`address.rs` 维护绑定、身份及中立地址；`error.rs` 集中维护错误码与 retriability。`lib.rs` 的公开重导出是 crate 接口。

## 上位文档

修改前读取 [Cell 数据与证据合同](../../docs/architecture/cell.md)、[LANCET 调用流程](../../docs/architecture/lancet.md)、[会话数据合同](../../docs/architecture/session.md)。

## 局部约束

- `InvocationId`/`TurnId` 为小写、带连字符的 UUID v4，`SessionId` 为 32 位小写十六进制；parse/display/serde/schema 一致。本 crate 校验但不生成 ID，不引入第二套 String/newtype 身份。
- 运行时使用已预留 InvocationId 的规范字符串，预留才创建 Invocation。控制通知可以引用已有 InvocationId，但不能预留或执行 Tool。
- 字段序列化变化是跨 crate 契约变化，显式核对 serde/schemars。Cell 词汇先在此定义，再协调策略、审计、运行时和生成消费者，不编码具体引擎/Host 实现假设。
- 三类持久证据记录先核版本再解释内容；字段演化、未知字段策略和 canonical JSON 遵循 Cell 合同。`truncated: Option<bool>` 保留三态，visibility 不携带文件内容，mutation 证据保留前后 revision、前提及安装事实。
- 不恢复已退役的 Turn/audit provenance、in-toto attestation 或另一套操作员投影词汇；artifact identity 与 Bundle provenance 在其所属契约维护。
- 新错误 family 同步 wire code 与 retriability 测试；新增证据字段同步策略校验、持久读写、runtime 生产者及操作员 DTO 测试。

## 验证入口

- `cargo test -p lancet-protocol`。
- schema/wire 变化增加必要序列化或 schemars 断言，并检查受影响的 authority、audit、runtime、API 和 CLI。
