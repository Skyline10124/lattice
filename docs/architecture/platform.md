---
status: active
---

# 宿主平台

> 类型：架构合同
> 上位依据：[项目宪章](../governance/charter.md)

## 目的与范围

本文件规定 LATTICE 的宿主平台支持范围，适用于开发、构建、测试、部署和运行，以及相应的维护文档。

## 平台边界

- LATTICE 仅支持 Linux 宿主，所有维护文档均以 Linux 宿主为前提。
- 不引入 macOS、Windows、BSD、通用 Unix 或其他宿主的兼容路径。
- 不为可能的未来移植，用可移植、降级或空实现的回退路径削弱 Linux 实现。
- 扩展 Linux 以外的宿主支持，必须有明确的架构决策和完整的原生实现。
- 默认后台托管需要 Linux 宿主的 systemd user session（`systemctl --user`）：`lattice daemon install` 写入用户级 `lattice.service` 并启用与启动 `lattice-daemon`。
- 没有 systemd user session 的环境不做自动降级，由用户或外部监督器直接手动运行 `lattice-daemon --state-root <绝对路径>`。
- 用户无登录会话时仍需保持后台运行的，由用户自行执行 `loginctl enable-linger`，产品不代为执行会话驻留变更。

## 验证

审查平台相关的文档或实现变更时，核对宿主支持范围与兼容路径是否符合上述边界。
扩展宿主支持时，应核验对应架构决策与原生实现，不能用文档声明代替实现。
