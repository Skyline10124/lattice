# lattice-system-harness 工作指引

## 职责

测试已构建 `lattice` 二进制的独立外部 harness，不是产品 crate 的 integration-test target。只共享通用驱动，场景 fixture 和断言留在本场景，Direct 场景共用的 plumbing 位于私有 `tests/support/mod.rs`。

## 上位文档

修改前读取[测试工程](../../docs/development/testing.md)及[宿主平台](../../docs/architecture/platform.md)。构建、命令和产物定位见[验证指南](../../docs/development/validation-guide.md)，产品断言同时读取对应领域合同。

## 局部约束

- manifest 不依赖 lattice/lancet 产品 crate，不用 workspace/path 形式引入产品实现；通用测试和系统库允许。由 `cargo xtask harness-boundary` 按产品名称及 workspace 归属核验，不是禁止所有依赖。
- 仅通过 `LATTICE_BIN` 获取产品。读取 session/audit 走公开 CLI，请求回执与订阅边界可用公开 Local IPC；工作区历史的既有通用 Git 对象检查例外按测试合同执行，不扩大私有读取面。
- 环境隔离、端口、空 `.env`、env_clear/allowlist、ResponseGate、预算、FailureReport、claim 和脱敏遵循测试合同，不增加非 Linux 空实现或故障跳过。
- `ScenarioWorld` 在首次可能自启前登记后台归属，驱动分别管理前端与后台进程组，证据收尾负责报告清理。具体顺序与失败语义按[进程生命周期](../../docs/development/testing.md#超时与进程生命周期)执行。
- fake LLM 通过正式 `OPENAI_COMPATIBLE_*` 接入，无隐藏参数或测试 provider。`fast_env_boundary` 删除变量用 `unset_env`，不能以设空串代替不存在而产生假绿。
- `SessionSendDriver` 驱动 `lattice --json session send`，同时返回原始命令证据与解析 JSONL；未知但有效事件保留为 `Unknown`，非空畸形行另存，不导入产品类型即可核验 0/1/2 退出契约。
- 场景专用断言不可为共享而丢失；独立 release operator fixture/断言由 rollout 对锁定 release 二进制运行，不能把 debug `direct_operator` 重跑当作 release 验收。

## 场景与验证入口

- 快速场景覆盖 help、环境和 CLI；Direct 场景覆盖真实 Turn/reopen、观察、变更、工具、失败/取消/deadline、操作员、秘密、会话隔离、工作区历史及 Rewind。
- `direct_background` 与相关 PTY 场景保护前后端分离、回执丢失、Busy/并发、Gap/固定正文、取消与 stop；具体断言边界见[后台与重连场景](../../docs/development/testing.md#后台与重连场景)。
- `direct_operator` 与 `release_operator` 分别保留 fetch、todo 和组合调用场景及 newest-20 断言；它们均使用 Component，不再用 native/Component/mixed 名称描述执行路径。
- crash-barriers 显式场景验证 SIGSTOP/SIGKILL 窗口、多步累积 transcript、分发前取消的持久关闭及变更后的 checkpoint；普通套件不等于运行了这些 feature。
- Rewind 场景保护无关外部内容、项目 Git、pin 可达性与预检冲突；预检无移动不推广到执行中失败。TurnCommit 的 checkpoint 绑定等未公开数据关系仍由库层验证。
- PTY smoke 验证同一 Direct-Local Turn 接线，不能以环境故障跳过。
- 先按测试合同准备当前被测二进制，再运行所需 fast/direct/PTY target；`release-operator`、`crash-barriers` 必须显式说明。`LATTICE_BIN` 在测试进程 cwd 解析，绝对路径是推荐做法而非额外代码契约。
- 全量 harness 仅在影响范围需要时执行，不作为每次局部修改的默认命令。
