---
status: active
---

# 配色与终端约定

> 所属领域：[TUI 交互与视觉](README.md)

TUI 面向支持真彩色和 UTF-8 的深色终端，直接使用下列 hex 与 Unicode 符号，不检测终端能力，也不提供命名色或 ASCII 降级显示。[Cassette Futurism 配色资产](../../reference/cassette-futurism-palette.md)提供参考色表，TUI 实际用色由本节约定。

| 前景角色 | Token | Hex | 用途 |
| --- | --- | --- | --- |
| User | iris-7 | `#8695e3` | USER 标题、`[COMMENT n]` 标记、stream phase |
| Assistant | cream-2 | `#f5f3ef` | agent 标题、正文、输入、评论正文、Warn 正文和面板的标题与内容 |
| ToolOk | cyan-7 | `#2ab0b0` | 成功勾号、diff 新增行 |
| ToolErr | signal-red-9 | `#cb2b31` | 失败符号、错误摘要、Error 级状态消息、cancel phase、ERROR 标题和 diff 删除行 |
| Warn | amber-7 | `#d38926` | WARN 标题 |
| DimMeta | warm-gray-8 | `#857f78` | 状态、工具名、字节数、标题线与右端状态、排队消息正文、statusline 文字、Info 与释放后的 Warn 状态消息、启动横幅及 Markdown 的引用、链接 URL、分隔线、表格框线、代码块装饰线和 diff 头 |

| 填充面 | Token | Hex | 文字 |
| --- | --- | --- | --- |
| 侧栏 | navy-11 | `#2c4267` | 暂无文字 |
| statusline | navy-11 | `#2c4267` | DimMeta，phase 按前景角色变化 |
| 评论锚点 | iris-12 | `#1f254b` | 文字保留原前景色 |
| 评论卡片 | warm-gray-12 | `#2b2825` | 正文 Assistant，右端状态 DimMeta |
| COMMENT 标签 | iris-9 | `#5965ca` | Assistant 前景、加粗 |

正文默认使用 Assistant；成功、失败符号与工具元信息分别着色。界面角色中 amber 只用于 WARN 标题。前景角色限于上表六种，左列除评论锚点与评论卡片外不铺背景色。

焦点所在处使用终端光标。启动时把光标设为闪烁块状，退出时恢复终端默认形状；不绘制软光标，也不设置光标颜色。终端实际背景由用户环境提供，`navy-12` 只是原视觉方案的背景参考。

## 代码高亮配色

代码块的语法高亮独立于上述界面角色，可以使用配色资产中的全部颜色。高亮只标出少数帮助阅读结构的成分，其余代码使用正文色：

| 语法成分 | Token | Hex |
| --- | --- | --- |
| 正文：关键字、变量、函数调用、类型引用及未归类成分 | cream-2 | `#f5f3ef` |
| 字符串 | cyan-6 | `#68c6c6` |
| 常量：数字、布尔值与内建常量 | purple-6 | `#c1a6e3` |
| 定义：函数、类型、模块等声明处的名称 | iris-6 | `#a2b0f2` |
| 注释 | amber-6 | `#e4a65f` |
| 标点 | warm-gray-7 | `#a09a94` |

代码高亮不使用 signal-red，红色保留给错误与 diff 删除行。

围栏信息串的第一个词按语法名或文件扩展名匹配语言，忽略大小写，如 `rust`、`rs`、`ts`、`yml`、`sh`；`diff` 始终按 [Markdown 正文](transcript.md#markdown-正文)中的行首规则着色。同一代码块按行序连续解析，跨行的块注释和多行字符串保持各自颜色。引号、注释符号和转义序列随所属的字符串或注释着色，不按标点处理；函数调用与类型引用不按定义着色。
