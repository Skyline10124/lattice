---
name: tdd
description: 在确认测试边界后按真实 red→green 循环开发，每轮完成一个可验证的纵向切片。
---

先按当前宿主读取 [Pi 说明](../../README.pi.md)或 [Delta 说明](../../README.delta.md)，再读取仓库内的[源工作流](../../skill-sources/tdd/SKILL.md)，以及源目录的 [tests.md](../../skill-sources/tdd/tests.md) 与 [mocking.md](../../skill-sources/tdd/mocking.md)。

先确认现行合同、公开测试 seam 与验收范围，观察真实失败后实现最小修复，再运行相同验证，使用 crate 局部验证入口。实现未通过时修改实现。

完成后按[工程规范](../../../docs/development/engineering.md#tdd)清理本轮留下的测试，优先删除；保留的测试符合其中[测试](../../../docs/development/engineering.md#测试)一节的判定。

通用 mock/依赖示例不授权与项目架构冲突的生产 seam。接口本身需设计时使用项目 codebase-design；术语定位遵循共用适配的新 glossary 路径。
