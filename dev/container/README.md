# Podman 开发容器

在 Linux x86_64 宿主上使用 rootless Podman 构建、测试和开发 LATTICE。
Rust 1.96.0、rustfmt、Clippy、rust-analyzer、WASM target 和 wasm-tools 1.254.0 随镜像安装。
基础镜像与 wasm-tools 发布资产固定摘要；系统包在构建时从 Debian trixie 仓库安装。

## 使用

在仓库根执行：

```sh
bash dev/container/dev.sh build
bash dev/container/dev.sh start
bash dev/container/dev.sh shell
```

执行单条命令或完整验证：

```sh
bash dev/container/dev.sh exec cargo build --locked -p lattice-cli --bin lattice
bash dev/container/dev.sh verify
bash dev/container/dev.sh status
bash dev/container/dev.sh stop
```

容器名为 `lattice-dev`，镜像名为 `localhost/lattice-dev:rust-1.96.0`。
启动脚本核对工作区与镜像身份，拒绝复用同名但属于其他目录或旧镜像的容器。
重建镜像后，先停止并删除旧容器，再执行 `start`；保留下面的持久卷即可复用缓存。

## 文件与资源

| 内容 | 容器位置 | 保存方式 |
| --- | --- | --- |
| 当前源码工作区 | `/workspace` | 宿主目录绑定挂载，可直接编辑 |
| Cargo 下载缓存 | `/home/developer/.cargo` | `lattice-dev-cargo` 命名卷 |
| 构建产物与验证记录 | `/workspace/target` | `lattice-dev-target` 命名卷 |

容器使用普通用户，通过 `keep-id` 将文件写入映射到宿主当前用户。
源码挂载使用 SELinux `:Z` 标签，仅挂载本工作区；宿主原有 `target` 被独立卷遮住，宿主编译产物不参与容器构建。
停止或删除容器不会删除源码和命名卷。卷由 Podman 管理，不要把构建记录误认为保存在宿主工作区的 `target`。

默认配置为 2 CPU、5 GiB 内存上限、4096 PID、2 个构建并发和 2 个测试线程。
debug/test 关闭调试符号，release 保持 Cargo 默认设置。
运行时保留默认 seccomp 和 SELinux，使用 init 回收子进程，并丢弃 capability、禁止获取新权限。
Git 提交身份、远程认证与开发者凭据遵循项目既有授权要求，镜像不包含这些凭据。

## 验证与记录

`verify` 先检查普通用户、`openat2`、Unix socket、PTY、localhost 和卷写入，
再顺序执行宿主构建、完整 workspace 测试、崩溃屏障、架构边界、工具生成检查、
WASM 重建、release 构建及 release 操作员场景。它会恢复普通 debug 产物。
场景预算与断言沿用仓库，检查失败以非零退出。

`tool-build --verify` 按仓库现有规则将摘要漂移报告为警告，其他构建和完整性错误仍失败。
当前 guest 构建会嵌入 Cargo registry 的绝对路径；容器的 `/home/developer/.cargo`
与既有产物的 `/home/astrin/.cargo` 不同，可能导致摘要漂移。此警告不能当作逐字节复现成功，
也不应为消除警告而覆盖仓库里的组件与摘要。具体规则见[验证指南](../../docs/development/validation-guide.md)。

每次验证使用独立目录，最新路径保存于容器内 `/workspace/target/container-check-latest`。
目录内保留日志、退出码、`time -v` 指标、失败材料和 release 校验和。
一次完整验证较重，日常修改按[测试工程指南](../../docs/development/testing.md)选择必要范围。

```sh
bash dev/container/dev.sh exec cat target/container-check-latest
podman cp lattice-dev:/workspace/target/<记录目录名> ./container-evidence
```

容器内的后台、网络和进程组行为必须实际验证，宿主验证不能代替。
实现依据：[Podman run](https://docs.podman.io/en/latest/markdown/podman-run.1.html)、
[官方 Rust 镜像](https://github.com/rust-lang/docker-rust)、
[wasm-tools 1.254.0](https://github.com/bytecodealliance/wasm-tools/releases/tag/v1.254.0)。
