pub(crate) mod anthropic;
pub(crate) mod anthropic_sse;
mod anthropic_req;
pub(crate) mod chat;
pub(crate) mod chat_sse;
mod chat_req;

use async_trait::async_trait;
use thiserror::Error;

use crate::config::{AuthKind, Connector as ConnectorKind};

/// 连接器内部错误。
/// - `HardFail`：v1 不支持/结构无法表达（§4.0 等），映射成 `ApiError::InvalidRequest`。
/// - `Http`：已是 codex `ApiError`（建连/状态码/流错误）。
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

/// 出口上下文：由 Task 08 的 `run()` 从 `Route` + `resolve_key` 组装。
pub struct EgressCtx {
    pub base_url: String,
    pub model: String,
    pub auth: AuthKind,
    pub key: Option<String>,
    pub anthropic_version: Option<String>,
    pub path_override: Option<String>,
    pub default_max_tokens: Option<u32>,
    pub http: reqwest::Client,
    /// bearer 密钥退路（§5.3 item 3）：key 为 None 且 auth 为 Bearer 时借用。
    pub auth_fallback: Option<codex_api::SharedAuthProvider>,
}

/// SSE 状态机抽象：chat 与 anthropic 各实现一次，由 `run_egress` 驱动。
/// `pub` 是为了让 `testing` 模块可以在集成测试中暴露此 trait（类型不透明，仅传 Box）。
pub trait SseTranslator: Send {
    fn push(&mut self, data: &serde_json::Value) -> Result<Vec<codex_api::ResponseEvent>, ConnError>;
    fn finish(&mut self) -> Vec<codex_api::ResponseEvent>;
}

/// 重导出 `run_egress`，供 chat/anthropic connector 直接用。
pub(crate) use crate::sse::run_egress;

#[async_trait]
pub trait Connector: Send + Sync {
    /// 同步完成 HTTP+状态码校验+SSE 建立后才 spawn（§4.7）；
    /// 返回与 `stream_request` 同型的流。
    async fn run(
        &self,
        req: codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError>;
}

/// 工厂：按 `config::Connector` 枚举选择具体连接器实现。
pub fn make_connector(kind: ConnectorKind) -> Box<dyn Connector> {
    match kind {
        ConnectorKind::Chat => Box::new(chat::ChatConnector),
        ConnectorKind::Anthropic => Box::new(anthropic::AnthropicConnector),
    }
}

/// 组装出口头。key 存在时走 `build_headers`；key 为 None 且 auth 为 Bearer 时走
/// `auth_fallback`（§5.3）；其余情况返回 `InvalidRequest` 错误。
pub(crate) fn egress_headers(
    ctx: &EgressCtx,
    anthropic_version: Option<&str>,
) -> Result<reqwest::header::HeaderMap, codex_api::ApiError> {
    if let Some(ref key) = ctx.key {
        return crate::http::build_headers(ctx.auth, Some(key.as_str()), anthropic_version)
            .map_err(|e| codex_api::ApiError::InvalidRequest { message: e.to_string() });
    }
    // 无原始 key：仅 Bearer 可借 codex auth（§5.3）
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

/// 返回 `ResponseItem` 变体名字符串（供 HardFail 错误信息使用）。
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
