// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::ffi::OsStr;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;

use std::time::Duration;

use crate::daemon::ScenarioDaemon;
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
    /// 场景唯一预算及其单步上限；首次 [`Self::budget`] 时登记，正常 daemon stop 也用它的剩余量。
    budget: Arc<BudgetSlot>,
}

/// 场景预算槽：唯一起点加单步上限，世界与场景证据共享同一个实例。
#[derive(Default)]
pub(crate) struct BudgetSlot(Mutex<Option<(ScenarioBudget, Duration)>>);

impl BudgetSlot {
    fn budget(&self, spec: &ScenarioSpec) -> ScenarioBudget {
        let mut slot = self.0.lock().expect("budget lock");
        let (budget, _) = slot.get_or_insert_with(|| {
            (
                ScenarioBudget::new(spec.total_timeout()),
                spec.step_timeout(),
            )
        });
        budget.clone()
    }

    /// 正常 stop 的绝对期限；取锁等待计入单步上限，已登记场景还受原总期限约束。
    pub(crate) fn normal_stop_deadline(&self) -> std::time::Instant {
        let started = std::time::Instant::now();
        match &*self.0.lock().expect("budget lock") {
            Some((budget, step)) => budget.deadline(started + *step),
            None => started + ScenarioSpec::default_step_timeout(),
        }
    }
}

#[derive(Debug, Error)]
pub enum WorldError {
    #[error("failed to create scenario world: {0}")]
    Io(#[from] std::io::Error),
}

impl ScenarioWorld {
    /// Build an isolated HOME/XDG/project tree with an empty project-root `.env`.
    pub fn new(prefix: &str) -> Result<Self, WorldError> {
        let root = tempfile::TempDir::with_prefix(prefix)?;
        let home = root.path().join("home");
        let project = root.path().join("project");
        // #143: durable session state lives under the XDG *state* root.
        let session_store = home.join(".local/state").join("lattice");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&session_store)?;
        std::fs::create_dir_all(project.join(".lattice"))?;
        std::fs::write(project.join(".env"), "")?;

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
        // Must be explicit because the audit database and private workspace
        // history resolve through the Linux XDG state directory. Scenarios
        // name that path directly, so keep it inside the isolated HOME.
        allowed_env.insert(
            "XDG_STATE_HOME".into(),
            home.join(".local/state").display().to_string(),
        );
        allowed_env.insert("LATTICE_AGENTS_DIR".into(), String::new());

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

    /// The private-state root holding `sessions.git`, the per-project stores,
    /// and the lock tree (ADR 0005 §3).
    pub fn session_store_dir(&self) -> &Path {
        &self.session_store
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn set_env(&self, key: impl Into<String>, value: impl Into<String>) {
        self.allowed_env
            .lock()
            .expect("allowed_env lock")
            .insert(key.into(), value.into());
    }

    /// Drop a key from the allow-list so the child process does not see it at
    /// all — distinct from `set_env(key, "")`, which still defines the variable.
    ///
    /// Needed by `.env` trust-boundary scenarios: a key the parent already
    /// defines can never be injected from a `.env` (non-override load), so
    /// asserting that the boundary refused it requires the key to be genuinely
    /// absent from the process environment.
    pub fn unset_env(&self, key: &str) {
        self.allowed_env
            .lock()
            .expect("allowed_env lock")
            .remove(key);
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

    /// Write a minimal agent profile to `.lattice/agents/<name>/agent.toml` in
    /// the project, consolidating the agent-profile fixture that would
    /// otherwise be hand-rolled in every scenario (issue #45 (Codeberg #23)
    /// fixture consolidation). The TOML is hand-written rather than produced
    /// via `lattice-profile` because the harness boundary
    /// (`cargo xtask harness-boundary`) forbids depending on any product
    /// crate; the product reads the same file a user would write.
    ///
    /// The profile is the Direct-Local end-state shape: no `[workspace]`
    /// section (removed in #62, Codeberg #40), no enabled tools. Turn
    /// scenarios that need catalog tools write their own profile via
    /// `write_project_file`.
    pub fn write_agent_profile(&self, name: &str, model: &str) -> Result<(), WorldError> {
        let toml = format!(
            "schema_version = 1\n\n[agent]\nname = \"{name}\"\nmodel = \"{model}\"\n\n[prompt]\nsystem = \"harness agent\"\n\n[tools]\nenabled = []\n"
        );
        self.write_project_file(&format!(".lattice/agents/{name}/agent.toml"), &toml)
    }
    /// Spawn a subprocess with cleared environment and only the allow-listed vars.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.project);
        cmd.env_clear();
        for (key, value) in self.allowed_env.lock().expect("allowed_env lock").iter() {
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
            .expect("allowed_env lock")
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

    /// 显式收尾用于场景证据；Drop 仍覆盖断言 panic 的路径。预算耗尽时正常 stop 记为失败。
    pub fn stop_daemon(&self) -> Result<(), Vec<String>> {
        self.daemon.cleanup(self.budget.normal_stop_deadline())
    }

    /// 进程控制仅作用于经过产物、状态根和组首领核对的后台。
    pub fn daemon_pids(&self) -> std::io::Result<Vec<u32>> {
        self.daemon.owners()
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
