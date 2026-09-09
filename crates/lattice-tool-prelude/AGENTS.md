# lattice-tool-prelude 工作指引

## 职责

提供 Source Tool 的 guest SDK、固定版本的 `wit_bindgen` 生成器重导出和 `export_tool!` Component 导出胶水。Host 策略、效果、审计及工作区、网络、会话操作不由本 crate 暴露。

## 上位文档

修改前读取 [LANCET 构建与封闭组成](../../docs/architecture/lancet.md#构建与封闭组成)及[宿主平台](../../docs/architecture/platform.md)。构建操作见[工具开发指南](../../docs/development/tool-development.md)。

## 局部约束

- 使用 `#![no_std]` 和 `extern crate alloc`；guest 目标为 `wasm32-unknown-unknown`，不是 `wasm32-wasip2`，导入仅限 Tool Host world。
- 不引入 `lancet-*` 依赖或 Host 内部类型。
- prelude 重导出的 `wit_bindgen` 与 guest 直接依赖的 `wit-bindgen` 版本一致；`generate!` 生成代码通过裸 crate 路径引用其运行时。
- `export_tool!` 在执行 `wit_bindgen::generate!` 的 guest `mod bindings` 内调用，不能从 crate 根接线，避免生成宏的 `self` 解析错误。
- `tools/noop` 是 `distribution = "fixture"` 的 tracer，仅由 `test-utils` fixture 组成。生产 Tool 的 manifest 拥有契约，参数 schema 由 Host 在进入 Component 前校验；guest 不再维护一套命令式 Descriptor 规则。

## 验证入口

- `cargo test -p lattice-tool-prelude`。
- `cargo tree -p lattice-tool-prelude`：无 `lancet-*` 依赖。
- guest 构建由 `cargo xtask tool-build` 驱动，对应 `cargo build -p <tool> --target wasm32-unknown-unknown`。
