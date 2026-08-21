// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use lattice_api::{CreateSessionRequest, OpenSessionRequest, SessionApi};

use crate::TuiOptions;

pub(crate) struct SessionContext {
    pub session_id: String,
    pub active_agent: String,
    pub project_branch: String,
}

/// Strict-open an existing session or first-create a new one, mirroring the
/// current `runtime::run` prologue verbatim.
pub(crate) fn open_or_create(options: &TuiOptions) -> anyhow::Result<SessionContext> {
    let layout = lattice_api::StateLayout::resolve().context("resolve state layout")?;
    let session_api = match &options.session_id {
        Some(_) => SessionApi::git_open_layout(&layout).context("open session store")?,
        None => SessionApi::git_create_layout(&layout).context("create session store")?,
    };
    let session = match &options.session_id {
        Some(session_id) => session_api
            .open_session(OpenSessionRequest {
                id: session_id.clone(),
            })
            .with_context(|| format!("open session {session_id}"))?,
        None => {
            let project_branch =
                required_project_branch(options.project_branch.as_deref())?.to_string();
            let workspace_root = lattice_api::resolve_local_workspace_root()
                .context("resolve local workspace root")?;
            session_api
                .create_session(CreateSessionRequest {
                    label: None,
                    active_agent: options.agent.clone(),
                    project_branch,
                    project_store_key: workspace_root.store_key_hex(),
                })
                .context("create chat session")?
        }
    };
    Ok(SessionContext {
        session_id: session.session_id,
        active_agent: session.active_agent_name,
        project_branch: session
            .project_target_branch
            .unwrap_or_else(|| "main".into()),
    })
}

/// Resolve and validate the named agent profile. Fails when
/// `profile.agent.name != active_agent`, same as today.
pub(crate) fn load_profile(
    options: &TuiOptions,
    active_agent: &str,
) -> anyhow::Result<lattice_api::LoadedAgentProfile> {
    let loaded = lattice_api::resolve_named_agent_profile(
        active_agent,
        &options.project_root,
        &options.agents_dir,
    )
    .with_context(|| format!("load agent profile {active_agent}"))?;
    if loaded.profile.agent.name != active_agent {
        anyhow::bail!(
            "agent profile '{}' declares agent.name='{}'; these must match",
            active_agent,
            loaded.profile.agent.name
        );
    }
    Ok(loaded)
}

/// `lattice_api::stream_turn` takes `SessionApi` **by value**
/// (`turn/execute.rs:253`), so a fresh handle is opened per turn exactly as the
/// REPL does today. Do not cache one in `SessionContext`.
pub(crate) fn open_session_api() -> anyhow::Result<lattice_api::SessionApi> {
    let layout = lattice_api::StateLayout::resolve().context("resolve state layout")?;
    SessionApi::git_open_layout(&layout).context("open session store")
}

/// Moved verbatim from `runtime.rs`, together with its three existing tests.
fn required_project_branch(project_branch: Option<&str>) -> Result<&str> {
    match project_branch {
        Some(branch) if !branch.trim().is_empty() => Ok(branch),
        _ => Err(anyhow::anyhow!(
            "--project-branch is required when creating a new TUI session"
        )),
    }
}
