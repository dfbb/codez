pub(crate) use super::anthropic_req::build_anthropic_request;

use async_trait::async_trait;

use super::{Connector, EgressCtx};

pub struct AnthropicConnector;

#[async_trait]
impl Connector for AnthropicConnector {
    async fn run(
        &self,
        req: codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
        let body = build_anthropic_request(&req, ctx).map_err(codex_api::ApiError::from)?;
        let url = crate::http::egress_url(
            &ctx.base_url,
            crate::config::Connector::Anthropic,
            ctx.path_override.as_deref(),
        );
        let headers = super::egress_headers(ctx, ctx.anthropic_version.as_deref())?;
        let translator = Box::new(super::anthropic_sse::AnthropicSseState::default());
        super::run_egress(url, headers, body, ctx.http.clone(), translator).await
    }
}
