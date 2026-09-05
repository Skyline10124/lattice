# 基准测试

本目录保留 InvocationStore 基准与资格化工具。

## InvocationStore

- [运行脚本](invocation-store/run-qualification.sh)：准备运行环境、执行测量并汇总结果。
- [资格化合同](invocation-store/qualification-contract-v2.json)：运行组、样本规模、阈值与基线信息。
- [结果汇总](invocation-store/summarize_qualification.py)：解析结果、计算指标与输出报告。
- [运行隔离](invocation-store/qualification_isolation.py)：分配运行目录并定位预构建的测试二进制。
- [汇总逻辑测试](invocation-store/test_summarize_qualification.py)与[运行隔离测试](invocation-store/test_qualification_isolation.py)。

运行条件与产物路径见运行脚本，资格化参数见合同，不在本入口重复维护。
