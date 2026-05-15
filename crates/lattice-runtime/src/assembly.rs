use std::path::PathBuf;
use std::str::FromStr;

use lattice_agent::{Agent, TaskState};
use lattice_core::catalog::ResolvedModel;
use lattice_core::types::{Message, ToolDefinition};
use lattice_profile::GenericAgentSpec;
use lattice_tools::{resolve_capabilities, CapabilitySelection, DefaultToolExecutor, ToolBundle};

use crate::{Runtime, RuntimeError, RuntimeSecurity};

#[derive(Clone)]
pub struct RuntimeInvocationContext {
    pub cwd: PathBuf,
    pub project_root: PathBuf,
    pub provider_override: Option<String>,
    pub security: RuntimeSecurity,
    pub task_state_seed: Option<TaskState>,
}

pub struct GenericAgentAssembly {
    pub agent: Agent,
    pub model: ResolvedModel,
    pub context_length: u32,
    pub tool_definitions: Vec<ToolDefinition>,
}

impl Runtime {
    pub fn assemble_generic_agent(
        &self,
        spec: &GenericAgentSpec,
        invocation: &RuntimeInvocationContext,
        history: Vec<Message>,
    ) -> Result<GenericAgentAssembly, RuntimeError> {
        let resolved =
            self.resolve_with_provider(&spec.model, invocation.provider_override.as_deref())?;
        let selection = capability_selection_from_spec(spec)?;
        if selection.bundles.is_empty() && selection.enabled.is_empty() {
            return Err(RuntimeError::Config(
                "GenericAgentSpec must declare tool bundles or enabled tools".into(),
            ));
        }

        let capabilities = resolve_capabilities(&selection);
        if capabilities.tool_names.is_empty() {
            return Err(RuntimeError::Config(
                "GenericAgentSpec resolved to no executable tools".into(),
            ));
        }

        let mut agent = Agent::new(resolved.clone()).with_tools(capabilities.definitions.clone());
        agent.set_system_prompt(&spec.system_prompt);

        if spec.context.project_instructions {
            agent = agent.with_provider(lattice_prompt::ProjectInstructionsProvider::new(
                invocation.project_root.clone(),
                invocation.cwd.clone(),
            ));
        }
        if spec.context.workspace_context {
            agent = agent.with_provider(lattice_prompt::WorkspaceContextProvider::new(
                invocation.project_root.clone(),
                invocation.cwd.clone(),
            ));
        }
        if !history.is_empty() {
            agent.seed_messages(history);
        }
        if spec.behavior.reasoning_effort.is_some() {
            agent = agent.with_thinking_effort(spec.behavior.reasoning_effort.clone());
        }
        if let Some(seed) = &invocation.task_state_seed {
            *agent.task_state_mut() = seed.clone();
        }
        agent = agent.with_task_state_provider();

        let project_root = invocation.project_root.to_string_lossy().to_string();
        let executor = DefaultToolExecutor::new(&project_root)
            .map_err(RuntimeError::Config)?
            .with_sandbox(invocation.security.sandbox.clone())
            .with_authorized_tools(capabilities.tool_names.clone());
        agent = agent.with_tool_executor(Box::new(executor));

        let context_length = if resolved.context_length > 0 {
            resolved.context_length
        } else {
            128000
        };

        Ok(GenericAgentAssembly {
            agent,
            model: resolved,
            context_length,
            tool_definitions: capabilities.definitions,
        })
    }
}

fn capability_selection_from_spec(
    spec: &GenericAgentSpec,
) -> Result<CapabilitySelection, RuntimeError> {
    let bundles = spec
        .tools
        .bundles
        .iter()
        .map(|bundle| {
            ToolBundle::from_str(bundle).map_err(|err| {
                RuntimeError::Config(format!("unknown tool bundle in GenericAgentSpec: {err}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CapabilitySelection {
        bundles,
        enabled: spec.tools.enabled.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry};
    use lattice_core::types::Role;
    use lattice_profile::{BehaviorSpec, ContextSpec, SecuritySpec, ToolSelectionSpec};
    use std::collections::HashMap;

    fn runtime_with_model(name: &str) -> Runtime {
        let mut runtime = Runtime::builder().build();
        runtime
            .register_model(ModelCatalogEntry {
                canonical_id: name.to_string(),
                context_length: 8192,
                providers: vec![CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: name.to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost:11434".to_string()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: Vec::new(),
            })
            .unwrap();
        runtime
    }

    fn spec_with_tools(bundles: Vec<&str>, enabled: Vec<&str>) -> GenericAgentSpec {
        GenericAgentSpec {
            profile_name: "default".to_string(),
            model: "generic-agent-test-model".to_string(),
            description: None,
            system_prompt: "You are a test agent.".to_string(),
            tools: ToolSelectionSpec {
                bundles: bundles.into_iter().map(str::to_string).collect(),
                enabled: enabled.into_iter().map(str::to_string).collect(),
            },
            security: SecuritySpec {
                sandbox_mode: "project".to_string(),
            },
            context: ContextSpec {
                project_instructions: true,
                workspace_context: true,
            },
            behavior: BehaviorSpec {
                max_turns: Some(4),
                reasoning_effort: Some("medium".to_string()),
            },
        }
    }

    fn invocation() -> RuntimeInvocationContext {
        let cwd = std::env::current_dir().unwrap();
        let security =
            crate::build_runtime_security(&crate::RuntimeSecurityConfig::default(), &cwd).unwrap();
        RuntimeInvocationContext {
            cwd: cwd.clone(),
            project_root: cwd,
            provider_override: None,
            security,
            task_state_seed: None,
        }
    }

    #[test]
    fn generic_agent_assembly_uses_generic_spec() {
        let runtime = runtime_with_model("generic-agent-test-model");
        let assembly = runtime
            .assemble_generic_agent(
                &spec_with_tools(vec!["filesystem"], vec!["workspace_context"]),
                &invocation(),
                vec![Message::new(
                    Role::User,
                    "hello".to_string(),
                    None,
                    None,
                    None,
                )],
            )
            .unwrap();

        let names = assembly
            .tool_definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"workspace_context"));
        assert_eq!(assembly.model.canonical_id, "generic-agent-test-model");
        assert_eq!(assembly.context_length, 8192);
        assert_eq!(assembly.agent.messages().len(), 1);
    }

    #[test]
    fn generic_agent_assembly_rejects_empty_capabilities() {
        let runtime = runtime_with_model("generic-agent-test-model");
        let err = match runtime.assemble_generic_agent(
            &spec_with_tools(Vec::new(), Vec::new()),
            &invocation(),
            vec![],
        ) {
            Ok(_) => panic!("empty tools should fail"),
            Err(err) => err,
        };
        assert!(
            matches!(err, RuntimeError::Config(message) if message.contains("tool bundles") || message.contains("executable tools"))
        );
    }

    #[test]
    fn generic_agent_assembly_binds_sandbox_executor() {
        let runtime = runtime_with_model("generic-agent-test-model");
        let assembly = runtime
            .assemble_generic_agent(
                &spec_with_tools(vec!["shell"], Vec::new()),
                &invocation(),
                vec![],
            )
            .unwrap();
        let names = assembly
            .tool_definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["bash"]);
    }

    #[test]
    fn generic_agent_assembly_stays_runtime_owned() {
        let runtime = runtime_with_model("generic-agent-test-model");
        let assembly = runtime
            .assemble_generic_agent(
                &spec_with_tools(vec!["workspace"], Vec::new()),
                &invocation(),
                vec![],
            )
            .unwrap();

        assert_eq!(assembly.model.canonical_id, "generic-agent-test-model");
        assert!(assembly
            .tool_definitions
            .iter()
            .any(|definition| definition.name == "workspace_context"));
    }
}
