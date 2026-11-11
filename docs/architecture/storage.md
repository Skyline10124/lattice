---
status: active
---

# 私有存储与持久化

> 类型：架构合同
> 上位依据：[项目宪章](../governance/charter.md)、[宿主平台](platform.md)
> 决策记录：[ADR 0006](../adr/0006-tree-session-and-global-redb-store.md)

## 目的与范围

本合同规定私有状态寻址、会话库的使用条件、创建与打开、锁和持久性验证边界。`lattice-store` 提供状态布局、Linux 文件锁、`fsync` 与位置能力探测，不依赖其他项目 crate，会话库表与事务由 `lattice-session` 维护。

私有状态目录下的存储是会话库 `lattice.redb`：全局一份，保存 Session、会话树（Step 节点内含调用槽位）、会话设置、配置表与各索引，由 `lattice-session` 维护。

会话记录与事务序列见[会话合同](session/README.md)，调用槽位推进与结果载荷见[调用槽位](session/steps.md#调用槽位)。

## 状态布局与身份

### 私有状态根

可变私有存储位于 XDG state root 下，不使用配置目录或项目 `.git`。`XDG_STATE_HOME` 为绝对路径时采用该值，否则使用绝对 `HOME` 下的 `.local/state`；两者均不可用时返回 `MissingStateRoot`。

| 相对 `<state-root>/lattice/` 的位置 | 归属与作用 |
| --- | --- |
| `lattice.redb` | `lattice-session`，全局会话库 |
| `locks/sessions/<SessionId>.lock` | 会话操作锁 |

打开会话库时，若 `lattice/` 下存在 `sessions.git` 或 `projects/*/invocations.git`，打开失败并在错误中列出这些路径，由人删除或存档。

私有状态位于 Local Workspace 外部。删除工作区或退回会话位置不级联删除其他领域的私有状态。配置私有状态位置时应核对其与工作区的实际关系，XDG 路径解析本身不提供包含关系隔离。

### 本地后台与访问边界

同一有效用户在同一规范状态根下共用一个本地后台，`STATE_ROOT` 指上述 XDG 状态根。后台使已接受任务在终端关闭后继续运行，其他客户端仍可连接查询或观察。后台可同时服务多个项目和 Session；Session 的项目绑定保持不变。

| 控制资源 | 作用与生命周期 |
| --- | --- |
| `<state-root>/lattice/local/owner.lock` | 后台终身持有其非阻塞独占 flock，排除同作用域的第二个后台；内核在进程退出后释放锁 |
| `local/api.sock` | 供本机客户端建立 Unix socket 连接，由实例锁持有者绑定和清理 |
| 后台 epoch | 标识一次后台生命周期，用于判定观察连续性；随进程重启改变 |
| 会话库句柄、Owner 和内存观察状态 | 后台持有并用于执行、控制和回答请求；持久业务事实保存在会话库中 |

后台生命周期依次为：

1. 首次使用时，客户端可自动启动当前 `lattice` 的后台主体，以独立进程组运行并脱离终端 stdio。
2. `serve` 取得实例锁，核验本地目录与 socket；需要处理残留 socket 时，仅清理本用户、身份未变化且已失效的对象，再绑定服务地址。
3. 通信双方完成身份和协议检查后，Backend 接管请求，并通过领域接口打开存储、管理独立任务。最后一个客户端断开后，后台继续运行。
4. 显式 stop 关闭新准入并等待已接管操作收尾，具体查询、取消和排空责任见[停止后台](session/recovery.md#停止后台)。
5. 后台正常退出时由实例锁持有者清理自己的 socket 并释放资源。锁文件可以留存，是否持锁以 flock 为准；异常退出留下的 socket 由下一持锁者按第 2 步核验处理。

请求接受、执行进度与请求身份由会话库的节点与 TurnId 索引保存，见[请求身份与找回](session/recovery.md#请求身份与找回)。进程控制资源不构成业务状态库；后台启动保持领域库原样，执行者消失的 Turn 停在已写入的进度，见[执行者消失后的 Turn](session/recovery.md#执行者消失后的-turn)。

#### 本地访问与所有权边界

- 本地目录及 socket 核验类型、所有者和私有权限，通信双方均核对 Linux peer UID。IPC 限于本机 Unix socket；身份检查或版本不兼容时拒绝连接，不开放远程入口。
- 只有实例锁持有者可绑定、替换或清理 socket，也只有它打开会话库。不按 PID、mtime、连接数或超时抢占所有权，也不终止身份或协议不兼容的已有后台；客户端报告错误，交由用户处理。
- 版本化帧、长度检查和有界队列限制请求与观察资源。实现入口为[本地 IPC](../../crates/lattice-api/src/local/ipc.rs)，具体计量及超限行为见[输入与传输限制](session/concurrency.md#输入与传输限制)。
- 后台启动不创建或修复缺失领域库，也不写入未关闭的 Turn。常驻句柄继续遵守各领域的身份、严格打开和发布规则，不增加第二份请求状态库。
- 单例锁只排除第二个后台，不串行化各 Session 的 Turn。短准入管理与领域锁分开，常驻持库不改变[锁序与持有期限](#锁与并发语义)。

### 项目绑定

Session 记录保存完整 Local Workspace 根的规范项目路径原始字节（[`ProjectPath`](../../crates/lattice-session/src/model/project_path.rs)）。路径由 [`root.rs`](../../crates/lattice-api/src/root.rs) 经 `canonicalize` 解析符号链接得到，按 Linux `OsStrExt` 字节保存，校验以 `/` 开头且不含 NUL 字节，不要求路径为 UTF-8。

在会话库、本地 IPC（含 `configure`、`submit`、`rewind` 请求中的 `workspace`）与 `--json` 结构化输出中，路径表示为每项 `0..=255` 的 JSON 整数数组；面向人阅读的输出与诊断信息中的路径使用有损 UTF-8 文本，非 UTF-8 字节显示为替换字符。

同一规范路径对应同一项目绑定，项目移动到其他路径后得到不同路径，需要创建新 Session；无关项目复用原路径时得到同一路径，可以续写绑定在该路径上的 Session。该规则不使用内容指纹、Git remote、inode 或 generation 注册表识别项目，也不自动 relink。

## 会话库

### 使用条件与持久性

会话库使用嵌入式事务库 redb，由后台进程独占打开。它提供的保证与本项目依赖的使用条件如下：

| 条件 | 要求与理由 |
| --- | --- |
| 版本 | redb ≥ 4.3.0。4.2.0 存在已知恢复缺陷：`check_integrity()` 修复过的库在提交后不久崩溃，恢复时可能静默回退该提交；4.3.0 已修复 |
| 后端 | 持久化文件后端。内存后端的同步不履行持久化语义 |
| 打开模式 | 默认独占写模式，不启用 `experimental-multiprocess`。该模式下读者直接读文件头，可能早于最后一次同步看到新提交 |
| 持久性 | 所有写事务使用 `Durability::Immediate`，不使用 `Durability::None`。`None` 提交先对读者可见而不保证持久 |

满足上述条件时：

- **原子性**：一个写事务内的全部记录变更一起生效或一起不生效。
- **持久性**：`commit` 返回成功时事务已持久。redb 先写入新根并完成最后一次 `sync_data`，之后才替换读者使用的共享头。
- **可见性**：读事务只看到已提交且已持久的状态，并在整个读事务中保持同一快照。提交可能在 `commit` 返回前就对新读事务可见，此时数据已持久。
- **单写者**：同一时刻只有一个写事务；另一写者等待。
- **提交失败**：`commit` 返回 `TransactionPoisoned` 时事务已回滚，库仍可开始新事务，但引起它的存储错误不因回滚而修复；其他错误时事务可能已持久，不能当作已回滚，库随后拒绝新的写事务，须关闭并重新打开。会话领域对两类采用同一保守策略，见[事务与一致性](session/concurrency.md#事务与一致性)。

redb 打开时对库文件取独占锁，另一进程打开得到 `DatabaseAlreadyOpen`。平台不支持文件锁时 redb 只告警并继续，所以单进程持有仍由后台实例锁保证，不依赖 redb 的锁。

这些保证依据 redb 4.2.0 与 4.3.0 源码核对的提交与可见顺序。它不是对磁盘、文件系统或掉电持久性的资格化；进程崩溃测试与成功的同步返回都不能单独作为真实硬件掉电恢复的实验结论。

### 身份、版本与打开

会话库保存一张元信息表，记录库格式版本（当前为 `9`）；各记录的值带同一格式版本。表定义、值格式及其演化由 `lattice-session` 维护（[ADR 0009](../adr/0009-remove-legacy-lancet-tool-executor.md#4-会话库升版与存储收敛)、[ADR 0012](../adr/0012-backend-owns-session-settings.md)）。

| 打开时的状态 | 处理 |
| --- | --- |
| 存在 `sessions.git` 或 `projects/*/invocations.git` | 拒绝打开，错误列出路径 |
| 库文件不存在 | 返回未初始化；是否创建由调用路径决定，见[首次创建](#首次创建) |
| 不是 redb 库、元信息缺失或格式版本不符 | 拒绝打开并报告损坏或版本不兼容 |
| 记录无法解码或违反业务不变量 | 领域返回类型化损坏，停止受影响路径 |

打开不初始化、不修复我们自己的记录，不回退旧路径，也不替换为空库。redb 在异常退出后打开时执行的自身崩溃恢复属于库的提交协议，不是记录修复。不符合当前格式的库需要另行获准处理。

### 首次创建

会话库只在 `configure(New)` 创建新会话的路径上首次创建，创建后再执行新会话写入事务。其他操作遇到库文件不存在时返回未初始化，不创建。

创建按以下顺序执行：

1. 对实际父目录执行[能力探测](#能力探测)，取得父目录中的目标专属 create lock。
2. 若目标已存在，严格打开并验证其身份，跳过初始化。
3. 在同一父目录建立临时库文件，写入元信息与空表，提交并同步。
4. 通过 `renameat2(RENAME_NOREPLACE)` 发布目标名称，同步父目录，再打开并核验已发布身份。

发布前失败时目标尚未发布，临时文件可能残留；后续创建者在持有 create lock 且目标仍不存在时清理残留临时文件。名称发布成功后父目录同步失败，目标已可见但持久性未确认，重试应验证原目标并重做父目录同步，不能删除后重新创建。

## 锁与并发语义

操作锁覆盖领域操作，会话库写事务覆盖一步会话状态变化。需要多类锁的操作遵循以下顺序，仅取得本次操作需要的锁：

```text
SessionExecutionLock（会话绑定操作）
  → 会话库写事务
```

不持有会话库写事务反向取得操作锁，不升级共享锁。会话库写事务内不等待模型、Tool、网络或其他锁，不跨 await 持有。[会话合同](session/concurrency.md#操作锁)维护会话操作的持有期限；只读会话查询只用会话库读事务，不取得操作锁。

| 锁类别 | 同进程语义 |
| --- | --- |
| 操作锁 | 每次获取都是独立操作；共享可并存，独占与其他操作冲突。不支持在已持有 guard 时嵌套获取 |
| 会话库首次创建锁 | 严格互斥、不可重入、阻塞等待者按票号 FIFO、非阻塞获取不插队 |
| 会话库写事务 | 由 redb 保证同一时刻只有一个；写者等待时间取决于其他写事务的长度，所以每个写事务只覆盖一步状态变化 |

进程级 registry 只仲裁本进程的持有和等待关系，跨进程互斥由 Linux flock 实现。FIFO 保证限定于同进程严格锁等待者，不扩展为跨进程调度顺序或固定等待时限。

guard 是显式持有凭据，不由线程 ID 或任务 ID 推定所有权。最后一个 guard 释放 OS 锁，进程退出由内核释放；没有基于 PID、mtime、心跳或超时的抢锁协议。

同一锁必须由一致的 StateLayout 路径寻址，不通过符号链接或多种拼写把同一锁文件注册为不同锁。锁不会阻止未遵循协议的外部写者；LATTICE 不读写工作区业务文件。

## 保留、索引与演化

已使用会话的树只追加、不删除，只有末端未关闭的 Step 节点会被改写，配置表只增不改；草稿会话在满足清理条件时由后台整体删除，同一事务删除 Session 记录、根节点与会话设置三项，资格条件见[会话状态与草稿清理](session/concurrency.md#会话状态与草稿清理)。本阶段没有压缩或备份机制。全部会话集中在一个库文件，文件损坏影响所有 Session；redb 自身提供崩溃恢复与完整性检查。

会话库中的 TurnId 索引与 InvocationId 索引是派生记录，与其权威记录在同一写事务中更新，不另设修复流程；两者不一致视为损坏。

会话库格式与各领域 schema 分别演化。格式、目录拓扑、事务序列或屏障变化须核验既有数据的打开、可达性及各领域写入接口，破坏性处理须先说明影响并获授权。历史性能与故障证据按[测试工程的证据保留要求](../development/testing.md#脱敏与完整性)处理。

## 位置能力与验证

### 能力探测

能力按实际发布位置验证，不由文件系统名称或一次主机检查代替。检查覆盖同挂载点的 sibling publication、`renameat2(RENAME_NOREPLACE)`、文件与目录 fsync 以及本地 flock。会话库的首次创建对实际父目录执行这些探测。

严格打开只验证已有存储，不自动运行创建型探测。诊断入口 `lattice-store-probe` 会建立临时目录与文件，属于有副作用的验证，应在明确允许的位置运行。

ADR 0005 规定 Arch Linux/Btrfs 为完整正确性、故障注入及性能维护环境；Rocky/ext4 使用预构建轻量探测进行补充核验。其他位置通过同样探测可以按 Unverified 运行，不因此扩大平台维护承诺。

SIGKILL 后重开证明相应进程崩溃场景。文件系统探测、进程崩溃测试和成功 fsync 返回均不能单独作为真实硬件掉电恢复的实验结论。redb 是被使用的成熟库，不做性能资格化；自有代码验证记录层的正确性。

### 验证入口与承接

| 检查对象 | 入口 |
| --- | --- |
| 会话库打开、创建、格式版本，以及存在 `sessions.git` 或 `invocations.git` 时的拒绝 | `cargo test -p lattice-session` |
| 会话库的崩溃与重开 | 真实二进制与真实状态根上的 E2E 场景，按[测试工程合同](../development/testing.md)选择 |
| 寻址、状态布局、锁、`fsync` 与能力探测 | `cargo test -p lattice-store` |
| 内核静态检查 | `cargo clippy -p lattice-store --all-targets -- -D warnings` |
| 依赖边界 | `cargo tree -p lattice-store --edges normal`、`cargo xtask harness-boundary` |
| 会话的新建与续写不在 `projects/` 下创建或改动内容 | `cargo test -p lattice-api --test session_without_workspace_history` |
| 实际存储位置能力 | 在获准目录运行预构建的 `lattice-store-probe` |

纯文档修订核对接口责任、锁范围、来源、状态及图示。修改内核或会话库接线时先检查直接故障窗口，再验证使用该机制的领域，执行资源和被测产物按[测试工程合同](../development/testing.md)处理。

承接来源为 ADR 0005 的项目寻址、通用内核、操作锁和位置能力部分，[ADR 0006](../adr/0006-tree-session-and-global-redb-store.md) 的存储布局与会话库使用条件，以及 [ADR 0009](../adr/0009-remove-legacy-lancet-tool-executor.md#4-会话库升版与存储收敛)、[ADR 0012](../adr/0012-backend-owns-session-settings.md) 的会话库升版与存储收敛。机制接口与实现核对位置为 [`layout.rs`](../../crates/lattice-store/src/layout.rs)、[`durability.rs`](../../crates/lattice-store/src/durability.rs)、[`lock.rs`](../../crates/lattice-store/src/lock.rs)及 [`probe.rs`](../../crates/lattice-store/src/probe.rs)。

创建残留的完整报告与跨位置能力覆盖仍需核验。
