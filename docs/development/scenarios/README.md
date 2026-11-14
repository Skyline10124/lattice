---
status: active
---

# 场景卡片

每张卡片描述一个要验证的场景。卡片只写本场景特有的内容；原则与共同规则见[测试工程](../testing.md)与[验证操作指南](../validation-guide.md)。

## 自动场景

由外部 harness 在构建出的 `lattice` 与 `lattice-daemon` 二进制上运行，共同的准备、计时、清理与证据见[测试工程](../testing.md#场景的共同规则)。卡片写的是要求；“相关测试”列出按名称对应的现有测试，要求是否全部覆盖、是否通过，以执行记录为准。

这些场景检验同一条提交链上的不同边界：前端提交一次，后台读取配置、接管执行并把每一步写入会话库，前端经视图与查询独立读取与观察。正常协作见[从提交到结果可见](../../architecture/session/flow.md#从提交到结果可见)，观察见[观察与重连](../../architecture/session/recovery.md#观察与重连)；断言以这些合同为准。

| 保护对象 | 由谁验证 |
| --- | --- |
| 提交、双向取消回执交接与结局分类 | API 交互句柄 [LocalTurn](../../architecture/session/README.md#组件与接口) 的局部测试 |
| CLI IPC：真实信号、记录与退出选择 | CLI 的局部回归测试 |
| TUI run_loop：按键、按 NodeId 替换条目、尝试边界，以及按 InvocationId 配对的绘制 | TUI 的局部回归测试 |
| 会话库表编码、值版本解析与旧库检测等产品内部交接 | 相应 crate 的单元与集成测试 |
| provider 故障、工具错误投影与 Turn deadline（模型流与调用期超时） | harness 的 `direct_failures`（见[超时与进程生命周期](../testing.md#超时与进程生命周期)） |
| 前端的实际入口 | 下表的自动场景 |

| 卡片 | 证明什么 |
| --- | --- |
| [接受后断开前端](harness/detach-after-accept.md) | 后台接受的任务在前端退出后继续执行 |
| [接受回执丢失后找回](harness/recover-lost-receipt.md) | 回执丢失时按 TurnId 得到确定结果，同一身份只被接受一次 |
| [同会话排他与跨会话并发](harness/session-exclusion.md) | 同 Session 忙时拒绝新请求，不同 Session 可以并行 |
| [视图、慢观察者与历史查询](harness/view-and-history.md) | 观察者最后持有的条目与关闭后重读的一致 |
| [退回与分叉](harness/rewind-and-fork.md) | 从任意节点分叉，原分支保留，上下文是根到该节点的路径 |
| [旧库拒绝](harness/legacy-store-rejection.md) | 存在旧版存储时会话库打开失败并指出路径 |
| [取消接管后退出](harness/cancel-then-exit.md) | 后台接管的取消在前端退出后仍被处理 |
| [停止后台与排空](harness/stop-and-drain.md) | stop 关闭新准入，等已接管的工作收尾 |
| [后台崩溃与执行者消失](harness/crash-and-vanished-executor.md) | Turn 推进的各提交点崩溃后，重开不经写入即可读到结局 |
| [配置来源与运行时重读](harness/configuration-sources.md) | 后台按启动时的配置目录与 TLS 根读取配置，显式指定 agent 时刷新 profile 快照 |
| [会话设置与草稿会话](harness/session-settings-and-drafts.md) | 选择经 `configure` 持久保存，草稿会话跨重启保留，`SettingsVersion`、项目绑定与档位调整生效 |

## 手动操作

用公开 CLI 动手验证产品行为、运行测试与查看证据。产物、环境与结果判读的原则见[验证操作指南](../validation-guide.md)。

| 卡片 | 内容 |
| --- | --- |
| [构建被测产物](manual/build.md) | 构建前端 `lattice` 与后台 `lattice-daemon`，设置被测路径 |
| [提交与找回](manual/submit-and-recover.md) | 新建与续接会话、选择模型与档位、提交中断后按 TurnId 找回 |
| [查询、观察与用量](manual/observe-and-usage.md) | 读取 Turn 详情、持续观察、在 TUI 中打开会话、查询用量 |
| [会话退回与分叉](manual/rewind.md) | 查看会话树，退回到节点后分叉提交 |
| [取消与执行者消失后继续](manual/cancel-and-continue.md) | 取消正在执行的一轮；执行者消失后继续发送 |
| [后台与升级](manual/daemon-and-upgrade.md) | 查询、停止与前台运行后台；升级与输出字段适配 |
| [凭据与模型元数据](manual/auth-and-models.md) | 录入 key、登录 ChatGPT、查看与刷新模型候选 |
| [运行测试](manual/run-tests.md) | 运行 crate 测试、默认 harness 场景与显式 feature 目标 |
| [本地 CI、远程工作流与 rollout](manual/ci-and-rollout.md) | 运行本地 CI 与 alpha rollout，了解远程门禁的差异 |
| [查看证据](manual/evidence.md) | 查看失败 bundle、rollout 记录与 CI 产物 |

## 卡片模板

自动场景：

```markdown
---
status: active
---

# <场景名>

| 项 | 内容 |
| --- | --- |
| 目标 | <这个场景证明什么> |
| 前提 | <本场景特有的准备> |
| 相关测试 | <harness 测试文件与测试名> |

## 操作

1. <步骤>

## 断言

- <要核对的结果>
```

手动操作：

```markdown
---
status: active
---

# <操作名>

| 项 | 内容 |
| --- | --- |
| 目标 | <要验证或完成什么> |
| 前提 | <所需产物、配置与状态> |
| 规则 | <合同中相应小节的链接> |

## 命令

（sh 代码块）

## 结果

| 结果 | 含义与下一步 |
| --- | --- |
```
