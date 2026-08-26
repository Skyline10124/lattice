// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::handle::handle_request;
use super::transport_config::ServerRunConfig;
use crate::server::registry::{HandlerContext, MethodRegistry};
use crate::workspace::LocalWorkspace;
use lancet_dispatch::{CepServer, ServerCall, ServerRuntime};
use lancet_protocol::CepError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone)]
struct NativeServerRuntime {
    config: ServerRunConfig,
    workspace: Arc<LocalWorkspace>,
}

impl ServerRuntime<HandlerContext> for NativeServerRuntime {
    fn handle_call(
        &self,
        registry: MethodRegistry,
        call: ServerCall,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let config = self.config.clone();
        let workspace = Arc::clone(&self.workspace);
        Box::pin(async move {
            handle_request(registry, call, config, workspace).await;
        })
    }
}

/// Convenience extension for running a dispatch server with the native CEP lifecycle.
pub trait CepServerNativeExt {
    fn run_native(
        self,
        registry: MethodRegistry,
        config: ServerRunConfig,
    ) -> Pin<Box<dyn Future<Output = Result<(), CepError>> + Send>>;
}

impl CepServerNativeExt for CepServer {
    fn run_native(
        self,
        registry: MethodRegistry,
        config: ServerRunConfig,
    ) -> Pin<Box<dyn Future<Output = Result<(), CepError>> + Send>> {
        Box::pin(async move {
            config.validate_capability_admission()?;
            config.validate_no_component_native_collision()?;
            let workspace = Arc::new(LocalWorkspace::with_history(
                &config.cep_context.policy.project_root,
                Arc::clone(&config.workspace_history),
            ));
            self.run(registry, NativeServerRuntime { config, workspace })
                .await
        })
    }
}
