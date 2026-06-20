pub(crate) use super::chat_req::build_chat_request;

use async_trait::async_trait;

use super::{Connector, EgressCtx};

pub struct ChatConnector;

#[async_trait]
impl Connector for ChatConnector {
    async fn run(
        &self,
        req: codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
        let body = build_chat_request(&req, ctx).map_err(codex_api::ApiError::from)?;
        let url = crate::http::egress_url(
            &ctx.base_url,
            crate::config::Connector::Chat,
            ctx.path_override.as_deref(),
        );
        let headers = super::egress_headers(ctx, None)?;
        let translator = Box::new(super::chat_sse::ChatSseState::default());
        super::run_egress(url, headers, body, ctx.http.clone(), translator).await
    }
}
