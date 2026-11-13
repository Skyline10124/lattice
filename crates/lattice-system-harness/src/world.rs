// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::ffi::OsStr;
use std::net::TcpListener;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;

use std::time::Duration;

use crate::daemon::{DaemonError, DaemonOptions, ScenarioDaemon};
use crate::driver::{CommandOutcome, LatticeDriver};
use crate::scenario::{ScenarioBudget, ScenarioSpec};

use thiserror::Error;

/// Ask the OS for a free ephemeral port by binding to `127.0.0.1:0` and then
/// dropping the listener.
///
/// This replaces the previous in-process `AtomicU16` counter, which only
/// advanced a process-local cursor from 18000 and never reserved anything from
/// the OS — so two parallel `cargo test` invocations (or any other process
/// holding that port) could collide. The OS-assigned port is unique at
/// reservation time. There is an inherent TOCTOU window between dropping the
/// listener and the scenario reusing the port; for a test harness this is the
/// standard accepted trade-off.
///
/// Returns an error instead of falling back to 0: a `ScenarioWorld` built with
/// port 0 would violate the scenario-local-port isolation contract (and a
/// caller that never inspects `port()` would silently proceed), so a bind
/// failure must surface as a `WorldError` and abort world construction.
fn os_free_port() -> Result<u16, std::io::Error> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    Ok(port)
}

/// Isolated external world for one harness scenario.
pub struct ScenarioWorld {
    root: Option<tempfile::TempDir>,
    home: PathBuf,
    project: PathBuf,
    session_store: PathBuf,
    port: u16,
    allowed_env: Mutex<HashMap<String, String>>,
    pub(crate) daemon: Arc<ScenarioDaemon>,
    /// 场景唯一预算及其单步上限；首次 [`Self::budget`] 时登记，后台启动与收尾共享同一起点。
    budget: Arc<BudgetSlot>,
}

/// 场景预算槽：唯一起点加单步上限，世界与场景证据共享同一个实例。
#[derive(Default)]
pub(crate) struct BudgetSlot(Mutex<Option<(ScenarioBudget, Duration)>>);

impl BudgetSlot {
    fn budget(&self, spec: &ScenarioSpec) -> ScenarioBudget {
        let mut slot = self.0.lock().expect("预算锁");
        let (budget, _) = slot.get_or_insert_with(|| {
            (
                ScenarioBudget::new(spec.total_timeout()),
                spec.step_timeout(),
            )
        });
        budget.clone()
    }

    /// 后台启动与就绪轮询的绝对截止时间：取场景剩余预算与 5 秒上限中的较小值。
    pub(crate) fn startup_deadline(&self) -> std::time::Instant {
        let started = std::time::Instant::now();
        let cap = started + crate::daemon::STARTUP_TIMEOUT;
        match &*self.0.lock().expect("预算锁") {
            Some((budget, _)) => budget.deadline(cap),
            None => cap,
        }
    }

    /// 正常 stop 的绝对期限；取锁等待计入单步上限，已登记场景还受原总期限约束。
    pub(crate) fn normal_stop_deadline(&self) -> std::time::Instant {
        let started = std::time::Instant::now();
        match &*self.0.lock().expect("预算锁") {
            Some((budget, step)) => budget.deadline(started + *step),
            None => started + ScenarioSpec::default_step_timeout(),
        }
    }
}

#[derive(Debug, Error)]
pub enum WorldError {
    #[error("failed to create scenario world: {0}")]
    Io(#[from] std::io::Error),
    #[error("fake provider wiring is unavailable: {0}")]
    Provider(&'static str),
}

impl ScenarioWorld {
    /// 创建隔离的 HOME/XDG 与项目目录。
    pub fn new(prefix: &str) -> Result<Self, WorldError> {
        let root = tempfile::TempDir::with_prefix(prefix)?;
        let home = root.path().join("home");
        let project = root.path().join("project");
        // #143: durable session state lives under the XDG *state* root.
        let session_store = home.join(".local/state").join("lattice");
        std::fs::DirBuilder::new().mode(0o700).create(&home)?;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(home.join(".lattice"))?;
        std::fs::write(
            home.join(".lattice/providers.toml"),
            "[metadata]\nauto_refresh = false\n",
        )?;
        std::fs::create_dir_all(&session_store)?;
        std::fs::create_dir_all(project.join(".lattice"))?;

        let port = os_free_port()?;

        let mut allowed_env = HashMap::new();
        if let Ok(path) = std::env::var("PATH") {
            allowed_env.insert("PATH".into(), path);
        }
        allowed_env.insert("HOME".into(), home.display().to_string());
        allowed_env.insert(
            "XDG_CONFIG_HOME".into(),
            home.join(".config").display().to_string(),
        );
        allowed_env.insert(
            "XDG_DATA_HOME".into(),
            home.join(".local/share").display().to_string(),
        );
        allowed_env.insert(
            "XDG_CACHE_HOME".into(),
            home.join(".cache").display().to_string(),
        );
        // 会话持久存储与后台运行锁都通过 Linux XDG state 目录解析，显式指向隔离 HOME 内。
        allowed_env.insert(
            "XDG_STATE_HOME".into(),
            home.join(".local/state").display().to_string(),
        );
        // 后台启动时经 rustls-native-certs 读取信任根；设了这两个变量就只信任它们指向的根。
        // 场景默认只信任 harness CA，fake provider 默认用它签发的证书。
        let trust = root.path().join("tls-trust");
        std::fs::create_dir_all(trust.join("empty"))?;
        std::fs::write(
            trust.join("ca.pem"),
            crate::tls::TlsIdentity::shared()?.ca_pem(),
        )?;
        allowed_env.insert(
            "SSL_CERT_FILE".into(),
            trust.join("ca.pem").display().to_string(),
        );
        allowed_env.insert(
            "SSL_CERT_DIR".into(),
            trust.join("empty").display().to_string(),
        );

        Ok(Self {
            daemon: Arc::new(ScenarioDaemon::new(home.join(".local/state"))),
            root: Some(root),
            home,
            project,
            session_store,
            port,
            allowed_env: Mutex::new(allowed_env),
            budget: Arc::new(BudgetSlot::default()),
        })
    }

    pub fn root(&self) -> &Path {
        self.root.as_ref().expect("scenario root").path()
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn project_dir(&self) -> &Path {
        &self.project
    }

    /// The private-state root holding the global `lattice.redb`, the
    /// per-project stores, and the lock tree.
    pub fn session_store_dir(&self) -> &Path {
        &self.session_store
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn set_env(&self, key: impl Into<String>, value: impl Into<String>) {
        self.allowed_env
            .lock()
            .expect("环境白名单锁")
            .insert(key.into(), value.into());
    }

    /// 从允许列表移除变量，使子进程中该变量不存在。
    /// 将值设为空串仍表示变量已定义，不能代替不存在状态。
    pub fn unset_env(&self, key: &str) {
        self.allowed_env.lock().expect("环境白名单锁").remove(key);
    }

    pub fn init_git_project(&self) -> Result<(), WorldError> {
        let root = &self.project;
        gix::init(root).map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?;
        // Point HEAD at main before first commit (gix::init already does this).
        self.commit_all("harness init")?;
        Ok(())
    }

    pub fn write_project_file(&self, relative: &str, contents: &str) -> Result<(), WorldError> {
        let path = self.project.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
        Ok(())
    }

    /// 向项目内 `.lattice/agents/<name>/agent.toml` 写入最小 agent profile。
    ///
    /// 因 `cargo xtask harness-boundary` 禁止依赖任何产品 crate，TOML 直接手写，与用户实际编写的
    /// 文件一致。`[lancet]` 与 `[tools]` 已移除；需要假工具的场景在后台启动前通过
    /// [`Self::set_env`] 设置 `LATTICE_TEST_TOOLS`。
    pub fn write_agent_profile(&self, name: &str, model: &str) -> Result<(), WorldError> {
        let toml = format!(
            "schema_version = 1\n\n[agent]\nname = \"{name}\"\nmodel = \"{model}\"\n\n[prompt]\nsystem = \"harness agent\"\n"
        );
        self.write_project_file(&format!(".lattice/agents/{name}/agent.toml"), &toml)
    }
    /// Spawn a subprocess with cleared environment and only the allow-listed vars.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.project);
        cmd.env_clear();
        for (key, value) in self.allowed_env.lock().expect("环境白名单锁").iter() {
            cmd.env(key, value);
        }
        cmd
    }

    /// Commit all current worktree files (skipping `.git`) onto `refs/heads/main`.
    pub fn commit_all(&self, message: &str) -> Result<(), WorldError> {
        let root = &self.project;
        let mut repo = gix::open_opts(root, gix::open::Options::isolated())
            .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?;
        {
            let mut cfg = repo.config_snapshot_mut();
            cfg.set_raw_value("user.name", "Lattice Harness")
                .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?;
            cfg.set_raw_value("user.email", "harness@test")
                .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?;
            let _ = cfg
                .commit()
                .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?;
        }
        let mut files: Vec<(String, String)> = Vec::new();
        collect_files(root, root, &mut files);
        let empty = gix::ObjectId::empty_tree(repo.object_hash());
        let base = match repo.head_id() {
            Ok(id) => repo
                .find_commit(id)
                .ok()
                .and_then(|c| c.tree_id().ok())
                .map(|t| t.detach())
                .unwrap_or(empty),
            Err(_) => empty,
        };
        let mut editor = repo
            .edit_tree(base)
            .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?;
        for (rel, content) in &files {
            let blob = repo
                .write_blob(content.as_bytes())
                .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?
                .detach();
            editor
                .upsert(rel, gix::objs::tree::EntryKind::Blob, blob)
                .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?;
        }
        let tree_id = editor
            .write()
            .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?
            .detach();
        let parents: Vec<gix::ObjectId> = match repo.head_id() {
            Ok(id) => vec![id.detach()],
            Err(_) => Vec::new(),
        };
        let sig = gix::actor::Signature {
            name: "Lattice Harness".into(),
            email: "harness@test".into(),
            time: gix::date::Time::now_utc(),
        };
        let mut time_buf = gix::date::parse::TimeBuf::default();
        let sig_ref = sig.to_ref(&mut time_buf);
        let commit = repo
            .new_commit_as(sig_ref, sig_ref, message, tree_id, parents.iter().copied())
            .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?;
        let constraint = match parents.first() {
            Some(p) => gix::refs::transaction::PreviousValue::MustExistAndMatch(
                gix::refs::Target::Object(*p),
            ),
            None => gix::refs::transaction::PreviousValue::MustNotExist,
        };
        repo.reference("refs/heads/main", commit.id, constraint, "harness commit")
            .map_err(|e| WorldError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }

    /// Backward-compatible shim used by a few scenarios that previously shelled
    /// out to `git add`/`git commit`. Only `add`+`commit` sequences are
    /// supported; other git subcommands are rejected.
    pub fn git(&self, args: &[&str]) -> Result<(), WorldError> {
        match args {
            ["add", ..] => Ok(()), // staging is implicit in commit_all
            ["commit", "-m", msg] | ["commit", "--allow-empty", "-m", msg] => self.commit_all(msg),
            other => Err(WorldError::Io(std::io::Error::other(format!(
                "unsupported harness git args: {other:?} (use commit_all)"
            )))),
        }
    }

    pub fn env_summary(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<_> = self
            .allowed_env
            .lock()
            .expect("环境白名单锁")
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }

    pub fn real_home_leak_check(&self) -> Result<(), String> {
        let real_home = std::env::var("HOME").unwrap_or_default();
        if real_home.is_empty() {
            return Ok(());
        }
        let canonical_real = std::fs::canonicalize(&real_home).ok();
        let canonical_test = std::fs::canonicalize(&self.home).ok();
        if canonical_real.is_some() && canonical_real == canonical_test {
            return Err("scenario HOME resolved to the developer HOME".into());
        }
        Ok(())
    }

    /// 场景唯一的墙钟预算。首次调用从当前时刻起算并登记；之后无论谁在何时调用
    /// （发送驱动、辅助命令、provider gate 等待）都拿到同一起点的副本，
    /// 正常 daemon stop 也只用它的剩余量。场景内不存在第二个起点。
    pub fn budget(&self, spec: &ScenarioSpec) -> ScenarioBudget {
        self.budget.budget(spec)
    }

    /// 与场景证据共享的预算槽：证据收尾时的普通 stop 也只用同一起点的剩余量。
    pub(crate) fn budget_slot(&self) -> Arc<BudgetSlot> {
        Arc::clone(&self.budget)
    }

    /// 按当前环境白名单快照显式启动场景后台并等待就绪。
    pub fn start_daemon(&self) -> Result<(), DaemonError> {
        self.start_daemon_with(&DaemonOptions::default())
    }

    /// 按当前环境白名单快照叠加选项覆盖，显式启动场景后台并等待就绪。
    pub fn start_daemon_with(&self, options: &DaemonOptions) -> Result<(), DaemonError> {
        let driver = LatticeDriver::from_env()?;
        let mut snapshot = self.allowed_env.lock().expect("环境白名单锁").clone();
        options.apply_env(&mut snapshot);
        self.daemon.start(
            driver.binary_path(),
            &self.project,
            snapshot,
            options,
            self.budget.startup_deadline(),
        )
    }

    /// 停止当前后台并确认其完全退出后，按当前环境白名单快照重新启动后台并等待就绪。
    pub fn restart_daemon(&self) -> Result<(), DaemonError> {
        self.restart_daemon_with(&DaemonOptions::default())
    }

    /// 停止当前后台并确认其完全退出后，按指定选项与环境重新启动后台并等待就绪。
    pub fn restart_daemon_with(&self, options: &DaemonOptions) -> Result<(), DaemonError> {
        self.stop_daemon().map_err(DaemonError::StopFailed)?;
        self.start_daemon_with(options)
    }

    /// 显式停止并清理自有后台，同时检查无残留后台；绝不隐式重启。
    pub fn stop_daemon(&self) -> Result<(), Vec<String>> {
        self.daemon.cleanup(self.budget.normal_stop_deadline())
    }

    /// 场景显式 `daemon stop` 返回后立即调用：核验 `owner.lock` 与 `api.sock` 已释放，
    /// 并在有界窗口内等待自有后台组退空。
    pub fn daemon_departed(&self) -> Result<(), Vec<String>> {
        self.daemon.departed()
    }

    /// 返回当前存活的自有后台组首领列表。
    pub fn daemon_pids(&self) -> std::io::Result<Vec<u32>> {
        self.daemon.owners()
    }

    /// 返回当前受管理后台的进程组号。
    pub fn daemon_pgid(&self) -> Option<u32> {
        self.daemon.pgid()
    }

    /// 向自有后台进程组发送 `SIGTERM`。
    pub fn terminate_daemon(&self) -> Result<(), DaemonError> {
        self.daemon.terminate()
    }

    /// 向自有后台进程组发送 `SIGINT`。
    pub fn interrupt_daemon(&self) -> Result<(), DaemonError> {
        self.daemon.interrupt()
    }

    /// 非阻塞检查自有后台直接子进程是否已退出。
    pub fn daemon_has_exited(&self) -> Result<bool, DaemonError> {
        self.daemon.has_exited()
    }

    /// 等待自有后台直接子进程退出（或取出最近一次已回收的退出结果）。
    pub fn wait_daemon(&self, timeout: Option<Duration>) -> Result<CommandOutcome, DaemonError> {
        self.daemon.wait(timeout)
    }

    /// 向自有后台进程组发送 `SIGKILL` 并回收退出结果。
    pub fn kill_daemon(&self) -> Result<CommandOutcome, DaemonError> {
        self.daemon.kill_and_wait()
    }

    /// 读取当前运行中（或最近一次已退出）自有后台的标准错误字节快照。
    pub fn daemon_stderr(&self) -> Vec<u8> {
        self.daemon.stderr_snapshot()
    }

    /// 取出最近一次已回收的自有后台 `CommandOutcome`。
    pub fn take_daemon_outcome(&self) -> Option<CommandOutcome> {
        self.daemon.take_outcome()
    }
}

impl Drop for ScenarioWorld {
    fn drop(&mut self) {
        if let Err(errors) = self.stop_daemon() {
            // 未确认清理完成时保留自有状态，避免留下失去数据根的后台。
            if let Some(root) = self.root.take() {
                let _ = root.keep();
            }
            // 清理不能覆盖原断言；展开期间只报告，不二次 panic。
            if std::thread::panicking() {
                eprintln!("scenario daemon cleanup: {errors:?}");
            } else {
                panic!("scenario daemon cleanup: {errors:?}");
            }
        }
    }
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, out);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .into_owned();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            out.push((rel, content));
        }
    }
}
