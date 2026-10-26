// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

/// 每个 Turn 持有提交时冻结的连接，不在模型重试或下一步骤重新读取环境。
pub(crate) struct RealLlmClient(pub std::sync::Arc<lattice_llm::LlmConnection>);

#[async_trait::async_trait]
impl lattice_agent::LlmClient for RealLlmClient {
    async fn stream_chat(
        &self,
        request: lattice_llm::ChatRequest,
        control: lattice_llm::StreamControl,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        self.0
            .stream(request, control)
            .await
            .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })
    }
}
