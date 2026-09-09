# lattice-cli 工作指引

## 职责

维护 `lattice` 二进制入口、clap 命令与分派、文本和 JSONL 渲染、dotenv/日志初始化、退出码；通过下层公开 API 调用会话、模型和审计能力，不重复 Turn 或 LANCET 执行逻辑。

- `main.rs`：命令、全局参数、分派和退出处理。
- `commands/run.rs`：Turn 流渲染、JSONL、idle timeout、usage 和退出分类。
- `commands/session.rs`：会话操作与发送；models/resolve/model_render 模块负责模型列表与解析输出。
- `commands/doctor.rs`：凭据与 endpoint 来源列、工作区 `.env` 摘要；audit/lancet 模块保持为公开下层接口的薄封装。
- `env_boundary.rs`：唯一进程环境安装入口，在 Tokio 运行时创建前执行，保证 `set_var` 的单线程前提。

## 上位文档

修改前读取[会话合同](../../docs/architecture/session.md)、[LANCET 操作员接口](../../docs/architecture/lancet.md)、[TUI 分工](../../docs/architecture/tui.md)和[测试工程](../../docs/development/testing.md)。运行配方见[验证指南](../../docs/development/validation-guide.md)。

## 环境与输出边界

- 工作区 `.env` 是不可信输入。按大小写无关模式拒绝 `LATTICE_*`、provider endpoint 键（`*_BASE_URL`、`*_API_BASE`、`*_API_URL`、`*_ENDPOINT`）、`*_PROXY`、`HOME`/`XDG_*`、`SSL_CERT_FILE`/`SSL_CERT_DIR`；普通凭据仍适用。
- 拒绝依据模式而非枚举变量名。新增依赖读取的环境变量时核对 `classify`：proxy 会影响 `hyper-util`，证书变量会让 `rustls-native-certs` 替换信任库。
- 不增加第二个 loader 或直接调用 `dotenvy::dotenv()`。拒绝值不得进入变量替换状态；启动不打印拒绝，doctor 提供观察结果且不回显源行。
- 环境决策进入纯 `plan_dotenv`：输入文件字节及 `already_set` 判定，返回报告与待安装键值。优先级、BOM 和解析失败在不修改测试进程环境的情况下验证，不把决策放到 `set_var` 循环。
- JSONL 对齐公开 `TurnEvent`，stdout 不混入人类状态行；保留公开的 0/1/2 退出契约，pre-stream 错误分类变化须有精确黑盒断言。
- idle timeout 与产品 deadline 是不同边界；调整时核对会话预算、实际事件及退出信息，不将观察超时当成持久关闭证明。

## 验证入口

- `cargo test -p lattice-cli`；每次命令变更覆盖 parser/help。
- Turn 渲染关联 API streaming 与外部 harness；环境边界关联 `fast_env_boundary`，退出码用黑盒断言。
- audit/status 测试通过 dev-dependency 的 `lancet-audit` `test-support` 使用唯一 `ScopedXdgStateHome` 串行化进程 XDG 变量，不新建 crate-local 副本。
- `tests/cli_cutover.rs` 保留局部覆盖，release 与端到端场景遵循测试合同规定的外部归属。
