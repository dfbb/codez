pub(crate) use super::anthropic_req::build_anthropic_request;

use async_trait::async_trait;

use super::{Connector, EgressCtx};

pub struct AnthropicConnector;

#[async_trait]
impl Connector for AnthropicConnector {
    async fn run(
        &self,
        _req: codex_api::ResponsesApiRequest,
        _ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
        // 实体逻辑见 Task 06（请求）+ Task 07（SSE）+ Task 08（接线）。
        Err(codex_api::ApiError::InvalidRequest {
            message: "anthropic connector not implemented yet".into(),
        })
    }
}
