# Git 内核删除边界与失败方式

本清单在 S5 修改 store 测试与实现前登记；不新增存储规格。

| 编号 | 失败方式与独立断言 | 入口 |
| --- | --- | --- |
| S5-K01 | 移除 Global 后误改 Project 身份编码、忽略目录键或版本不符；既有项目库规范字节保持不变，错误项目键仍为 Corruption | meta 的 project_scope_wire_form、round_trip_and_expected_check、rejects_wrong_kind_version_scope_and_format；repo 的 project_scope_must_match_directory_key |
| S5-K02 | 测试从旧 global 夹具换成 project 身份时丢失首次创建、CAS 前后屏障、fanout、精确重试的断言 | 保留 repo 对应原测试，仅迁移夹具；integration 保留 SIGKILL 命中确认及 absent/valid 重开断言 |
| S5-K03 | 删除 retained 后工作区历史失去已记录祖先或 mutation 的 TurnId/InvocationId | runtime history 与 API workspace_history_acceptance；harness direct_workspace_history、direct_tree_session 的真实二进制场景 |

无生产调用方的 Git 会话读取、候选对象寻址、跨引用发布与 retained 专属测试随删除退出；不留下扫描旧符号的墓碑测试。变异红只证明保留断言的失败信号，不冒称原实现存在缺陷。实际运行与未覆盖窗口写入 S5 交接。
