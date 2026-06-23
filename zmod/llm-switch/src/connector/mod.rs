pub(crate) mod anthropic;
pub(crate) mod anthropic_sse;
mod anthropic_req;
pub(crate) mod chat;
pub(crate) mod chat_sse;
mod chat_req;

use async_trait::async_trait;
use thiserror::Error;

use crate::config::{AuthKind, Connector as ConnectorKind};

/// Connector internal errors.
/// - `HardFail`: unsupported in v1 / structure cannot express (§4.0, etc.), mapped to `ApiError::InvalidRequest`.
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
    /// Opt-in: emit top-level `cache_control` for Anthropic (off by default).
    pub prompt_cache: bool,
    pub http: reqwest::Client,
    /// Bearer key fallback (§5.3 item 3): borrowed when key is None and auth is Bearer.
    pub auth_fallback: Option<codex_api::SharedAuthProvider>,
}

/// SSE state machine abstraction: chat and anthropic each implement once, driven by `run_egress`.
/// `pub` is to allow the `testing` module to expose this trait in integration tests (opaque type, only pass Box).
pub trait SseTranslator: Send {
    fn push(&mut self, data: &serde_json::Value) -> Result<Vec<codex_api::ResponseEvent>, ConnError>;
    fn finish(&mut self) -> Vec<codex_api::ResponseEvent>;
}

/// Re-export `run_egress` for direct use by chat/anthropic connector.
pub(crate) use crate::sse::run_egress;

#[async_trait]
pub trait Connector: Send + Sync {
    /// Spawned only after sync completion of HTTP + status code validation + SSE establishment (§4.7);
    /// returns a stream of the same type as `stream_request`.
    async fn run(
        &self,
        req: codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError>;
}

/// Factory: select concrete connector implementation according to `config::Connector` enum.
pub fn make_connector(kind: ConnectorKind) -> Box<dyn Connector> {
    match kind {
        ConnectorKind::Chat => Box::new(chat::ChatConnector),
        ConnectorKind::Anthropic => Box::new(anthropic::AnthropicConnector),
    }
}

/// Assemble egress headers. When key exists, use `build_headers`; when key is None and auth is Bearer,
/// use `auth_fallback` (§5.3); otherwise return `InvalidRequest` error.
pub(crate) fn egress_headers(
    ctx: &EgressCtx,
    anthropic_version: Option<&str>,
) -> Result<reqwest::header::HeaderMap, codex_api::ApiError> {
    if let Some(ref key) = ctx.key {
        return crate::http::build_headers(ctx.auth, Some(key.as_str()), anthropic_version)
            .map_err(|e| codex_api::ApiError::InvalidRequest { message: e.to_string() });
    }
    // No original key: only Bearer can borrow codex auth (§5.3)
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
