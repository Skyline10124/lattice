# 基准测试

本目录仅保留 R2 之前 Git InvocationStore 的历史基准与资格化工具。独立调用账本及 `lancet-audit` Rust 测量 target 已删除，完整采样脚本不能在当前工作树运行；这些工具不用于 redb 资格化或当前产品验收。适用范围与证据保留见[历史诊断指南](../docs/development/invocation-store-diagnostics.md)。

## InvocationStore

- [历史运行脚本](invocation-store/run-qualification.sh)：旧环境、测量与汇总流程，仅作追溯。
- [资格化合同](invocation-store/qualification-contract-v2.json)：运行组、样本规模、阈值与基线信息。
- [结果汇总](invocation-store/summarize_qualification.py)：解析结果、计算指标与输出报告。
- [运行隔离](invocation-store/qualification_isolation.py)：分配运行目录并定位预构建的测试二进制。
- [汇总逻辑测试](invocation-store/test_summarize_qualification.py)与[运行隔离测试](invocation-store/test_qualification_isolation.py)。

只读汇总与生成表检查见历史诊断指南。阈值与原始样本不随存储切换改写，不运行旧采样脚本来验证当前会话库。
