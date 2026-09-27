# TUI R2 失败方式（WP3）

接缝为既有 app_loop_tests 的真实 LocalClient/socket、App 显示与 connection 文案。保持布局、键位和环境边界；不把依赖接口编译失败当作行为红。

| ID | 失败方式与独立信号 |
| --- | --- |
| W3-02 | 启动提前创建 Session；无接受事实即产生 SID；Busy 丢原输入/身份。启动仅校验配置，首次 Submit 的 SID 为空，Accepted/Existing 后回填，失败保留草稿与原 TurnId。 |
| W3-03 | 只读打开强制加载 profile；提交忽略握手 agent。公开 socket 核查询无 Submit，配置按后台选定名称冻结。 |
| W3-04 | 终结事件与重载重算 notice；Cancelled 被显示为错误。原 TurnSummary 的 outcome/notice 在两路径一致。 |
| W3-05a | StepClosed/Closed snapshot 或历史读取启动清临时文字。挂起/失败仍显示已收文字，实际装入对应 Step 才替换。 |
| W3-05b | 重试拼旧尝试、旧 generation 覆盖新节点、同 Invocation 重复块或事件覆盖持久终态。用精确 key/node/generation 及来源测试。 |
| W3-06 | 旧取消交接归新目标、NotSubmitted/迟到错误撤销已知接受。保留真实 run_loop 的双目标/慢历史/取消回执回归。 |

| W3-05c | LocalUpdate::Finished 被吞，或 Closed 快照仍受 submitted/owned 控制而显示 Streaming/Cancelling。真实 run_loop 必须先显示 summary/Idle，正文交付后单独记 Finished。 |
| W3-03b | 已有 Session 显式 --agent 被忽略（尤其 rewind 0）。提交 IPC 必须携带显式选择，未指定才继承后台 current agent。 |
| W3-05d | 关闭 Step 的延迟事件重新开启已装入节点的临时块，或 History 清除了未装入 Step 的工具终态。相同 Step/Invocation 应幂等，持久优先。 |

删除承接：project_branch 校验及 Git commit/OID、Gap、完成确认专属断言按 ADR0006 退役；结局持久性、正文读取、重连与取消风险迁至新快照/Step/TurnSummary 断言，不删除仍有效键位、布局、工具来源和独立控制回归。
