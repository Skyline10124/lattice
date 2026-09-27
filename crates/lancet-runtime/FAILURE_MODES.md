# S2 WP1 历史与绑定边界

删除恢复算法前的失败清单；现行 workspace/lancet 合同及 S2 §1.1 为依据。

| ID | 风险与断言 | 承接 |
| --- | --- | --- |
| W1-03 | 登记 Descriptor 与真实 compiled catalog 不同仍执行；返回 SchemaMismatch 且无文件/历史效果 | lancet-api `started_dispatch_checks_binding_and_records_mutation`（已先观察真实错误绑定安装文件的红） |
| W1-07a | 删除 Rewind 连带删除正常 installer、baseline 或身份解析 | 保留 history create/edit/外部漂移/并发/current前进/严格 trailer 测试及 host_mutate tests |
| W1-07b | 仅恢复 planner 中的历史真实性校验被一起删掉 | 把 forged merge、baseline tip、root edit、hash、缺 kind 测试迁到 `current` 读取；先确认原读取行为红，再提取只读校验，不保留恢复规划 |

删除范围仅文件恢复、回退 current、恢复审计及其专用测试。目标可达性、恢复内容/冲突/补偿的测试随已取消功能退出；普通历史内容、身份、解析及损坏拒绝继续保护。不新增证明旧符号不存在的扫描测试。
