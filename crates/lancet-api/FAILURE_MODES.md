# S2 WP1 执行与操作员接缝

实施前清单；依据 session/lancet/cell 合同及 S2 计划。测试使用临时真实会话库与生成 Component，不接真实状态根。

| ID | 失败方式及断言 | 入口 |
| --- | --- | --- |
| W1-03 | 无成功 Started 无执行凭据；method/arguments/实际 descriptor 与登记不同必须拒绝且不安装文件；凭据无 Clone/Deserialize | runtime 接缝测试、session 既有开始失败矩阵及 compile-fail 文档测试 |
| W1-04 | Started 后取消未同步给 sender；延迟 monitor 时仍返回真实 CallCancelled receipt，文件/历史无新增 | `started_cancellation_is_synchronous` |
| W1-05 | 门面自行 open 或跨快照拼接；保留调用方读事务、全局高水位与项目过滤，错误直接保持 StoreError | `operator_facade_uses_supplied_snapshot`；S1 F37–39 承接分页/20条/隐私 |
| W1-07 | 正常执行不再记录文件前后与调用身份 | `started_dispatch_checks_binding_and_records_mutation`；runtime history/host_mutate 既有测试 |

旧 admission fsync 分类测试由 session 提交失败/Started 事务测试承接；不保留已取消的双库 admission 分支。产品 Owner 的失败后零 dispatch、跨项目 IPC 和 worker 排空由 WP2–WP4 承接，不以本测试宣称 E2E。
