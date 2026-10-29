// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 后台按项目与配置目录读取 profile，并冻结提示与有效策略。

use crate::session::{ApiError, Result};
use lattice_profile::{
    LancetNetworkSection, LancetObservationSection, LancetProjectRootSection,
    LancetWorkspacePolicySection,
};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub use lattice_profile::LoadedAgentProfile;

/// 配置目录来自后台启动环境；项目根已经与会话绑定核对。
pub(crate) fn load_for_turn(
    agent_name: &str,
    workspace: &crate::LocalWorkspaceRoot,
    configuration_dir: &Path,
) -> Result<LoadedAgentProfile> {
    let auth = lattice_llm::auth::AuthStore::new(configuration_dir.to_owned());
    let reject =
        |message: &str| ApiError::Configuration(crate::ConfigRejection::new(message, Some(&auth)));
    let paths = lattice_profile::DiscoveryPaths {
        project_root: workspace.canonical_path().to_owned(),
        global_agents_dir: configuration_dir.join("agents"),
    };
    let mut loaded = lattice_profile::resolve_agent_profile(
        lattice_profile::ProfileSource::Name(agent_name.to_owned()),
        &paths,
    )
    .map_err(|error| reject(&profile_error_message(&error)))?;
    if let Err(message) = ensure_global_profile_protected(
        &paths.global_agents_dir,
        &paths.project_agents_dir(),
        &loaded,
    ) {
        return Err(reject(&message));
    }
    if loaded.profile.agent.name != agent_name {
        return Err(reject(&format!(
            "[agent].name at {} declares '{}'; must match selected agent '{agent_name}'",
            loaded.path.display(),
            loaded.profile.agent.name,
        )));
    }
    if loaded.profile.agent.model.len()
        > lattice_conversation::MAX_PROVIDER_ID_BYTES + 1 + lattice_llm::MAX_MODEL_ID_BYTES
    {
        return Err(reject(&format!(
            "invalid or oversized profile at {}",
            loaded.path.display()
        )));
    }
    let policy =
        crate::turn::cep::load_policy_config(&loaded.profile.lancet, workspace.canonical_path())?;
    let lancet = &mut loaded.profile.lancet;
    // 将准入时的有效策略填回具体字段，执行时不再读取策略文件，保持这一轮的值不变。
    lancet.policy = None;
    lancet.project_root = Some(LancetProjectRootSection {
        root: Some(
            policy
                .project_root
                .to_str()
                .ok_or_else(invalid_profile)?
                .into(),
        ),
    });
    lancet.network = Some(LancetNetworkSection {
        allow_public_https: Some(policy.network.allow_public_https),
        allow_localhost: Some(policy.network.allow_localhost),
        allowed_hosts: policy.network.allowed_hosts,
        max_web_response_bytes: Some(policy.network.max_web_response_bytes),
    });
    lancet.workspace = Some(LancetWorkspacePolicySection {
        denied_write_patterns: policy.workspace.denied_write_patterns,
        included_ignored_read_patterns: policy.workspace.included_ignored_read_patterns,
        denied_read_patterns: policy.workspace.denied_read_patterns,
    });
    lancet.observation = Some(LancetObservationSection {
        max_single_read_bytes: Some(policy.observation.max_single_read_bytes),
        max_turn_observation_bytes: Some(policy.observation.max_turn_observation_bytes),
        max_grep_matches: Some(policy.observation.max_grep_matches),
        max_list_entries: Some(policy.observation.max_list_entries),
        max_find_paths: Some(policy.observation.max_find_paths),
    });
    Ok(loaded)
}

fn effective_uid() -> u32 {
    // Linux geteuid 无参数、无内存访问，与本地 IPC 的对端检查相同。
    unsafe { libc::geteuid() }
}

/// 全局 agents 目录与其中的 profile 文件只能由当前用户改写。
/// profile 不是秘密，group/other 可读（默认 umask 022 的 0755/0644）照常接受；
/// 风险是被他人改写，所以只拒绝 group/other 写位。auth.json 的 0600/0700 规则另行维护。
/// 项目内 profile 由仓库权限管理，不在这里拒绝。
fn ensure_global_profile_protected(
    global_agents_dir: &Path,
    project_agents_dir: &Path,
    loaded: &LoadedAgentProfile,
) -> std::result::Result<(), String> {
    // 项目 profile 优先，并且测试里配置目录可能就放在项目的 .lattice 下。
    // 只约束用户全局目录，不把仓库里的 agent.toml 当成全局配置。
    if !loaded.path.starts_with(global_agents_dir) || loaded.path.starts_with(project_agents_dir) {
        return Ok(());
    }
    let owner = effective_uid();
    check_protected(global_agents_dir, true, owner)?;
    check_protected(&loaded.package_dir, true, owner)?;
    check_protected(&loaded.path, false, owner)?;
    if let Some(relative) = loaded.profile.prompt.system_path.as_deref() {
        let prompt = loaded.package_dir.join(relative);
        if prompt.starts_with(global_agents_dir) {
            check_protected(&prompt, false, owner)?;
        }
    }
    Ok(())
}

/// group 与 other 的写位。读与执行位不拒绝。
const FOREIGN_WRITE_BITS: u32 = 0o022;

fn check_protected(path: &Path, directory: bool, owner: u32) -> std::result::Result<(), String> {
    let label = if directory {
        "global agents directory"
    } else {
        "profile file"
    };
    // symlink_metadata 不跟随最后一级符号链接，避免把链接目标当成配置本身。
    let meta = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "{}: cannot inspect {label} ({})",
            path.display(),
            error.kind()
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{}: {label} must not be a symbolic link; permissions were not changed",
            path.display()
        ));
    }
    if meta.uid() != owner {
        return Err(format!(
            "{}: {label} must belong to the current user",
            path.display()
        ));
    }
    if directory && !meta.is_dir() {
        return Err(format!("{}: {label} must be a directory", path.display()));
    }
    if !directory && !meta.is_file() {
        return Err(format!(
            "{}: {label} must be a regular file",
            path.display()
        ));
    }
    if meta.mode() & FOREIGN_WRITE_BITS != 0 {
        return Err(format!(
            "{}: {label} must not be writable by group or other; permissions were not changed",
            path.display()
        ));
    }
    Ok(())
}

fn invalid_profile() -> ApiError {
    ApiError::BadRequest("invalid or oversized frozen configuration".into())
}

// TOML 的 Display 会附上多行源码；保留解析器说明及位置，省去源码摘录。
pub(crate) fn profile_error_message(error: &lattice_profile::ProfileError) -> String {
    match error {
        lattice_profile::ProfileError::Toml { path, source, line } => {
            let location = line
                .map(|line| format!("line {line}: "))
                .unwrap_or_default();
            format!(
                "TOML parse error at {}: {location}{}",
                path.display(),
                source.message()
            )
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn loaded_prompt_and_effective_policy_survive_source_removal() {
        let root = tempfile::tempdir().unwrap();
        let configuration_dir = root.path().join("home/.lattice");
        std::fs::create_dir_all(&configuration_dir).unwrap();
        std::fs::set_permissions(&configuration_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let project = root.path().join("project");
        let package = project.join(".lattice/agents/test");
        std::fs::create_dir_all(&package).unwrap();
        let profile = package.join("agent.toml");
        let prompt = "冻结的提示".to_owned();
        std::fs::write(package.join("prompt.md"), &prompt).unwrap();
        std::fs::write(&profile, "schema_version=1\n[agent]\nname='test'\nmodel='local/test'\n[prompt]\nsystem_path='prompt.md'\n[lancet.policy]\npath='policy.toml'\n").unwrap();
        let policy_path = project.join("policy.toml");
        std::fs::write(
            &policy_path,
            "[network]\nallow_public_https=true\nallow_localhost=true\nallowed_hosts=['localhost']\nmax_web_response_bytes=234\n\
             [workspace]\ndenied_write_patterns=['locked/**']\n\
             included_ignored_read_patterns=['generated/**']\ndenied_read_patterns=['private/**']\n\
             [observation]\nmax_single_read_bytes=123\nmax_turn_observation_bytes=456\n\
             max_grep_matches=7\nmax_list_entries=8\nmax_find_paths=9\n",
        )
        .unwrap();
        let workspace = crate::LocalWorkspaceRoot::from_path(&project).unwrap();
        let loaded = load_for_turn("test", &workspace, &configuration_dir).unwrap();
        std::fs::remove_file(policy_path).unwrap();
        std::fs::remove_dir_all(package).unwrap();
        let lancet = &loaded.profile.lancet;
        assert_eq!(loaded.system_prompt, prompt);
        assert_eq!(
            lancet.project_root,
            Some(LancetProjectRootSection {
                root: Some(workspace.canonical_path().to_str().unwrap().into()),
            })
        );
        assert_eq!(
            lancet.network,
            Some(LancetNetworkSection {
                allow_public_https: Some(true),
                allow_localhost: Some(true),
                allowed_hosts: vec!["localhost".into()],
                max_web_response_bytes: Some(234),
            })
        );
        assert_eq!(
            lancet.workspace,
            Some(LancetWorkspacePolicySection {
                denied_write_patterns: vec!["locked/**".into()],
                included_ignored_read_patterns: vec!["generated/**".into()],
                denied_read_patterns: vec!["private/**".into()],
            })
        );
        assert_eq!(
            lancet.observation,
            Some(LancetObservationSection {
                max_single_read_bytes: Some(123),
                max_turn_observation_bytes: Some(456),
                max_grep_matches: Some(7),
                max_list_entries: Some(8),
                max_find_paths: Some(9),
            })
        );
        assert!(lancet.policy.is_none());
    }

    fn private_configuration(
        root: &std::path::Path,
    ) -> (std::path::PathBuf, crate::LocalWorkspaceRoot) {
        let configuration_dir = root.join("home/.lattice");
        std::fs::create_dir_all(&configuration_dir).unwrap();
        std::fs::set_permissions(&configuration_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let workspace = crate::LocalWorkspaceRoot::from_path(&project).unwrap();
        (configuration_dir, workspace)
    }

    /// 写一个从 prompt.md 读取系统提示的全局 agent。包目录用 `dir_mode`，
    /// agent.toml 与 prompt.md 分别用各自的 mode。
    fn write_global_agent(
        agents: &std::path::Path,
        dir_mode: u32,
        file_mode: u32,
        prompt_mode: u32,
    ) {
        let package = agents.join("test");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(dir_mode)).unwrap();
        let prompt = package.join("prompt.md");
        std::fs::write(&prompt, "hi").unwrap();
        std::fs::set_permissions(&prompt, std::fs::Permissions::from_mode(prompt_mode)).unwrap();
        let profile = package.join("agent.toml");
        std::fs::write(
            &profile,
            "schema_version=1\n[agent]\nname='test'\nmodel='local/test'\n[prompt]\nsystem_path='prompt.md'\n",
        )
        .unwrap();
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(file_mode)).unwrap();
    }

    fn global_agents_dir(configuration_dir: &std::path::Path, mode: u32) -> std::path::PathBuf {
        let agents = configuration_dir.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(mode)).unwrap();
        agents
    }

    // 这些测试只比较 symlink_metadata 的 uid 与 mode 位，不依赖 access(2)，
    // 因此以 root 运行时结论相同：文件属于 euid 0，mode 就是 set_permissions 设的值。
    // 不同属主由 global_profile_rejects_a_different_owner 以 euid+1 直接构造。

    #[test]
    fn global_profile_with_default_umask_permissions_loads() {
        let root = tempfile::tempdir().unwrap();
        let (configuration_dir, workspace) = private_configuration(root.path());
        let agents = global_agents_dir(&configuration_dir, 0o755);
        write_global_agent(&agents, 0o755, 0o644, 0o644);
        let result = load_for_turn("test", &workspace, &configuration_dir);
        assert!(
            result.is_ok(),
            "umask 022 layout (dirs 0755, files 0644) must load, got {:?}",
            result.err().map(|error| error.to_string())
        );
    }

    #[test]
    fn global_agents_directories_reject_group_or_other_write() {
        for (agents_mode, package_mode, rejected) in [
            (0o775, 0o755, "/agents: "),
            (0o757, 0o755, "/agents: "),
            (0o755, 0o775, "/agents/test: "),
            (0o755, 0o757, "/agents/test: "),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (configuration_dir, workspace) = private_configuration(root.path());
            let agents = global_agents_dir(&configuration_dir, agents_mode);
            write_global_agent(&agents, package_mode, 0o644, 0o644);
            let message = load_for_turn("test", &workspace, &configuration_dir)
                .unwrap_err()
                .to_string();
            assert!(
                message.contains("global agents directory must not be writable by group or other")
                    && message.contains(rejected),
                "agents {agents_mode:o} / package {package_mode:o} must reject '{rejected}', got {message}"
            );
        }
    }

    #[test]
    fn global_profile_files_reject_group_or_other_write() {
        for (file_mode, prompt_mode, rejected) in [
            (0o664, 0o644, "/agent.toml: "),
            (0o646, 0o644, "/agent.toml: "),
            (0o644, 0o664, "/prompt.md: "),
            (0o644, 0o646, "/prompt.md: "),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (configuration_dir, workspace) = private_configuration(root.path());
            let agents = global_agents_dir(&configuration_dir, 0o755);
            write_global_agent(&agents, 0o755, file_mode, prompt_mode);
            let message = load_for_turn("test", &workspace, &configuration_dir)
                .unwrap_err()
                .to_string();
            assert!(
                message.contains("profile file must not be writable by group or other")
                    && message.contains(rejected),
                "agent.toml {file_mode:o} / prompt.md {prompt_mode:o} must reject '{rejected}', got {message}"
            );
        }
    }

    #[test]
    fn global_agents_directory_rejects_a_symbolic_link() {
        let root = tempfile::tempdir().unwrap();
        let (configuration_dir, workspace) = private_configuration(root.path());
        let real = root.path().join("real-agents");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        write_global_agent(&real, 0o755, 0o644, 0o644);
        std::os::unix::fs::symlink(&real, configuration_dir.join("agents")).unwrap();
        let message = load_for_turn("test", &workspace, &configuration_dir)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("symbolic link"),
            "a symlinked agents directory must be rejected, got {message}"
        );
    }

    #[test]
    fn global_profile_file_rejects_a_symbolic_link() {
        let root = tempfile::tempdir().unwrap();
        let (configuration_dir, workspace) = private_configuration(root.path());
        let agents = global_agents_dir(&configuration_dir, 0o755);
        write_global_agent(&agents, 0o755, 0o644, 0o644);
        let profile = agents.join("test/agent.toml");
        let real = agents.join("test/real.toml");
        std::fs::rename(&profile, &real).unwrap();
        std::os::unix::fs::symlink(&real, &profile).unwrap();
        let message = load_for_turn("test", &workspace, &configuration_dir)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("/agent.toml: profile file must not be a symbolic link"),
            "a symlinked agent.toml must be rejected, got {message}"
        );
    }

    #[test]
    fn project_agents_are_exempt_from_global_permission_rules() {
        // 项目 profile 由仓库权限管理：配置目录在项目外、或配置目录就是项目 .lattice
        // （两条路径重合）时，group/other 可写的项目 agent 都照常装载。
        let root = tempfile::tempdir().unwrap();
        let (outside_configuration, workspace) = private_configuration(root.path());
        let project_agents = workspace.canonical_path().join(".lattice/agents");
        std::fs::create_dir_all(&project_agents).unwrap();
        std::fs::set_permissions(&project_agents, std::fs::Permissions::from_mode(0o777)).unwrap();
        write_global_agent(&project_agents, 0o777, 0o666, 0o666);
        for (case, configuration_dir) in [
            ("configuration outside project", outside_configuration),
            (
                "configuration is project .lattice",
                workspace.canonical_path().join(".lattice"),
            ),
        ] {
            let result = load_for_turn("test", &workspace, &configuration_dir);
            assert!(
                result.is_ok(),
                "{case}: writable project agent must load, got {:?}",
                result.err().map(|error| error.to_string())
            );
        }
    }

    // 非 root 无法创建属于他人的文件，load_for_turn 走不到属主分支；
    // 这里直接以 euid+1 调用同一检查，root 与非 root 结论一致。
    #[test]
    fn global_profile_rejects_a_different_owner() {
        let root = tempfile::tempdir().unwrap();
        let (configuration_dir, _workspace) = private_configuration(root.path());
        let agents = configuration_dir.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o700)).unwrap();
        let message = check_protected(&agents, true, effective_uid().wrapping_add(1)).unwrap_err();
        assert!(
            message.contains("must belong to the current user"),
            "a different owner must be rejected, got {message}"
        );
    }
}
