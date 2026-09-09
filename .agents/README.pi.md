# Pi 项目 Skills

本文件用于 Pi 会话。共享入口位于 `skills/<name>/SKILL.md`，工作方法与项目要求由共享入口维护，Pi 的执行方式在这里说明。

## 源工作流与加载

共享入口要求读取源工作流时，用 `read` 打开 `~/.pi/agent/skills/<name>/SKILL.md`，将 `~` 展开为当前用户主目录。直接读取文件，不再次调用同名 skill，以免递归命中项目入口。源工作流的相对附件、模板和脚本相对于源目录解析；全局源只读。源目录缺失时报告所缺输入，不自动安装或改全局配置。

Pi 在项目信任后发现 `.agents/skills/`。通过当前安装版本的 skill 加载诊断核对实际来源；同名条目冲突时不能仅凭名称认定已使用项目版本。可以用 `/skill:<name>` 显式调用；元信息与发现行为以当前 Pi `docs/skills.md` 为准。

## 工具与执行

- 文件读取用 `read`，新文件或完整重写用 `write`，局部修改用 `replace/insert`，撤销用 `undo_last_change`。内容检索用 Hypa，代码关系和诊断用 Pi Lens。
- 命令用 `hypa_shell`；GitHub 通过它调用 `gh-app`，遵循[Git 规范](../docs/governance/git-operations.md)。
- 库资料用官方 Context7；网页搜索与读取用 `web_search`、`fetch_content`，保留来源与版本。不加载 MCP 或 OpenViking。
- 委派统一用 taskflow，先读取当前 SKILL.md，使用完整 DSL，显式 `agentScope: "project"`，多阶段前台运行。核对 `.pi/agents/` 角色与当前工具配置；同一工作目录一个写者，审查不改对象，子 Agent 不自行委派或做 Git/GitHub 写操作。
- 任务列表用 `todowrite`；项目与会话回溯用 Magic Context `ctx_*`。没有完成独立审查时，明确标为单 Agent 核对。

工具能力以当前会话实际目录为准。必需工具不可用或操作被拒绝时，报告受影响步骤，不通过更换工具绕过权限。项目资源文件不证明扩展已加载、凭据已配置或操作已获授权。

## 项目约束

先遵循根及目标路径的 `AGENTS.md`，领域资料的读取顺序见[领域文档指引](../docs/agents/domain.md)。源工作流中的 `CONTEXT.md` 操作统一由项目 domain-modeling skill 适配到 `docs/reference/glossary.md`。

中文写作、文档归属和状态遵循[文档生命周期](../docs/governance/document-lifecycle.md)。源模板中的自动提交、发布、长篇报告或任务账本要求不扩大当前授权；权限与凭据边界遵循[Agent 身份与权限](../docs/governance/agent-permissions.md)。
