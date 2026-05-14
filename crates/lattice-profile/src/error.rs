use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("TOML parse error at {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("invalid schema_version {found} at {path}: only schema_version = 1 is supported")]
    UnsupportedSchemaVersion { path: PathBuf, found: u64 },

    #[error("forbidden top-level section [{section}] at {path}: {reason}. Allowed sections: schema_version, [agent], [prompt], [tools], [security], [context], [behavior]")]
    ForbiddenSection {
        path: PathBuf,
        section: String,
        reason: String,
    },

    #[error("forbidden field `{field}` in [{section}] at {path}: {reason}")]
    ForbiddenField {
        path: PathBuf,
        section: String,
        field: String,
        reason: String,
    },

    #[error(
        "invalid value `{value}` for `{field}` in [{section}] at {path}: {reason}. {expected}"
    )]
    InvalidField {
        path: PathBuf,
        section: String,
        field: String,
        value: String,
        reason: String,
        expected: String,
    },

    #[error("both `system` and `system_path` set in [prompt] at {path}: exactly one is required")]
    PromptMutuallyExclusive { path: PathBuf },

    #[error(
        "neither `system` nor `system_path` set in [prompt] at {path}: exactly one is required"
    )]
    MissingPrompt { path: PathBuf },

    #[error(
        "prompt file path `{given}` at {profile_path} escapes package directory: resolved to {resolved} which is outside {package_dir}"
    )]
    PathEscape {
        profile_path: PathBuf,
        package_dir: PathBuf,
        given: String,
        resolved: PathBuf,
    },

    #[error("prompt.system_path `{given}` at {profile_path} is an absolute path — must be relative to the agent.toml package directory")]
    AbsoluteSystemPath {
        profile_path: PathBuf,
        given: String,
    },

    #[error(
        "prompt.system_path `{given}` at {profile_path} contains '..' — traversal is not allowed"
    )]
    SystemPathTraversal {
        profile_path: PathBuf,
        given: String,
    },

    #[error("prompt.system_path `{given}` ({resolved}) at {profile_path} is not a file")]
    SystemPathNotAFile {
        profile_path: PathBuf,
        given: String,
        resolved: PathBuf,
    },

    #[error("prompt.system_path `{given}` at {profile_path}: file not found ({source})")]
    SystemPathNotFound {
        profile_path: PathBuf,
        given: String,
        #[source]
        source: std::io::Error,
    },

    #[error("agent profile '{name}' not found at project path {project_path} or global path {global_path}")]
    ProfileNotFound {
        name: String,
        project_path: PathBuf,
        global_path: PathBuf,
    },

    #[error("no default agent profile found: create at {dir} with `lattice new agent default`, or specify an agent with `--agent <name>`")]
    MissingDefault { dir: PathBuf, template: String },
}
