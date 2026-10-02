// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lancet-runtime` —— ADR 0004 的 Component 引擎、封闭组成门禁，以及供
//! LATTICE Source Tool 使用的最小 ToolHost。
//!
//! 本 crate 是 Component 执行基座（#27 slice C，Codeberg #5），负责：
//!
//! - **内嵌 Tool 注册** —— 通过 `inventory` 收集 `EmbeddedTool`（名称 + 字节）。
//!   Tool crate 不能自我批准；只有受信任的 Host 组成步骤可以固定身份。
//! - **封闭组成门禁** —— `ClosedCatalog::compose` 在启动时、提供任何调用之前，
//!   按 Host 固定的 `ExpectedToolIdentity`（描述符摘要、制品摘要、ABI、imports）
//!   校验每个注册项。组成完成后目录不可变。
//! - **Component 引擎** —— 启用 component-model 的 wasmtime，不提供 ambient WASI
//!   （不依赖 `wasmtime-wasi`/`wasi-common`）。只链接带版本的 Tool Host WIT
//!   imports。
//! - **Cell 能力准入** —— 单次调用的各准入阶段（Tool 已启用 → profile deny →
//!   边界谓词）与单操作授权器是 Component Tool 唯一的授权权威。
//! - **ToolHost** —— `host-control`（取消 + 结构化事件），以及自 GitHub #30 起的
//!   `workspace-observe` 领域接口；每个有副作用的 import 都受其 host-bound Cell
//!   与具体目标约束。Tool 只能经由这些 import 触及文件系统；不存在 ambient
//!   authority，guest 也无法在它们之外指名任何路径。
//! - **调用执行** —— 参数校验、Component 实例化、按描述符上限与已准入集合校验
//!   结果 CellRef、结果 schema 校验、Host 计算的上下文度量，以及 Cell 证据记录。
//!   错误映射为结构化的 `CepError` 代码
//!   （`ToolTrap`/`ToolTimeout`/`ToolResourceExhausted`/`CallCancelled`）。
//! - **私有工作区历史** —— [`history::WorkspaceHistory`] 把每个成功安装的
//!   Atomic Local Mutation 追加到项目级 `lattice-store` 内核仓库；该仓库位于
//!   Local Workspace 与用户项目的 `.git` 之外（GitHub #63，于 #143 迁移）。
//! - **具体的 Local Workspace** —— [`workspace::LocalWorkspace`] 与
//!   [`workspace::VisibilityEngine`] 在 GitHub #30 中下移到这里，因为
//!   `workspace-observe` 和 `workspace-mutate` 这两组 Tool Host imports 的全部
//!   文件系统操作都在这里执行。安全路径解析、System Path Exclusion、可见性、
//!   有界读取、原子安装与变更历史共用这一个具体模块。
//!
//! `lancet-runtime` **不**依赖 `lancet-api` 或任何 LATTICE session/agent/profile
//! crate。与 `lancet-authority` 的依赖边指向下层。
pub mod admission;
pub mod bindings;
pub mod catalog;
pub mod composition;
pub mod control;
pub mod error_map;
pub mod evidence;
pub mod execution;
pub mod generated;
pub mod history;
pub mod host;
pub mod host_mutate;
pub mod host_network;
pub mod host_workspace;
pub mod limits;
pub mod runtime;
pub mod workspace;

#[cfg(feature = "test-utils")]
pub mod test_helpers;

pub use admission::{
    AdmissionDenial, AdmissionDenialReason, AdmittedTool, OperationDenial, PolicyValidationError,
    admit_tool, authorize_operation, validate_policy_against_catalog,
};
pub use catalog::{CatalogSelectionError, ToolDefinition, enabled_tool_definitions};
pub use composition::{ClosedCatalog, CompositionError, EmbeddedTool, ExpectedToolIdentity};
pub use control::{Cancellation, CancellationSender, EventSink, ExecutionReceipt};
pub use evidence::CellEvidenceLedger;
pub use execution::RuntimeExecution;
pub use history::{
    HistorySnapshot, MutationRecord, RecordedMutation, WorkspaceHistory, WorkspaceHistoryBacking,
    WorkspaceHistoryError,
};
/// Lock and addressing vocabulary from the durability kernel, re-exported so
/// callers holding a runtime handle need no direct kernel dependency
/// (ADR 0005 §5).
pub use lattice_store::{Blocking, FlockGuard, LockMode, ProjectStoreKey, StateLayout};

pub use host_mutate::{
    CreateInstalled, FileBytes, MutateError, MutationCallContext, ReplaceInstalled,
    WorkspaceMutationHost, mutate_cep_error,
};
pub use host_network::{BudgetRemaining, NetworkFetchHost};
pub use host_workspace::{
    BudgetCharge, BudgetRefusal, ObservationCallContext, ObservationError,
    WorkspaceObservationHost, observation_cep_error,
};
pub use limits::ExecutionLimits;
pub use runtime::{ComponentCallContext, ComponentRuntime};
pub use workspace::{
    CreatedFile, DirectoryEntry, FindRequest, GrepRequest, InstalledFile, LocalWorkspace,
    LocalWorkspaceError, MAX_WORKSPACE_WRITE_BYTES, PathFacts, PathIntent, PathKind,
    VisibilityEngine, WorkspacePath, content_hash,
};
