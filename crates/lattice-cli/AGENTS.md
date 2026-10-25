# lattice-cli 工作指引

## 职责

维护 `lattice` 二进制入口、clap 命令与分派、文本和 JSONL 渲染、日志与退出码。Session 操作、凭据与模型命令通过公开 LocalClient 交给后台；CLI 不读取 profile、provider 配置、模型元数据或凭据，也不在前端拥有 Turn 执行任务。

- `main.rs`：命令、全局参数、分派和退出处理。
- `commands/run.rs`：保留提交身份与 agent、模型、推理档位的选择，经 LocalClient 的具体交互句柄 LocalTurn 发起提交/观察/取消意图，渲染回执、会话头、对话条目、生成中的文字与 usage，处理信号和退出选择；共同编排见[客户端接口](../../docs/architecture/session.md#实现模块与公开操作)。
- `commands/session.rs`：经后台执行会话操作、发送、查询与取消；执行者消失的 Turn 停在已写入的进度，新提交和会话写操作直接进行。`session usage` 渲染后台给出的逐轮用量与费用估算。
- `lattice auth` 与 `lattice models`：只负责界面，经后台录入与删除 key、SIWC 登录与登出、查询凭据状态与模型候选、刷新元数据，见[凭据命令](../../docs/architecture/llm.md#凭据命令)。
- `commands/daemon.rs`：serve/status/stop 入口与状态根解析；自动启动使用 API 的具体 LocalClient，把配置目录、agents 目录与 TLS 根证书来源交给后台，见[配置来源与后台启动](../../docs/architecture/session.md#配置来源与后台启动)。
- audit/status 经 Local IPC 查询后台，不在 CLI 直接打开会话库或组成 runtime。

## 上位文档

修改前读取[会话合同](../../docs/architecture/session.md)、[模型接入合同](../../docs/architecture/llm.md)、[存储后台边界](../../docs/architecture/storage.md#本地后台与访问边界)、[LANCET 操作员接口](../../docs/architecture/lancet.md#操作员接口)、[TUI 分工](../../docs/architecture/tui.md)和[测试工程](../../docs/development/testing.md)。提交接线重点核对[配置冻结](../../docs/architecture/session.md#执行配置冻结)、[profile 副本](../../docs/architecture/session.md#会话的-profile-副本)与[请求接受](../../docs/architecture/session.md#请求接受)；找回、查询和重连分别核对[请求身份与找回](../../docs/architecture/session.md#请求身份与找回)及[观察边界](../../docs/architecture/session.md#观察边界)。运行配方见[验证指南](../../docs/development/validation-guide.md)。

## 环境与输出边界

- CLI 不加载工作区 `.env`，也不从环境变量读取 key、endpoint 或模型；交给后台的环境值只有[配置来源与后台启动](../../docs/architecture/session.md#配置来源与后台启动)所列的几项，新增前先修订会话合同。
- key 只从不回显的终端或标准输入读取，不作为命令行参数，不进入日志、错误、文本或 JSONL 输出；SIWC 登录只显示授权 URL 与结果。
- `--model` 与 `--effort` 只检查写法（`<provider>/<model>` 与档位名），provider、模型与档位是否可用由后台在接受前判定。
- 流式命令的 JSONL 只含所提交或所跟的那一轮，记录只有 `submit`、`cancel`、`head`、`entry`、`text`、`connection` 与结构化 `error` 七种，见[公开操作](../../docs/architecture/session.md#实现模块与公开操作)；stdout 不混入人类状态行。人类可读输出在一轮结束时打印一次结局摘要，等级等面向用户的文字用稳定的小写词，不用 `Debug` 输出。保留 0/1/2 退出契约；查询成功、Turn 结局和正文读取分别核验。
- 提交前生成请求身份（新会话含 SessionId）并先输出，再连同选择提交一次；找回经按 TurnId 查询，与只读查询一样只使用原持久事实，无需当前凭据。前端保留用户输入与退出选择，接受和取消交接遵循会话合同，不以重复补发代替后台接管保证。
- 观察 idle 由客户端库管理，回执等待由 LocalTurn 管理，CLI 只消费诊断并选择退出；调整时分别核对客户端等待与产品 deadline。新增命令的只读查询使用公开读取入口，不接提交路径，不建前端执行旁路。
- `pre-stream` 错误分类与固定正文读取失败仍须有精确退出码回归，不能只核对可见 TurnOutcome。
- 配置拒绝的 `message` 取自 API 的 `ConfigRejection`，已经脱敏；CLI 不把它换成固定文本，也不另拼未脱敏的原始错误；退出 2。

## 验证入口

- `cargo test -p lattice-cli`；每次命令变更覆盖 parser/help。
- Turn 渲染关联 API streaming 与外部 harness；配置来源的负向测试关联 `fast_env_boundary`，退出码用黑盒断言。
- release 与端到端场景遵循测试合同规定的外部归属。
