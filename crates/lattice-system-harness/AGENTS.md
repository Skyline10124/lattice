# lattice-system-harness 工作指引

## 职责

测试已构建 `lattice` 二进制的独立外部 harness，不是产品 crate 的 integration-test target。只共享通用驱动，场景 fixture 和断言留在本场景，Direct 场景共用的 plumbing 位于私有 `tests/support/mod.rs`。

## 上位文档

修改前读取[测试工程](../../docs/development/testing.md)及[宿主平台](../../docs/architecture/platform.md)。构建、命令和产物定位见[验证指南](../../docs/development/validation-guide.md)，产品断言同时读取对应领域合同。

## 局部约束

- manifest 不依赖 lattice/lancet 产品 crate，不用 workspace/path 形式引入产品实现；通用测试和系统库允许。由 `cargo xtask harness-boundary` 按产品名称及 workspace 归属核验，不是禁止所有依赖。
- 仅通过 `LATTICE_BIN` 获取产品。读取 session/audit 走公开 CLI，请求回执与视图边界可用公开 Local IPC；工作区历史的既有通用 Git 对象检查例外按测试合同执行，不扩大私有读取面。
- 环境隔离、端口、env_clear/allowlist、ResponseGate、预算、FailureReport、claim 和脱敏遵循测试合同，不增加非 Linux 空实现或故障跳过。
- `ScenarioWorld` 在首次可能自启前登记后台归属，驱动分别管理前端与后台进程组，证据收尾负责报告清理。具体顺序与失败语义按[进程生命周期](../../docs/development/testing.md#超时与进程生命周期)执行。
- fake LLM 经正式的配置入口接入：场景 HOME 下的 `providers.toml` 把 provider 指向 fake LLM，key 经 `lattice auth set` 录入，无隐藏参数或测试 provider。`fast_env_boundary` 核对工作区 `.env` 与环境变量不提供 key 或 endpoint，删除变量用 `unset_env`，不能以设空串代替不存在而产生假绿。
- `SessionSendDriver` 驱动 `lattice --json session send`，同时返回原始命令证据与解析 JSONL；未知但有效记录保留为 `Unknown`，非空畸形行另存，不导入产品类型即可核验 0/1/2 退出契约。
- 场景专用断言不可为共享而丢失；独立 release operator fixture/断言由 rollout 对锁定 release 二进制运行，不能把 debug `direct_operator` 重跑当作 release 验收。

## 场景与验证入口

- 快速场景覆盖 help、环境变量与 CLI。Direct 场景覆盖真实 Turn 与重新打开、观察、变更、工具、失败、取消与 deadline、操作员、秘密、会话隔离和工作区历史。
- `direct_background` 与相关 PTY 场景保护前后端分离、回执丢失后按 TurnId 找回、Busy 与并发、视图推送与历史查询、取消与 stop；断言的边界见[后台与重连场景](../../docs/development/testing.md#后台与重连场景)。
- `direct_operator` 与 `release_operator` 各自保留 fetch、todo 与组合调用的场景，以及 `newest-20` 断言。两者都使用 Component，不再用 native、Component、mixed 区分执行路径。
- `direct_crash_barriers` 是显式 feature 场景，验证会话库提交后的 SIGSTOP 与 SIGKILL 窗口、各阶段的结论、持久取消、库级故障与观察调度。普通套件通过不代表运行过它。
- `direct_tree_session` 保护按 NodeId 退回分叉、profile 副本与 agent 规则、原 Turn 的 Step 详情，以及文件与历史不回退，并验证旧库被拒绝。未公开的表编码、值版本与事务关系由库层测试验证，harness 不直接打开 redb。
- 配置来源与 profile 副本的场景要求见[测试合同](../../docs/development/testing.md#配置来源与-profile-副本)。
- PTY smoke 验证同一个 Direct-Local Turn 的接线；[模型与档位选择](../../docs/architecture/tui.md#选择面板)的验证待 #315 L4 实现后补齐；环境故障不能当作跳过。
- 先按测试合同准备当前的被测二进制，再运行需要的 fast、direct 或 PTY 目标；运行 `release-operator`、`crash-barriers` 时要明确说明。`LATTICE_BIN` 按测试进程的工作目录解析，推荐用绝对路径，但这不是额外的代码约束。
- 只在影响范围需要时运行全量 harness，它不是每次局部修改的默认命令。
