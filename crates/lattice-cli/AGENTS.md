# lattice-cli 工作指引

## 职责

维护 `lattice` 二进制入口、clap 命令与分派、文本和 JSONL 渲染、提交端 dotenv/配置冻结、日志与退出码。Session 操作通过公开 LocalClient 交给后台，不在前端拥有 Turn 执行任务。

- `main.rs`：命令、全局参数、分派和退出处理。
- `commands/run.rs`：保留提交身份和配置来源，经 LocalClient 的具体交互句柄 LocalTurn 发起提交/观察/取消意图，渲染回执、固定结果、正文、事件与 usage，处理信号和退出选择；共同编排见[客户端接口](../../docs/architecture/session.md#实现模块与公开操作)。
- `commands/session.rs`：经后台执行会话操作、发送、查询、取消与恢复；models/resolve/model_render 负责模型列表与解析输出。
- `commands/daemon.rs`：serve/status/stop 入口与状态根解析；自动启动使用 API 的具体 LocalClient。
- `commands/doctor.rs`：凭据与 endpoint 来源列、工作区 `.env` 摘要；audit/lancet 保持为公开下层接口的薄封装。
- `env_boundary.rs`：唯一进程环境安装入口，在 Tokio 运行时创建前保持 `set_var` 的单线程前提；提交端加载工作区 dotenv，daemon serve 分支隔离启动环境。

## 上位文档

修改前读取[会话合同](../../docs/architecture/session.md)、[存储后台边界](../../docs/architecture/storage.md#本地后台与访问边界)、[LANCET 操作员接口](../../docs/architecture/lancet.md)、[TUI 分工](../../docs/architecture/tui.md)和[测试工程](../../docs/development/testing.md)。提交接线重点核对[配置冻结](../../docs/architecture/session.md#执行配置冻结)与[请求接受](../../docs/architecture/session.md#请求接受)；找回、查询和重连分别核对[身份与确认](../../docs/architecture/session.md#请求身份与确认)及[观察边界](../../docs/architecture/session.md#观察边界)。运行配方见[验证指南](../../docs/development/validation-guide.md)。

> 本入口的后台相关修订待人类批准；生效前原有效版本继续适用，见[修订状态](../../docs/README.md#工作分支修订状态)。

## 环境与输出边界

- 工作区 `.env` 是不可信输入。按大小写无关模式拒绝 `LATTICE_*`、provider endpoint 键（`*_BASE_URL`、`*_API_BASE`、`*_API_URL`、`*_ENDPOINT`）、`*_PROXY`、`HOME`/`XDG_*`、`SSL_CERT_FILE`/`SSL_CERT_DIR`；普通凭据仍适用。
- 拒绝依据模式而非枚举变量名。新增依赖读取的环境变量时核对 `classify`：proxy 会影响 `hyper-util`，证书变量会让 `rustls-native-certs` 替换信任库。
- 不增加第二个 loader 或直接调用 `dotenvy::dotenv()`。拒绝值不得进入变量替换状态；启动不打印拒绝，doctor 提供观察结果且不回显源行。
- 环境决策进入纯 `plan_dotenv`：输入文件字节及 `already_set` 判定，返回报告与待安装键值。优先级、BOM 和解析失败在不修改测试进程环境的情况下验证，不把决策放到 `set_var` 循环。
- JSONL 保留公开 `TurnEvent`，另含 submission、acceptance、snapshot、transcript、gap、connection 与结构化 error 控制记录，stdout 不混入人类状态行。保留 0/1/2 退出契约；查询成功、完成确认、结果正文读取分别核验。
- 开始新执行才加载并冻结 profile/provider 配置；找回、只读查询及恢复先使用原持久事实，无需当前凭据。前端保留用户输入、请求身份与退出选择，接受和取消交接遵循会话合同，不以重复补发代替后台接管保证。
- 观察 idle 和回执等待由 LocalTurn 管理，CLI 只消费诊断并选择退出；调整时分别核对客户端等待与产品 deadline。新增命令的只读查询使用公开读取入口，不接 `lookup_or_recover` 或前端执行旁路。
- `pre-stream` 错误分类与固定正文读取失败仍须有精确退出码回归，不能只核对可见 TurnOutcome。

## 验证入口

- `cargo test -p lattice-cli`；每次命令变更覆盖 parser/help。
- Turn 渲染关联 API streaming 与外部 harness；环境边界关联 `fast_env_boundary`，退出码用黑盒断言。
- audit/status 测试通过 dev-dependency 的 `lancet-audit` `test-support` 使用唯一 `ScopedXdgStateHome` 串行化进程 XDG 变量，不新建 crate-local 副本。
- `tests/cli_cutover.rs` 保留局部覆盖，release 与端到端场景遵循测试合同规定的外部归属。
