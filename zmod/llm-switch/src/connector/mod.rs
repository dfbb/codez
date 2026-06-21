pub(crate) mod anthropic;
pub(crate) mod anthropic_sse;
mod anthropic_req;
pub(crate) mod chat;
pub(crate) mod chat_sse;
mod chat_req;

use async_trait::async_trait;
use thiserror::Error;

use crate::config::{AuthKind, Connector as ConnectorKind};

/// Connector-internal error.
/// - `HardFail`: unsupported in v1 / cannot be expressed structurally (§4.0 etc.), maps to `ApiError::InvalidRequest`.
/// - `Http`: already a codex `ApiError` (connection / status code / stream error).
#[derive(Debug, Error)]
pub enum ConnError {
    #[error("llm-switch unsupported: {0}")]
    HardFail(String),
    #[error(transparent)]
    Http(#[from] codex_api::ApiError),
}

impl From<ConnError> for codex_api::ApiError {
    fn from(e: ConnError) -> Self {
        match e {
            ConnError::Http(api) => api,
            ConnError::HardFail(msg) => codex_api::ApiError::InvalidRequest { message: msg },
        }
    }
}

/// Egress context: assembled by Task 08's `run()` from `Route` + `resolve_key`.
pub struct EgressCtx {
    pub base_url: String,
    pub model: String,
    pub auth: AuthKind,
    pub key: Option<String>,
    pub anthropic_version: Option<String>,
    pub path_override: Option<String>,
    pub default_max_tokens: Option<u32>,
    pub http: reqwest::Client,
    /// Bearer key fallback (§5.3 item 3): borrowed when key is None and auth is Bearer.
    pub auth_fallback: Option<codex_api::SharedAuthProvider>,
}

/// SSE state-machine abstraction: implemented once each by chat and anthropic, driven by `run_egress`.
/// `pub` so the `testing` module can expose this trait in integration tests (the type is opaque, only passed as a Box).
pub trait SseTranslator: Send {
    fn push(&mut self, data: &serde_json::Value) -> Result<Vec<codex_api::ResponseEvent>, ConnError>;
    fn finish(&mut self) -> Vec<codex_api::ResponseEvent>;
}

/// Re-export `run_egress`, for direct use by the chat/anthropic connectors.
pub(crate) use crate::sse::run_egress;

#[async_trait]
pub trait Connector: Send + Sync {
    /// Complete HTTP + status-code check + SSE setup synchronously before spawning (§4.7);
    /// returns a stream of the same shape as `stream_request`.
    async fn run(
        &self,
        req: codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError>;
}

/// Factory: select the concrete connector implementation by the `config::Connector` enum.
pub fn make_connector(kind: ConnectorKind) -> Box<dyn Connector> {
    match kind {
        ConnectorKind::Chat => Box::new(chat::ChatConnector),
        ConnectorKind::Anthropic => Box::new(anthropic::AnthropicConnector),
    }
}

/// Assemble egress headers. When key exists, use `build_headers`; when key is None and auth is Bearer, use
/// `auth_fallback` (§5.3); otherwise return an `InvalidRequest` error.
pub(crate) fn egress_headers(
    ctx: &EgressCtx,
    anthropic_version: Option<&str>,
) -> Result<reqwest::header::HeaderMap, codex_api::ApiError> {
    if let Some(ref key) = ctx.key {
        return crate::http::build_headers(ctx.auth, Some(key.as_str()), anthropic_version)
            .map_err(|e| codex_api::ApiError::InvalidRequest { message: e.to_string() });
    }
    // No raw key: only Bearer can borrow codex auth (§5.3)
    match (ctx.auth, &ctx.auth_fallback) {
        (AuthKind::Bearer, Some(provider)) => {
            let mut h = reqwest::header::HeaderMap::new();
            provider.add_auth_headers(&mut h);
            h.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
            Ok(h)
        }
        _ => Err(codex_api::ApiError::InvalidRequest {
            message: "missing API key (set key_env or auth_key)".into(),
        }),
    }
}

/// Return the `ResponseItem` variant name string (for use in HardFail error messages).
pub(crate) fn variant_name(item: &codex_protocol::models::ResponseItem) -> &'static str {
    use codex_protocol::models::ResponseItem::*;
    match item {
        Message { .. } => "Message",
        AgentMessage { .. } => "AgentMessage",
        Reasoning { .. } => "Reasoning",
        LocalShellCall { .. } => "LocalShellCall",
        FunctionCall { .. } => "FunctionCall",
        ToolSearchCall { .. } => "ToolSearchCall",
        FunctionCallOutput { .. } => "FunctionCallOutput",
        CustomToolCall { .. } => "CustomToolCall",
        CustomToolCallOutput { .. } => "CustomToolCallOutput",
        ToolSearchOutput { .. } => "ToolSearchOutput",
        WebSearchCall { .. } => "WebSearchCall",
        ImageGenerationCall { .. } => "ImageGenerationCall",
        Compaction { .. } => "Compaction",
        CompactionTrigger { .. } => "CompactionTrigger",
        ContextCompaction { .. } => "ContextCompaction",
        Other => "Other",
    }
}
