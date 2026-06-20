use async_trait::async_trait;

use super::{Connector, EgressCtx};

pub struct ChatConnector;

#[async_trait]
impl Connector for ChatConnector {
    async fn run(
        &self,
        _req: codex_api::ResponsesApiRequest,
        _ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
        // 实体逻辑见 Task 04（请求）+ Task 05（SSE）+ Task 08（接线）。
        Err(codex_api::ApiError::InvalidRequest {
            message: "chat connector not implemented yet".into(),
        })
    }
}
