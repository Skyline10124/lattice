---
status: active
---

# Cassette Futurism 配色资产

> 类型：参考资料 · 设计资产

## 用途与来源

本资产保存 Cassette Futurism 的 8 个色系、每系 12 阶，共 96 个颜色，以及色系用途、色阶语义和配色示例。Token 使用 `<色系>-<阶数>`，例如 `iris-7`。

原设计标注为 OKLCH 色表，以 Lone Trail 为视觉参考。本文保留原资产的 hex 值，不重新生成色阶；原 JSX 展示组件中的复制、明暗预览切换和装饰动画不再保留为代码。

色系与色阶说明是设计参考。TUI 的实际角色、填充面与回退要求由 [TUI 交互与视觉合同](../architecture/tui.md)维护。

## 色系用途

| 色系 | 用途 | 原设计语义 |
| --- | --- | --- |
| cream | 做旧表面、纸张、磁带标签 | aged surface / paper / tape label |
| amber | 警告、注意 | warning / caution |
| purple | 辅助、装饰 | auxiliary / decorative |
| signal-red | VHS 红色条纹、错误、CRT 红色 | VHS red stripe / error / CRT red |
| cyan | CRT 荧光、技术高亮、信息 | CRT phosphor / tech highlight / info |
| navy | 深空、终端背景 | deep space / terminal bg |
| iris | 主色、链接、交互 | primary / link / interactive |
| warm-gray | 面板、设备、文字、边框 | panel / equipment / text / border |

## 色阶语义

| 阶数 | 标签 | 参考用途 |
| --- | --- | --- |
| 1 | app-bg | 应用背景 |
| 2 | subtle-bg | 淡背景 |
| 3 | elem-bg | 元素背景 |
| 4 | hover-bg | 悬停背景 |
| 5 | active-bg | 激活背景 |
| 6 | subtle-border | 淡边框 |
| 7 | border | 边框 |
| 8 | strong-border | 强调边框 |
| 9 | solid | 实色填充 |
| 10 | solid-hover | 实色悬停填充 |
| 11 | lo-text | 低强调文字 |
| 12 | hi-text | 高强调文字 |

## 完整色值

### cream、amber、purple、signal-red

| 阶数 | cream | amber | purple | signal-red |
| --- | --- | --- | --- | --- |
| 1 | `#fbfaf8` | `#fff9f3` | `#fbf9ff` | `#fff8f8` |
| 2 | `#f5f3ef` | `#fcf1e7` | `#f6f1fc` | `#ffefee` |
| 3 | `#eae5dd` | `#f9e2ca` | `#ebe1f8` | `#ffddda` |
| 4 | `#dfd6c8` | `#f7d0a6` | `#e0cff5` | `#ffc8c3` |
| 5 | `#d1c6b4` | `#f0bc85` | `#d2bcee` | `#ffb0a8` |
| 6 | `#c0b29c` | `#e4a65f` | `#c1a6e3` | `#f8958d` |
| 7 | `#a9997f` | `#d38926` | `#ab8ad1` | `#e8736c` |
| 8 | `#907d5f` | `#b36e00` | `#926bbd` | `#d34c49` |
| 9 | `#836e4c` | `#9f6200` | `#8658b4` | `#cb2b31` |
| 10 | `#705d3e` | `#885300` | `#73489e` | `#b21823` |
| 11 | `#4e402a` | `#5f3800` | `#50316e` | `#7c1117` |
| 12 | `#2f281b` | `#3c2200` | `#301f42` | `#4b1111` |

### cyan、navy、iris、warm-gray

| 阶数 | cyan | navy | iris | warm-gray |
| --- | --- | --- | --- | --- |
| 1 | `#f4fcfc` | `#f8fafe` | `#f9faff` | `#fafaf9` |
| 2 | `#e9f7f6` | `#f0f4fa` | `#f0f3fe` | `#f4f3f2` |
| 3 | `#cfeded` | `#dee7f5` | `#dfe5fe` | `#e8e6e4` |
| 4 | `#aee3e3` | `#cad8f0` | `#ccd5ff` | `#dad7d3` |
| 5 | `#8ed7d6` | `#b6c8e7` | `#b8c4fa` | `#cac6c2` |
| 6 | `#68c6c6` | `#9fb5da` | `#a2b0f2` | `#b8b3ae` |
| 7 | `#2ab0b0` | `#829cc8` | `#8695e3` | `#a09a94` |
| 8 | `#009393` | `#6381b2` | `#6977d1` | `#857f78` |
| 9 | `#008282` | `#5072a9` | `#5965ca` | `#777068` |
| 10 | `#006f6f` | `#416093` | `#4a54b2` | `#665f58` |
| 11 | `#004d4d` | `#2c4267` | `#333a7c` | `#46413c` |
| 12 | `#003030` | `#1c293e` | `#1f254b` | `#2b2825` |

## 维护

颜色以本资产的 token 与 hex 对应关系维护。调整色值时核对引用它的界面规则与设计示例；新增展示器应从此来源生成或读取数据，不独立维护第二份色表。
